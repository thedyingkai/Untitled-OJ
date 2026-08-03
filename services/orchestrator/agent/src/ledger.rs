use orchestrator_control_plane::{CompletionStatus, JobKind, NewJobEvent};
use orchestrator_runtime::ReleaseProviderRevision;
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde_json::Value;
use std::fs;
use std::path::Path;
use thiserror::Error;

const INTERRUPTED_MESSAGE: &str =
    "agent restarted while a runtime mutation may have been in flight";

#[derive(Debug, Error)]
pub enum LedgerError {
    #[error("failed to prepare ledger directory: {0}")]
    Io(#[from] std::io::Error),
    #[error("SQLite ledger error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("invalid JSON stored in the agent ledger: {0}")]
    Json(#[from] serde_json::Error),
    #[error("job {job_id} was previously recorded with a different payload or kind")]
    PayloadConflict { job_id: String },
    #[error("job {0} is not in RUNNING state")]
    NotRunning(String),
    #[error("unknown ledger state: {0}")]
    InvalidState(String),
    #[error("migration {service_name}@{version} conflicts with its durable checksum or OCI image")]
    MigrationConflict {
        service_name: String,
        version: String,
    },
    #[error("migration {service_name}@{version} has an ambiguous or failed prior outcome")]
    MigrationNeedsAttention {
        service_name: String,
        version: String,
    },
    #[error("provider revision saga for job {0} conflicts with its durable revision payload")]
    ProviderRevisionConflict(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LedgerRunState {
    Running,
    RetryableFailure,
    Succeeded,
    Failed,
    Cancelled,
    NeedsAttention,
}

impl LedgerRunState {
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Succeeded | Self::Failed | Self::Cancelled | Self::NeedsAttention
        )
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Running => "RUNNING",
            Self::RetryableFailure => "RETRYABLE_FAILURE",
            Self::Succeeded => "SUCCEEDED",
            Self::Failed => "FAILED",
            Self::Cancelled => "CANCELLED",
            Self::NeedsAttention => "NEEDS_ATTENTION",
        }
    }

    fn parse(value: &str) -> Result<Self, LedgerError> {
        match value {
            "RUNNING" => Ok(Self::Running),
            "RETRYABLE_FAILURE" => Ok(Self::RetryableFailure),
            "SUCCEEDED" => Ok(Self::Succeeded),
            "FAILED" => Ok(Self::Failed),
            "CANCELLED" => Ok(Self::Cancelled),
            "NEEDS_ATTENTION" => Ok(Self::NeedsAttention),
            other => Err(LedgerError::InvalidState(other.to_string())),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct StoredCompletion {
    pub status: CompletionStatus,
    pub result: Value,
    pub error_message: String,
    pub events: Vec<NewJobEvent>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum BeginDecision {
    Execute { attempt: u32 },
    Replay(StoredCompletion),
}

#[derive(Debug, Clone, PartialEq)]
pub struct JobRun {
    pub job_id: String,
    pub kind: JobKind,
    pub payload_sha256: String,
    pub state: LedgerRunState,
    pub attempt: u32,
    pub lease_token: String,
    pub completion: Option<StoredCompletion>,
    pub started_at_ms: i64,
    pub completed_at_ms: Option<i64>,
    pub updated_at_ms: i64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct JobStep {
    pub job_id: String,
    pub attempt: u32,
    pub step_index: u32,
    pub step_name: String,
    pub state: String,
    pub output: Option<Value>,
    pub error_message: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MigrationRun {
    pub service_name: String,
    pub version: String,
    pub checksum: String,
    pub image: String,
    pub state: String,
    pub job_id: String,
    pub container_id: Option<String>,
    pub error_message: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MigrationDecision {
    Execute,
    AlreadyApplied(MigrationRun),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderRevisionRun {
    pub job_id: String,
    pub previous: ReleaseProviderRevision,
    pub desired: ReleaseProviderRevision,
    pub state: String,
    pub applied_components: Vec<String>,
    pub error_message: Option<String>,
}

pub struct AgentLedger {
    connection: Connection,
}

impl AgentLedger {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, LedgerError> {
        let path = path.as_ref();
        if let Some(parent) = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            fs::create_dir_all(parent)?;
        }
        let connection = Connection::open(path)?;
        Self::from_connection(connection)
    }

    pub fn open_in_memory() -> Result<Self, LedgerError> {
        Self::from_connection(Connection::open_in_memory()?)
    }

    fn from_connection(connection: Connection) -> Result<Self, LedgerError> {
        connection.busy_timeout(std::time::Duration::from_secs(5))?;
        connection.execute_batch(
            r#"
            PRAGMA journal_mode = WAL;
            PRAGMA synchronous = FULL;
            PRAGMA foreign_keys = ON;

            CREATE TABLE IF NOT EXISTS job_runs (
                job_id TEXT PRIMARY KEY,
                job_kind TEXT NOT NULL,
                payload_sha256 TEXT NOT NULL,
                state TEXT NOT NULL CHECK (state IN (
                    'RUNNING', 'RETRYABLE_FAILURE', 'SUCCEEDED', 'FAILED',
                    'CANCELLED', 'NEEDS_ATTENTION'
                )),
                attempt INTEGER NOT NULL CHECK (attempt > 0),
                lease_token TEXT NOT NULL,
                result_json TEXT,
                completion_status TEXT,
                error_message TEXT,
                events_json TEXT NOT NULL DEFAULT '[]',
                started_at_ms INTEGER NOT NULL,
                completed_at_ms INTEGER,
                updated_at_ms INTEGER NOT NULL
            );

            CREATE TABLE IF NOT EXISTS job_steps (
                job_id TEXT NOT NULL,
                attempt INTEGER NOT NULL,
                step_index INTEGER NOT NULL,
                step_name TEXT NOT NULL,
                state TEXT NOT NULL CHECK (state IN ('RUNNING', 'SUCCEEDED', 'FAILED')),
                output_json TEXT,
                error_message TEXT,
                started_at_ms INTEGER NOT NULL,
                completed_at_ms INTEGER,
                PRIMARY KEY (job_id, attempt, step_index),
                FOREIGN KEY (job_id) REFERENCES job_runs(job_id) ON DELETE CASCADE
            );

            CREATE INDEX IF NOT EXISTS idx_job_steps_run
                ON job_steps(job_id, attempt, step_index);

            CREATE TABLE IF NOT EXISTS migration_runs (
                service_name TEXT NOT NULL,
                migration_version TEXT NOT NULL,
                checksum TEXT NOT NULL,
                image TEXT NOT NULL,
                state TEXT NOT NULL CHECK (state IN (
                    'RUNNING', 'SUCCEEDED', 'FAILED', 'NEEDS_ATTENTION'
                )),
                job_id TEXT NOT NULL,
                container_id TEXT,
                error_message TEXT,
                started_at_ms INTEGER NOT NULL,
                completed_at_ms INTEGER,
                updated_at_ms INTEGER NOT NULL,
                PRIMARY KEY (service_name, migration_version)
            );

            CREATE INDEX IF NOT EXISTS idx_migration_runs_job
                ON migration_runs(job_id, state);

            CREATE TABLE IF NOT EXISTS provider_revision_runs (
                job_id TEXT PRIMARY KEY,
                previous_revision_json TEXT NOT NULL,
                desired_revision_json TEXT NOT NULL,
                state TEXT NOT NULL CHECK (state IN (
                    'APPLYING', 'DESIRED_APPLIED', 'COMMITTED',
                    'ROLLING_BACK', 'ROLLED_BACK', 'NEEDS_ATTENTION'
                )),
                applied_components_json TEXT NOT NULL DEFAULT '[]',
                error_message TEXT,
                created_at_ms INTEGER NOT NULL,
                updated_at_ms INTEGER NOT NULL,
                FOREIGN KEY (job_id) REFERENCES job_runs(job_id) ON DELETE CASCADE
            );

            CREATE INDEX IF NOT EXISTS idx_provider_revision_state
                ON provider_revision_runs(state, updated_at_ms);
            "#,
        )?;
        let has_events = {
            let mut statement = connection.prepare("PRAGMA table_info(job_runs)")?;
            let columns = statement.query_map([], |row| row.get::<_, String>(1))?;
            columns
                .collect::<Result<Vec<_>, _>>()?
                .iter()
                .any(|column| column == "events_json")
        };
        if !has_events {
            connection.execute(
                "ALTER TABLE job_runs ADD COLUMN events_json TEXT NOT NULL DEFAULT '[]'",
                [],
            )?;
        }
        let mut ledger = Self { connection };
        ledger.recover_interrupted(crate::now_ms())?;
        Ok(ledger)
    }

    /// Atomically marks every in-flight run as ambiguous. This is deliberately
    /// not retryable: the previous process may have completed a Docker mutation
    /// immediately before it died.
    pub fn recover_interrupted(&mut self, now_ms: i64) -> Result<usize, LedgerError> {
        let result_json = serde_json::to_string(&serde_json::json!({
            "recovered_after_restart": true
        }))?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        transaction.execute(
            "UPDATE provider_revision_runs
             SET state = 'NEEDS_ATTENTION', error_message = ?1, updated_at_ms = ?2
             WHERE state IN ('APPLYING', 'DESIRED_APPLIED', 'ROLLING_BACK')",
            params![INTERRUPTED_MESSAGE, now_ms],
        )?;
        transaction.execute(
            "UPDATE migration_runs
             SET state = 'NEEDS_ATTENTION', error_message = ?1,
                 completed_at_ms = ?2, updated_at_ms = ?2
             WHERE state = 'RUNNING'",
            params![INTERRUPTED_MESSAGE, now_ms],
        )?;
        transaction.execute(
            "UPDATE job_steps
             SET state = 'FAILED', error_message = ?1, completed_at_ms = ?2
             WHERE state = 'RUNNING'
               AND EXISTS (
                   SELECT 1 FROM job_runs
                   WHERE job_runs.job_id = job_steps.job_id
                     AND job_runs.state = 'RUNNING'
               )",
            params![INTERRUPTED_MESSAGE, now_ms],
        )?;
        let recovered = transaction.execute(
            "UPDATE job_runs
             SET state = 'NEEDS_ATTENTION', completion_status = 'NEEDS_ATTENTION',
                 result_json = ?1, error_message = ?2, completed_at_ms = ?3,
                 updated_at_ms = ?3
             WHERE state = 'RUNNING'",
            params![result_json, INTERRUPTED_MESSAGE, now_ms],
        )?;
        transaction.commit()?;
        Ok(recovered)
    }

    pub fn begin_provider_revision(
        &mut self,
        job_id: &str,
        previous: &ReleaseProviderRevision,
        desired: &ReleaseProviderRevision,
        now_ms: i64,
    ) -> Result<ProviderRevisionRun, LedgerError> {
        let previous_json = serde_json::to_string(previous)?;
        let desired_json = serde_json::to_string(desired)?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let existing = transaction
            .query_row(
                "SELECT previous_revision_json, desired_revision_json, state,
                        applied_components_json, error_message
                 FROM provider_revision_runs WHERE job_id = ?1",
                params![job_id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, Option<String>>(4)?,
                    ))
                },
            )
            .optional()?;
        let run = if let Some((stored_previous, stored_desired, state, applied, error_message)) =
            existing
        {
            if stored_previous != previous_json || stored_desired != desired_json {
                return Err(LedgerError::ProviderRevisionConflict(job_id.to_string()));
            }
            ProviderRevisionRun {
                job_id: job_id.to_string(),
                previous: previous.clone(),
                desired: desired.clone(),
                state,
                applied_components: serde_json::from_str(&applied)?,
                error_message,
            }
        } else {
            transaction.execute(
                "INSERT INTO provider_revision_runs (
                    job_id, previous_revision_json, desired_revision_json, state,
                    applied_components_json, created_at_ms, updated_at_ms
                 ) VALUES (?1, ?2, ?3, 'APPLYING', '[]', ?4, ?4)",
                params![job_id, previous_json, desired_json, now_ms],
            )?;
            ProviderRevisionRun {
                job_id: job_id.to_string(),
                previous: previous.clone(),
                desired: desired.clone(),
                state: "APPLYING".to_string(),
                applied_components: Vec::new(),
                error_message: None,
            }
        };
        transaction.commit()?;
        Ok(run)
    }

    pub fn mark_provider_component_applied(
        &mut self,
        job_id: &str,
        component: &str,
        now_ms: i64,
    ) -> Result<(), LedgerError> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let applied_json: String = transaction
            .query_row(
                "SELECT applied_components_json FROM provider_revision_runs
                 WHERE job_id = ?1 AND state = 'APPLYING'",
                params![job_id],
                |row| row.get(0),
            )
            .optional()?
            .ok_or_else(|| {
                LedgerError::InvalidState(format!(
                    "provider revision saga for job {job_id} is not APPLYING"
                ))
            })?;
        let mut applied: Vec<String> = serde_json::from_str(&applied_json)?;
        if !applied.iter().any(|value| value == component) {
            applied.push(component.to_string());
        }
        transaction.execute(
            "UPDATE provider_revision_runs
             SET applied_components_json = ?2, updated_at_ms = ?3
             WHERE job_id = ?1 AND state = 'APPLYING'",
            params![job_id, serde_json::to_string(&applied)?, now_ms],
        )?;
        transaction.commit()?;
        Ok(())
    }

    pub fn set_provider_revision_state(
        &mut self,
        job_id: &str,
        state: &str,
        error_message: Option<&str>,
        now_ms: i64,
    ) -> Result<(), LedgerError> {
        if !matches!(
            state,
            "APPLYING"
                | "DESIRED_APPLIED"
                | "COMMITTED"
                | "ROLLING_BACK"
                | "ROLLED_BACK"
                | "NEEDS_ATTENTION"
        ) {
            return Err(LedgerError::InvalidState(format!(
                "invalid provider revision state {state}"
            )));
        }
        let changed = self.connection.execute(
            "UPDATE provider_revision_runs
             SET state = ?2, error_message = ?3, updated_at_ms = ?4
             WHERE job_id = ?1",
            params![job_id, state, error_message, now_ms],
        )?;
        if changed != 1 {
            return Err(LedgerError::InvalidState(format!(
                "provider revision saga for job {job_id} was not found"
            )));
        }
        Ok(())
    }

    pub fn provider_revision(
        &self,
        job_id: &str,
    ) -> Result<Option<ProviderRevisionRun>, LedgerError> {
        self.connection
            .query_row(
                "SELECT previous_revision_json, desired_revision_json, state,
                        applied_components_json, error_message
                 FROM provider_revision_runs WHERE job_id = ?1",
                params![job_id],
                |row| {
                    let previous_json: String = row.get(0)?;
                    let desired_json: String = row.get(1)?;
                    let applied_json: String = row.get(3)?;
                    Ok((
                        previous_json,
                        desired_json,
                        row.get::<_, String>(2)?,
                        applied_json,
                        row.get::<_, Option<String>>(4)?,
                    ))
                },
            )
            .optional()?
            .map(|(previous, desired, state, applied, error_message)| {
                Ok(ProviderRevisionRun {
                    job_id: job_id.to_string(),
                    previous: serde_json::from_str(&previous)?,
                    desired: serde_json::from_str(&desired)?,
                    state,
                    applied_components: serde_json::from_str(&applied)?,
                    error_message,
                })
            })
            .transpose()
    }

    pub fn begin_migration(
        &mut self,
        service_name: &str,
        version: &str,
        checksum: &str,
        image: &str,
        job_id: &str,
        now_ms: i64,
    ) -> Result<MigrationDecision, LedgerError> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let existing = transaction
            .query_row(
                "SELECT checksum, image, state, job_id, container_id, error_message
                 FROM migration_runs
                 WHERE service_name = ?1 AND migration_version = ?2",
                params![service_name, version],
                |row| {
                    Ok(MigrationRun {
                        service_name: service_name.to_string(),
                        version: version.to_string(),
                        checksum: row.get(0)?,
                        image: row.get(1)?,
                        state: row.get(2)?,
                        job_id: row.get(3)?,
                        container_id: row.get(4)?,
                        error_message: row.get(5)?,
                    })
                },
            )
            .optional()?;
        let decision = match existing {
            None => {
                transaction.execute(
                    "INSERT INTO migration_runs (
                        service_name, migration_version, checksum, image, state,
                        job_id, started_at_ms, updated_at_ms
                     ) VALUES (?1, ?2, ?3, ?4, 'RUNNING', ?5, ?6, ?6)",
                    params![service_name, version, checksum, image, job_id, now_ms],
                )?;
                MigrationDecision::Execute
            }
            Some(run) if run.checksum != checksum || run.image != image => {
                return Err(LedgerError::MigrationConflict {
                    service_name: service_name.to_string(),
                    version: version.to_string(),
                });
            }
            Some(run) if run.state == "SUCCEEDED" => MigrationDecision::AlreadyApplied(run),
            Some(_) => {
                return Err(LedgerError::MigrationNeedsAttention {
                    service_name: service_name.to_string(),
                    version: version.to_string(),
                });
            }
        };
        transaction.commit()?;
        Ok(decision)
    }

    pub fn set_migration_container(
        &mut self,
        service_name: &str,
        version: &str,
        job_id: &str,
        container_id: &str,
        now_ms: i64,
    ) -> Result<(), LedgerError> {
        let changed = self.connection.execute(
            "UPDATE migration_runs
             SET container_id = ?4, updated_at_ms = ?5
             WHERE service_name = ?1 AND migration_version = ?2
               AND job_id = ?3 AND state = 'RUNNING'",
            params![service_name, version, job_id, container_id, now_ms],
        )?;
        if changed != 1 {
            return Err(LedgerError::MigrationNeedsAttention {
                service_name: service_name.to_string(),
                version: version.to_string(),
            });
        }
        Ok(())
    }

    pub fn finish_migration(
        &mut self,
        service_name: &str,
        version: &str,
        job_id: &str,
        succeeded: bool,
        error_message: Option<&str>,
        now_ms: i64,
    ) -> Result<(), LedgerError> {
        let state = if succeeded { "SUCCEEDED" } else { "FAILED" };
        let changed = self.connection.execute(
            "UPDATE migration_runs
             SET state = ?4, error_message = ?5, completed_at_ms = ?6,
                 updated_at_ms = ?6
             WHERE service_name = ?1 AND migration_version = ?2
               AND job_id = ?3 AND state = 'RUNNING'",
            params![service_name, version, job_id, state, error_message, now_ms],
        )?;
        if changed != 1 {
            return Err(LedgerError::MigrationNeedsAttention {
                service_name: service_name.to_string(),
                version: version.to_string(),
            });
        }
        Ok(())
    }

    pub fn mark_migration_needs_attention(
        &mut self,
        service_name: &str,
        version: &str,
        job_id: &str,
        error_message: &str,
        now_ms: i64,
    ) -> Result<(), LedgerError> {
        let changed = self.connection.execute(
            "UPDATE migration_runs
             SET state = 'NEEDS_ATTENTION', error_message = ?4,
                 completed_at_ms = ?5, updated_at_ms = ?5
             WHERE service_name = ?1 AND migration_version = ?2
               AND job_id = ?3 AND state = 'RUNNING'",
            params![service_name, version, job_id, error_message, now_ms],
        )?;
        if changed != 1 {
            return Err(LedgerError::MigrationNeedsAttention {
                service_name: service_name.to_string(),
                version: version.to_string(),
            });
        }
        Ok(())
    }

    pub fn migration(
        &self,
        service_name: &str,
        version: &str,
    ) -> Result<Option<MigrationRun>, LedgerError> {
        self.connection
            .query_row(
                "SELECT checksum, image, state, job_id, container_id, error_message
                 FROM migration_runs
                 WHERE service_name = ?1 AND migration_version = ?2",
                params![service_name, version],
                |row| {
                    Ok(MigrationRun {
                        service_name: service_name.to_string(),
                        version: version.to_string(),
                        checksum: row.get(0)?,
                        image: row.get(1)?,
                        state: row.get(2)?,
                        job_id: row.get(3)?,
                        container_id: row.get(4)?,
                        error_message: row.get(5)?,
                    })
                },
            )
            .optional()
            .map_err(Into::into)
    }

    pub fn begin(
        &mut self,
        job_id: &str,
        kind: &JobKind,
        payload_sha256: &str,
        lease_token: &str,
        now_ms: i64,
    ) -> Result<BeginDecision, LedgerError> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let existing = transaction
            .query_row(
                "SELECT job_kind, payload_sha256, state, attempt, result_json,
                        completion_status, error_message, events_json
                 FROM job_runs WHERE job_id = ?1",
                [job_id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, u32>(3)?,
                        row.get::<_, Option<String>>(4)?,
                        row.get::<_, Option<String>>(5)?,
                        row.get::<_, Option<String>>(6)?,
                        row.get::<_, String>(7)?,
                    ))
                },
            )
            .optional()?;

        let kind_json = serde_json::to_string(kind)?;
        let decision = match existing {
            None => {
                transaction.execute(
                    "INSERT INTO job_runs (
                        job_id, job_kind, payload_sha256, state, attempt, lease_token,
                        started_at_ms, updated_at_ms
                     ) VALUES (?1, ?2, ?3, 'RUNNING', 1, ?4, ?5, ?5)",
                    params![job_id, kind_json, payload_sha256, lease_token, now_ms],
                )?;
                BeginDecision::Execute { attempt: 1 }
            }
            Some((stored_kind, stored_hash, state, attempt, result, status, error, events)) => {
                if stored_hash != payload_sha256 || stored_kind != kind_json {
                    return Err(LedgerError::PayloadConflict {
                        job_id: job_id.to_string(),
                    });
                }
                let state = LedgerRunState::parse(&state)?;
                if state == LedgerRunState::RetryableFailure {
                    let next_attempt = attempt.saturating_add(1);
                    transaction.execute(
                        "UPDATE job_runs
                         SET state = 'RUNNING', attempt = ?2, lease_token = ?3,
                             result_json = NULL, completion_status = NULL,
                             error_message = NULL, events_json = '[]', completed_at_ms = NULL,
                             started_at_ms = ?4, updated_at_ms = ?4
                         WHERE job_id = ?1",
                        params![job_id, next_attempt, lease_token, now_ms],
                    )?;
                    BeginDecision::Execute {
                        attempt: next_attempt,
                    }
                } else if state == LedgerRunState::Running {
                    let result = serde_json::json!({
                        "recovered_without_process_restart": true
                    });
                    transaction.execute(
                        "UPDATE job_steps
                         SET state = 'FAILED', error_message = ?2, completed_at_ms = ?3
                         WHERE job_id = ?1 AND attempt = ?4 AND state = 'RUNNING'",
                        params![job_id, INTERRUPTED_MESSAGE, now_ms, attempt],
                    )?;
                    transaction.execute(
                        "UPDATE job_runs
                         SET state = 'NEEDS_ATTENTION', lease_token = ?2,
                             result_json = ?3, completion_status = 'NEEDS_ATTENTION',
                             error_message = ?4, completed_at_ms = ?5,
                             updated_at_ms = ?5
                         WHERE job_id = ?1",
                        params![
                            job_id,
                            lease_token,
                            serde_json::to_string(&result)?,
                            INTERRUPTED_MESSAGE,
                            now_ms
                        ],
                    )?;
                    BeginDecision::Replay(StoredCompletion {
                        status: CompletionStatus::NeedsAttention,
                        result,
                        error_message: INTERRUPTED_MESSAGE.to_string(),
                        events: vec![],
                    })
                } else {
                    let completion = stored_completion(state, result, status, error, &events)?;
                    transaction.execute(
                        "UPDATE job_runs SET lease_token = ?2, updated_at_ms = ?3
                         WHERE job_id = ?1",
                        params![job_id, lease_token, now_ms],
                    )?;
                    BeginDecision::Replay(completion)
                }
            }
        };
        transaction.commit()?;
        Ok(decision)
    }

    pub fn finish(
        &mut self,
        job_id: &str,
        completion: &StoredCompletion,
        now_ms: i64,
    ) -> Result<(), LedgerError> {
        let state = state_for_completion(&completion.status);
        let changed = self.connection.execute(
            "UPDATE job_runs
             SET state = ?2, completion_status = ?3, result_json = ?4,
                 error_message = ?5, events_json = ?6,
                 completed_at_ms = ?7, updated_at_ms = ?7
             WHERE job_id = ?1 AND state = 'RUNNING'",
            params![
                job_id,
                state.as_str(),
                completion_status_str(&completion.status),
                serde_json::to_string(&completion.result)?,
                nullable_message(&completion.error_message),
                serde_json::to_string(&completion.events)?,
                now_ms,
            ],
        )?;
        if changed != 1 {
            return Err(LedgerError::NotRunning(job_id.to_string()));
        }
        Ok(())
    }

    pub fn step_started(
        &mut self,
        job_id: &str,
        step_index: u32,
        step_name: &str,
        now_ms: i64,
    ) -> Result<(), LedgerError> {
        let attempt = self.running_attempt(job_id)?;
        self.connection.execute(
            "INSERT INTO job_steps (
                job_id, attempt, step_index, step_name, state, started_at_ms
             ) VALUES (?1, ?2, ?3, ?4, 'RUNNING', ?5)",
            params![job_id, attempt, step_index, step_name, now_ms],
        )?;
        Ok(())
    }

    pub fn step_succeeded(
        &mut self,
        job_id: &str,
        step_index: u32,
        output: &Value,
        now_ms: i64,
    ) -> Result<(), LedgerError> {
        self.finish_step(job_id, step_index, "SUCCEEDED", Some(output), None, now_ms)
    }

    pub fn step_failed(
        &mut self,
        job_id: &str,
        step_index: u32,
        error_message: &str,
        now_ms: i64,
    ) -> Result<(), LedgerError> {
        self.finish_step(
            job_id,
            step_index,
            "FAILED",
            None,
            Some(error_message),
            now_ms,
        )
    }

    fn finish_step(
        &mut self,
        job_id: &str,
        step_index: u32,
        state: &str,
        output: Option<&Value>,
        error_message: Option<&str>,
        now_ms: i64,
    ) -> Result<(), LedgerError> {
        let attempt = self.running_attempt(job_id)?;
        let changed = self.connection.execute(
            "UPDATE job_steps
             SET state = ?4, output_json = ?5, error_message = ?6,
                 completed_at_ms = ?7
             WHERE job_id = ?1 AND attempt = ?2 AND step_index = ?3
               AND state = 'RUNNING'",
            params![
                job_id,
                attempt,
                step_index,
                state,
                output.map(serde_json::to_string).transpose()?,
                error_message,
                now_ms,
            ],
        )?;
        if changed != 1 {
            return Err(LedgerError::NotRunning(job_id.to_string()));
        }
        Ok(())
    }

    fn running_attempt(&self, job_id: &str) -> Result<u32, LedgerError> {
        self.connection
            .query_row(
                "SELECT attempt FROM job_runs WHERE job_id = ?1 AND state = 'RUNNING'",
                [job_id],
                |row| row.get(0),
            )
            .optional()?
            .ok_or_else(|| LedgerError::NotRunning(job_id.to_string()))
    }

    pub(crate) fn active_attempt(&self, job_id: &str) -> Result<u32, LedgerError> {
        self.running_attempt(job_id)
    }

    pub fn get(&self, job_id: &str) -> Result<Option<JobRun>, LedgerError> {
        let raw = self
            .connection
            .query_row(
                "SELECT job_kind, payload_sha256, state, attempt, lease_token,
                        result_json, completion_status, error_message, events_json,
                        started_at_ms, completed_at_ms, updated_at_ms
                 FROM job_runs WHERE job_id = ?1",
                [job_id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, u32>(3)?,
                        row.get::<_, String>(4)?,
                        row.get::<_, Option<String>>(5)?,
                        row.get::<_, Option<String>>(6)?,
                        row.get::<_, Option<String>>(7)?,
                        row.get::<_, String>(8)?,
                        row.get::<_, i64>(9)?,
                        row.get::<_, Option<i64>>(10)?,
                        row.get::<_, i64>(11)?,
                    ))
                },
            )
            .optional()?;
        raw.map(
            |(
                kind,
                payload_sha256,
                state,
                attempt,
                lease_token,
                result,
                status,
                error,
                events,
                started_at_ms,
                completed_at_ms,
                updated_at_ms,
            )| {
                let state = LedgerRunState::parse(&state)?;
                let completion = if state == LedgerRunState::Running {
                    None
                } else {
                    Some(stored_completion(state, result, status, error, &events)?)
                };
                Ok(JobRun {
                    job_id: job_id.to_string(),
                    kind: serde_json::from_str(&kind)?,
                    payload_sha256,
                    state,
                    attempt,
                    lease_token,
                    completion,
                    started_at_ms,
                    completed_at_ms,
                    updated_at_ms,
                })
            },
        )
        .transpose()
    }

    pub fn steps(&self, job_id: &str) -> Result<Vec<JobStep>, LedgerError> {
        let mut statement = self.connection.prepare(
            "SELECT attempt, step_index, step_name, state, output_json, error_message
             FROM job_steps WHERE job_id = ?1 ORDER BY attempt, step_index",
        )?;
        let rows = statement.query_map([job_id], |row| {
            Ok((
                row.get::<_, u32>(0)?,
                row.get::<_, u32>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, Option<String>>(4)?,
                row.get::<_, Option<String>>(5)?,
            ))
        })?;
        rows.map(|row| {
            let (attempt, step_index, step_name, state, output, error_message) = row?;
            Ok(JobStep {
                job_id: job_id.to_string(),
                attempt,
                step_index,
                step_name,
                state,
                output: output
                    .map(|value| serde_json::from_str(&value))
                    .transpose()?,
                error_message,
            })
        })
        .collect()
    }
}

fn stored_completion(
    state: LedgerRunState,
    result: Option<String>,
    status: Option<String>,
    error: Option<String>,
    events: &str,
) -> Result<StoredCompletion, LedgerError> {
    let status = match status.as_deref() {
        Some("SUCCEEDED") => CompletionStatus::Succeeded,
        Some("RETRYABLE_FAILURE") => CompletionStatus::RetryableFailure,
        Some("FAILED") => CompletionStatus::Failed,
        Some("CANCELLED") => CompletionStatus::Cancelled,
        Some("NEEDS_ATTENTION") => CompletionStatus::NeedsAttention,
        Some(other) => return Err(LedgerError::InvalidState(other.to_string())),
        None if state == LedgerRunState::Running => {
            return Err(LedgerError::InvalidState("RUNNING completion".to_string()));
        }
        None => return Err(LedgerError::InvalidState("missing completion".to_string())),
    };
    Ok(StoredCompletion {
        status,
        result: result
            .map(|value| serde_json::from_str(&value))
            .transpose()?
            .unwrap_or(Value::Null),
        error_message: error.unwrap_or_default(),
        events: serde_json::from_str(events)?,
    })
}

fn completion_status_str(status: &CompletionStatus) -> &'static str {
    match status {
        CompletionStatus::Succeeded => "SUCCEEDED",
        CompletionStatus::RetryableFailure => "RETRYABLE_FAILURE",
        CompletionStatus::Failed => "FAILED",
        CompletionStatus::Cancelled => "CANCELLED",
        CompletionStatus::NeedsAttention => "NEEDS_ATTENTION",
    }
}

fn state_for_completion(status: &CompletionStatus) -> LedgerRunState {
    match status {
        CompletionStatus::Succeeded => LedgerRunState::Succeeded,
        CompletionStatus::RetryableFailure => LedgerRunState::RetryableFailure,
        CompletionStatus::Failed => LedgerRunState::Failed,
        CompletionStatus::Cancelled => LedgerRunState::Cancelled,
        CompletionStatus::NeedsAttention => LedgerRunState::NeedsAttention,
    }
}

fn nullable_message(message: &str) -> Option<&str> {
    (!message.is_empty()).then_some(message)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn completion(status: CompletionStatus) -> StoredCompletion {
        StoredCompletion {
            status,
            result: serde_json::json!({"container_id": "container-1"}),
            error_message: String::new(),
            events: vec![],
        }
    }

    fn probe_event() -> NewJobEvent {
        NewJobEvent {
            sequence: 1_000_001,
            event_type: "runtime.health_probe".to_string(),
            level: "INFO".to_string(),
            message: "health probe 1: ready".to_string(),
            data: serde_json::json!({"decision": "ready"}),
        }
    }

    #[test]
    fn terminal_run_is_replayed_without_new_attempt() {
        let mut ledger = AgentLedger::open_in_memory().unwrap();
        assert_eq!(
            ledger
                .begin("job-1", &JobKind::Install, "hash", "lease-1", 10)
                .unwrap(),
            BeginDecision::Execute { attempt: 1 }
        );
        ledger
            .finish("job-1", &completion(CompletionStatus::Succeeded), 20)
            .unwrap();

        let replay = ledger
            .begin("job-1", &JobKind::Install, "hash", "lease-2", 30)
            .unwrap();
        assert!(matches!(replay, BeginDecision::Replay(_)));
        assert_eq!(ledger.get("job-1").unwrap().unwrap().attempt, 1);
    }

    #[test]
    fn completion_events_are_persisted_and_replayed() {
        let mut ledger = AgentLedger::open_in_memory().unwrap();
        ledger
            .begin("job-events", &JobKind::Install, "hash", "lease-1", 10)
            .unwrap();
        let mut completed = completion(CompletionStatus::Succeeded);
        completed.events.push(probe_event());
        ledger.finish("job-events", &completed, 20).unwrap();

        let replay = ledger
            .begin("job-events", &JobKind::Install, "hash", "lease-2", 30)
            .unwrap();

        assert_eq!(replay, BeginDecision::Replay(completed));
    }

    #[test]
    fn existing_ledger_is_expanded_with_event_storage() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("old-agent.sqlite3");
        let connection = Connection::open(&path).unwrap();
        connection
            .execute_batch(
                "CREATE TABLE job_runs (
                    job_id TEXT PRIMARY KEY,
                    job_kind TEXT NOT NULL,
                    payload_sha256 TEXT NOT NULL,
                    state TEXT NOT NULL,
                    attempt INTEGER NOT NULL,
                    lease_token TEXT NOT NULL,
                    result_json TEXT,
                    completion_status TEXT,
                    error_message TEXT,
                    started_at_ms INTEGER NOT NULL,
                    completed_at_ms INTEGER,
                    updated_at_ms INTEGER NOT NULL
                );",
            )
            .unwrap();
        drop(connection);

        let ledger = AgentLedger::open(&path).unwrap();
        let has_events = ledger
            .connection
            .prepare("PRAGMA table_info(job_runs)")
            .unwrap()
            .query_map([], |row| row.get::<_, String>(1))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap()
            .iter()
            .any(|column| column == "events_json");
        assert!(has_events);
    }

    #[test]
    fn payload_or_kind_change_is_rejected() {
        let mut ledger = AgentLedger::open_in_memory().unwrap();
        ledger
            .begin("job-1", &JobKind::Install, "hash-a", "lease-1", 10)
            .unwrap();
        assert!(matches!(
            ledger.begin("job-1", &JobKind::Install, "hash-b", "lease-2", 20),
            Err(LedgerError::PayloadConflict { .. })
        ));
        assert!(matches!(
            ledger.begin("job-1", &JobKind::Start, "hash-a", "lease-2", 20),
            Err(LedgerError::PayloadConflict { .. })
        ));
    }

    #[test]
    fn retryable_failure_creates_a_new_attempt() {
        let mut ledger = AgentLedger::open_in_memory().unwrap();
        ledger
            .begin("job-1", &JobKind::Health, "hash", "lease-1", 10)
            .unwrap();
        ledger
            .finish("job-1", &completion(CompletionStatus::RetryableFailure), 20)
            .unwrap();
        assert_eq!(
            ledger
                .begin("job-1", &JobKind::Health, "hash", "lease-2", 30)
                .unwrap(),
            BeginDecision::Execute { attempt: 2 }
        );
    }

    #[test]
    fn opening_ledger_marks_interrupted_run_needs_attention() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("agent.sqlite3");
        {
            let mut ledger = AgentLedger::open(&path).unwrap();
            ledger
                .begin("job-1", &JobKind::Start, "hash", "lease-1", 10)
                .unwrap();
            ledger.step_started("job-1", 1, "start", 11).unwrap();
        }

        let mut reopened = AgentLedger::open(&path).unwrap();
        let run = reopened.get("job-1").unwrap().unwrap();
        assert_eq!(run.state, LedgerRunState::NeedsAttention);
        assert_eq!(
            run.completion.unwrap().status,
            CompletionStatus::NeedsAttention
        );
        assert!(matches!(
            reopened
                .begin("job-1", &JobKind::Start, "hash", "lease-2", 30)
                .unwrap(),
            BeginDecision::Replay(StoredCompletion {
                status: CompletionStatus::NeedsAttention,
                ..
            })
        ));
    }

    #[test]
    fn migration_ledger_replays_exact_success_and_rejects_changed_artifact() {
        let mut ledger = AgentLedger::open_in_memory().unwrap();
        assert_eq!(
            ledger
                .begin_migration(
                    "demo",
                    "0001",
                    "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                    "ghcr.io/acme/migrate@sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
                    "job-1",
                    10,
                )
                .unwrap(),
            MigrationDecision::Execute
        );
        ledger
            .set_migration_container("demo", "0001", "job-1", "container-1", 11)
            .unwrap();
        ledger
            .finish_migration("demo", "0001", "job-1", true, None, 12)
            .unwrap();
        assert!(matches!(
            ledger
                .begin_migration(
                    "demo",
                    "0001",
                    "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                    "ghcr.io/acme/migrate@sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
                    "job-2",
                    20,
                )
                .unwrap(),
            MigrationDecision::AlreadyApplied(MigrationRun { state, .. }) if state == "SUCCEEDED"
        ));
        assert!(matches!(
            ledger.begin_migration(
                "demo",
                "0001",
                "sha256:cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc",
                "ghcr.io/acme/migrate@sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
                "job-3",
                30,
            ),
            Err(LedgerError::MigrationConflict { .. })
        ));
    }

    #[test]
    fn restart_marks_inflight_migration_needs_attention() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("migration-ledger.sqlite3");
        {
            let mut ledger = AgentLedger::open(&path).unwrap();
            ledger
                .begin_migration(
                    "demo",
                    "0001",
                    "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                    "ghcr.io/acme/migrate@sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
                    "job-1",
                    10,
                )
                .unwrap();
            ledger
                .set_migration_container("demo", "0001", "job-1", "container-1", 11)
                .unwrap();
        }

        let ledger = AgentLedger::open(&path).unwrap();
        let run = ledger.migration("demo", "0001").unwrap().unwrap();
        assert_eq!(run.state, "NEEDS_ATTENTION");
        assert!(matches!(
            ledger
                .connection
                .query_row(
                    "SELECT state FROM migration_runs WHERE service_name = 'demo'",
                    [],
                    |row| row.get::<_, String>(0),
                )
                .unwrap()
                .as_str(),
            "NEEDS_ATTENTION"
        ));
    }

    #[test]
    fn restart_preserves_both_provider_revisions_and_marks_saga_for_attention() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("provider-revision-ledger.sqlite3");
        let previous = ReleaseProviderRevision {
            revision_id: "revision-old".to_string(),
            ..ReleaseProviderRevision::default()
        };
        let desired = ReleaseProviderRevision {
            revision_id: "revision-new".to_string(),
            ..ReleaseProviderRevision::default()
        };
        {
            let mut ledger = AgentLedger::open(&path).unwrap();
            ledger
                .begin("job-provider", &JobKind::Upgrade, "hash", "lease-1", 10)
                .unwrap();
            ledger
                .begin_provider_revision("job-provider", &previous, &desired, 11)
                .unwrap();
            ledger
                .mark_provider_component_applied("job-provider", "auth", 12)
                .unwrap();
        }

        let ledger = AgentLedger::open(&path).unwrap();
        let revision = ledger
            .provider_revision("job-provider")
            .unwrap()
            .expect("durable provider revision");
        assert_eq!(revision.state, "NEEDS_ATTENTION");
        assert_eq!(revision.previous, previous);
        assert_eq!(revision.desired, desired);
        assert_eq!(revision.applied_components, ["auth"]);
        assert!(revision.error_message.unwrap().contains("restarted"));
    }
}
