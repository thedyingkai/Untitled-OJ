use orchestrator_control_plane::{CompletionStatus, JobKind, NewJobEvent};
use orchestrator_runtime::{
    ManagedServiceContextSpec, ManagedVolumeSpec, MigrationContainerIdentityV1,
    ReleaseProviderRevision, RuntimeContext,
};
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
    #[error("runtime context for deployment {0} conflicts with its durable Agent-local record")]
    RuntimeContextConflict(String),
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
    pub resource_claims_sha256: String,
    pub identity_sha256: String,
    pub state: String,
    pub job_id: String,
    pub container_id: Option<String>,
    pub error_message: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MigrationDecision {
    Execute,
    AlreadyApplied(Box<MigrationRun>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MigrationRegistration {
    Missing,
    Exact(MigrationRun),
    Conflict(MigrationRun),
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeContextRun {
    pub deployment_id: String,
    pub job_id: String,
    pub context: RuntimeContext,
    pub state: String,
    pub container_id: Option<String>,
    pub error_message: Option<String>,
    pub managed_context: Option<ManagedServiceContextSpec>,
    pub previous_managed_context: Option<ManagedServiceContextSpec>,
    pub binding_context_state: String,
    pub managed_volume: Option<ManagedVolumeSpec>,
    pub managed_volume_state: String,
    pub managed_volume_owned: bool,
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
                resource_claims_sha256 TEXT NOT NULL DEFAULT '',
                identity_sha256 TEXT NOT NULL DEFAULT '',
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

            CREATE TABLE IF NOT EXISTS runtime_context_runs (
                deployment_id TEXT PRIMARY KEY,
                job_id TEXT NOT NULL,
                context_json TEXT NOT NULL,
                state TEXT NOT NULL CHECK (state IN (
                    'MATERIALIZING', 'PREPARED', 'CREATING', 'BOUND', 'ACTIVE',
                    'CLEANUP_NEEDED', 'CLEANUP_RUNNING', 'CLEANED',
                    'NEEDS_ATTENTION'
                )),
                container_id TEXT,
                error_message TEXT,
                managed_context_json TEXT,
                previous_managed_context_json TEXT,
                binding_context_state TEXT NOT NULL DEFAULT 'ACTIVE'
                    CHECK (binding_context_state IN ('ACTIVE', 'REVOKED')),
                managed_volume_spec_json TEXT,
                managed_volume_state TEXT NOT NULL DEFAULT 'NONE'
                    CHECK (managed_volume_state IN (
                        'NONE', 'CREATING', 'CREATED', 'CLEANUP_NEEDED',
                        'CLEANUP_RUNNING', 'CLEANED', 'NEEDS_ATTENTION'
                    )),
                managed_volume_owned INTEGER NOT NULL DEFAULT 0
                    CHECK (managed_volume_owned IN (0, 1)),
                created_at_ms INTEGER NOT NULL,
                updated_at_ms INTEGER NOT NULL
            );

            CREATE UNIQUE INDEX IF NOT EXISTS idx_runtime_context_container
                ON runtime_context_runs(container_id)
                WHERE container_id IS NOT NULL;

            CREATE INDEX IF NOT EXISTS idx_runtime_context_state
                ON runtime_context_runs(state, updated_at_ms);
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
        let migration_columns = {
            let mut statement = connection.prepare("PRAGMA table_info(migration_runs)")?;
            statement
                .query_map([], |row| row.get::<_, String>(1))?
                .collect::<Result<Vec<_>, _>>()?
        };
        for (name, declaration) in [
            ("resource_claims_sha256", "TEXT NOT NULL DEFAULT ''"),
            ("identity_sha256", "TEXT NOT NULL DEFAULT ''"),
        ] {
            if !migration_columns.iter().any(|column| column == name) {
                connection.execute(
                    &format!("ALTER TABLE migration_runs ADD COLUMN {name} {declaration}"),
                    [],
                )?;
            }
        }
        let runtime_context_columns = {
            let mut statement = connection.prepare("PRAGMA table_info(runtime_context_runs)")?;
            statement
                .query_map([], |row| row.get::<_, String>(1))?
                .collect::<Result<Vec<_>, _>>()?
        };
        for (name, declaration) in [
            ("managed_context_json", "TEXT"),
            ("previous_managed_context_json", "TEXT"),
            ("binding_context_state", "TEXT NOT NULL DEFAULT 'ACTIVE'"),
            ("managed_volume_spec_json", "TEXT"),
            ("managed_volume_state", "TEXT NOT NULL DEFAULT 'NONE'"),
            ("managed_volume_owned", "INTEGER NOT NULL DEFAULT 0"),
        ] {
            if !runtime_context_columns.iter().any(|column| column == name) {
                connection.execute(
                    &format!("ALTER TABLE runtime_context_runs ADD COLUMN {name} {declaration}"),
                    [],
                )?;
            }
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
            "UPDATE runtime_context_runs
             SET managed_volume_state = 'CLEANUP_NEEDED', updated_at_ms = ?1
             WHERE state IN ('MATERIALIZING', 'PREPARED', 'CLEANUP_RUNNING')
               AND managed_volume_state IN (
                    'CREATING', 'CREATED', 'CLEANUP_NEEDED', 'CLEANUP_RUNNING'
               )",
            params![now_ms],
        )?;
        transaction.execute(
            "UPDATE runtime_context_runs
             SET managed_volume_state = 'NEEDS_ATTENTION', updated_at_ms = ?1
             WHERE state IN ('CREATING', 'BOUND')
               AND managed_volume_state IN (
                    'CREATING', 'CREATED', 'CLEANUP_NEEDED', 'CLEANUP_RUNNING'
               )",
            params![now_ms],
        )?;
        transaction.execute(
            "UPDATE runtime_context_runs
             SET state = 'CLEANUP_NEEDED', error_message = ?1, updated_at_ms = ?2
             WHERE state IN ('MATERIALIZING', 'PREPARED', 'CLEANUP_RUNNING')",
            params![INTERRUPTED_MESSAGE, now_ms],
        )?;
        transaction.execute(
            "UPDATE runtime_context_runs
             SET state = 'NEEDS_ATTENTION', error_message = ?1, updated_at_ms = ?2
            WHERE state IN ('CREATING', 'BOUND')",
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

    pub fn begin_runtime_context(
        &mut self,
        job_id: &str,
        deployment_id: &str,
        context: &RuntimeContext,
        now_ms: i64,
    ) -> Result<RuntimeContextRun, LedgerError> {
        let context_json = serde_json::to_string(context)?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let existing = transaction
            .query_row(
                "SELECT job_id, context_json, state, container_id, error_message,
                        managed_volume_spec_json, managed_volume_state,
                        managed_volume_owned
                 FROM runtime_context_runs WHERE deployment_id = ?1",
                [deployment_id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, Option<String>>(3)?,
                        row.get::<_, Option<String>>(4)?,
                        row.get::<_, Option<String>>(5)?,
                        row.get::<_, String>(6)?,
                        row.get::<_, bool>(7)?,
                    ))
                },
            )
            .optional()?;
        match existing {
            None => {
                transaction.execute(
                    "INSERT INTO runtime_context_runs (
                        deployment_id, job_id, context_json, state,
                        created_at_ms, updated_at_ms
                     ) VALUES (?1, ?2, ?3, 'MATERIALIZING', ?4, ?4)",
                    params![deployment_id, job_id, context_json, now_ms],
                )?;
            }
            Some((_, stored_context, state, _, _, _, _, _))
                if stored_context == context_json && state == "CLEANED" =>
            {
                transaction.execute(
                    "UPDATE runtime_context_runs
                     SET job_id = ?2, state = 'MATERIALIZING', container_id = NULL,
                         error_message = NULL, managed_volume_spec_json = NULL,
                         managed_volume_state = 'NONE', managed_volume_owned = 0,
                         updated_at_ms = ?3
                     WHERE deployment_id = ?1 AND state = 'CLEANED'",
                    params![deployment_id, job_id, now_ms],
                )?;
            }
            Some((
                stored_job,
                stored_context,
                state,
                container_id,
                error_message,
                volume_spec,
                volume_state,
                volume_owned,
            )) if stored_job == job_id
                && stored_context == context_json
                && matches!(state.as_str(), "MATERIALIZING" | "PREPARED") =>
            {
                transaction.commit()?;
                return Ok(RuntimeContextRun {
                    deployment_id: deployment_id.to_string(),
                    job_id: stored_job,
                    context: context.clone(),
                    state,
                    container_id,
                    error_message,
                    managed_context: None,
                    previous_managed_context: None,
                    binding_context_state: "ACTIVE".to_string(),
                    managed_volume: volume_spec
                        .map(|value| serde_json::from_str(&value))
                        .transpose()?,
                    managed_volume_state: volume_state,
                    managed_volume_owned: volume_owned,
                });
            }
            Some(_) => {
                return Err(LedgerError::RuntimeContextConflict(
                    deployment_id.to_string(),
                ));
            }
        }
        transaction.commit()?;
        Ok(RuntimeContextRun {
            deployment_id: deployment_id.to_string(),
            job_id: job_id.to_string(),
            context: context.clone(),
            state: "MATERIALIZING".to_string(),
            container_id: None,
            error_message: None,
            managed_context: None,
            previous_managed_context: None,
            binding_context_state: "ACTIVE".to_string(),
            managed_volume: None,
            managed_volume_state: "NONE".to_string(),
            managed_volume_owned: false,
        })
    }

    pub fn record_binding_context_transition(
        &mut self,
        deployment_id: &str,
        job_id: &str,
        previous: Option<&ManagedServiceContextSpec>,
        desired: Option<&ManagedServiceContextSpec>,
        revoked: bool,
        now_ms: i64,
    ) -> Result<(), LedgerError> {
        let previous_json = previous.map(serde_json::to_string).transpose()?;
        let desired_json = desired.map(serde_json::to_string).transpose()?;
        let state = if revoked { "REVOKED" } else { "ACTIVE" };
        let changed = self.connection.execute(
            "UPDATE runtime_context_runs
             SET job_id = ?2, previous_managed_context_json = ?3,
                 managed_context_json = ?4, binding_context_state = ?5,
                 error_message = NULL, updated_at_ms = ?6
             WHERE deployment_id = ?1 AND state = 'ACTIVE'",
            params![
                deployment_id,
                job_id,
                previous_json,
                desired_json,
                state,
                now_ms
            ],
        )?;
        if changed != 1 {
            return Err(LedgerError::InvalidState(format!(
                "runtime context for deployment {deployment_id} is not ACTIVE"
            )));
        }
        Ok(())
    }

    pub fn mark_runtime_context_prepared(
        &mut self,
        deployment_id: &str,
        job_id: &str,
        now_ms: i64,
    ) -> Result<(), LedgerError> {
        self.transition_runtime_context(
            deployment_id,
            job_id,
            &["MATERIALIZING"],
            "PREPARED",
            None,
            None,
            now_ms,
        )
    }

    pub fn begin_managed_volume(
        &mut self,
        deployment_id: &str,
        job_id: &str,
        spec: &ManagedVolumeSpec,
        now_ms: i64,
    ) -> Result<(), LedgerError> {
        let current = self
            .runtime_context_for_deployment(deployment_id)?
            .ok_or_else(|| {
                LedgerError::InvalidState(format!(
                    "runtime context for deployment {deployment_id} was not found"
                ))
            })?;
        if current.job_id != job_id
            || current.state != "MATERIALIZING"
            || spec.deployment_id != deployment_id
            || match spec.runtime_contract.id {
                orchestrator_runtime::RuntimeProfile::JudgeSandboxV1 => {
                    spec.name != current.context.cache_volume_name
                }
                orchestrator_runtime::RuntimeProfile::StandardV1 => {
                    !current.context.cache_volume_name.is_empty()
                        || spec.lifecycle != orchestrator_runtime::RETAIN_VOLUME_LIFECYCLE
                }
            }
        {
            return Err(LedgerError::InvalidState(format!(
                "managed volume for deployment {deployment_id} does not match its MATERIALIZING runtime context"
            )));
        }
        let spec_json = serde_json::to_string(spec)?;
        if current.managed_volume.as_ref() == Some(spec)
            && current.managed_volume_state == "CREATING"
        {
            return Ok(());
        }
        if !matches!(current.managed_volume_state.as_str(), "NONE" | "CLEANED")
            || current.managed_volume_owned
        {
            return Err(LedgerError::InvalidState(format!(
                "managed volume for deployment {deployment_id} is already {}",
                current.managed_volume_state
            )));
        }
        let changed = self.connection.execute(
            "UPDATE runtime_context_runs
             SET managed_volume_spec_json = ?3, managed_volume_state = 'CREATING',
                 managed_volume_owned = 0, updated_at_ms = ?4
             WHERE deployment_id = ?1 AND job_id = ?2 AND state = 'MATERIALIZING'
               AND managed_volume_state IN ('NONE', 'CLEANED')
               AND managed_volume_owned = 0",
            params![deployment_id, job_id, spec_json, now_ms],
        )?;
        if changed != 1 {
            return Err(LedgerError::InvalidState(format!(
                "managed volume for deployment {deployment_id} changed concurrently"
            )));
        }
        Ok(())
    }

    pub fn mark_managed_volume_created(
        &mut self,
        deployment_id: &str,
        job_id: &str,
        now_ms: i64,
    ) -> Result<(), LedgerError> {
        let changed = self.connection.execute(
            "UPDATE runtime_context_runs
             SET managed_volume_state = 'CREATED', managed_volume_owned = 1,
                 updated_at_ms = ?3
             WHERE deployment_id = ?1 AND job_id = ?2 AND state = 'MATERIALIZING'
               AND managed_volume_spec_json IS NOT NULL
               AND managed_volume_state = 'CREATING'",
            params![deployment_id, job_id, now_ms],
        )?;
        if changed != 1 {
            return Err(LedgerError::InvalidState(format!(
                "managed volume for deployment {deployment_id} is not CREATING"
            )));
        }
        Ok(())
    }

    /// Starts idempotent compensation and returns the exact persisted
    /// ownership contract. CREATING is intentionally eligible: Docker may have
    /// created the volume immediately before the Agent lost the response, and
    /// the runtime adapter verifies ownership labels before deleting it.
    pub fn begin_managed_volume_cleanup(
        &mut self,
        deployment_id: &str,
        now_ms: i64,
    ) -> Result<Option<ManagedVolumeSpec>, LedgerError> {
        let current = self
            .runtime_context_for_deployment(deployment_id)?
            .ok_or_else(|| {
                LedgerError::InvalidState(format!(
                    "runtime context for deployment {deployment_id} was not found"
                ))
            })?;
        let Some(spec) = current.managed_volume else {
            return Ok(None);
        };
        if matches!(current.managed_volume_state.as_str(), "NONE" | "CLEANED") {
            return Ok(None);
        }
        if current.state != "CLEANUP_RUNNING"
            || !matches!(
                current.managed_volume_state.as_str(),
                "CREATING" | "CREATED" | "CLEANUP_NEEDED" | "CLEANUP_RUNNING" | "NEEDS_ATTENTION"
            )
        {
            return Err(LedgerError::InvalidState(format!(
                "managed volume for deployment {deployment_id} cannot begin cleanup from context={} volume={}",
                current.state, current.managed_volume_state
            )));
        }
        if current.managed_volume_state != "CLEANUP_RUNNING" {
            let changed = self.connection.execute(
                "UPDATE runtime_context_runs
                 SET managed_volume_state = 'CLEANUP_RUNNING', updated_at_ms = ?2
                 WHERE deployment_id = ?1 AND state = 'CLEANUP_RUNNING'
                   AND managed_volume_state IN (
                       'CREATING', 'CREATED', 'CLEANUP_NEEDED', 'NEEDS_ATTENTION'
                   )",
                params![deployment_id, now_ms],
            )?;
            if changed != 1 {
                return Err(LedgerError::InvalidState(format!(
                    "managed volume for deployment {deployment_id} changed concurrently"
                )));
            }
        }
        Ok(Some(spec))
    }

    pub fn finish_managed_volume_cleanup(
        &mut self,
        deployment_id: &str,
        now_ms: i64,
    ) -> Result<(), LedgerError> {
        let changed = self.connection.execute(
            "UPDATE runtime_context_runs
             SET managed_volume_state = 'CLEANED', managed_volume_owned = 0,
                 updated_at_ms = ?2
             WHERE deployment_id = ?1 AND state = 'CLEANUP_RUNNING'
               AND managed_volume_state = 'CLEANUP_RUNNING'",
            params![deployment_id, now_ms],
        )?;
        if changed != 1 {
            return Err(LedgerError::InvalidState(format!(
                "managed volume for deployment {deployment_id} is not being cleaned"
            )));
        }
        Ok(())
    }

    pub fn mark_managed_volume_cleanup_needed(
        &mut self,
        deployment_id: &str,
        now_ms: i64,
    ) -> Result<(), LedgerError> {
        let changed = self.connection.execute(
            "UPDATE runtime_context_runs
             SET managed_volume_state = 'CLEANUP_NEEDED', updated_at_ms = ?2
             WHERE deployment_id = ?1
               AND managed_volume_state IN (
                   'CREATING', 'CREATED', 'CLEANUP_RUNNING', 'NEEDS_ATTENTION'
               )",
            params![deployment_id, now_ms],
        )?;
        if changed != 1 {
            return Err(LedgerError::InvalidState(format!(
                "managed volume for deployment {deployment_id} cannot be marked for cleanup"
            )));
        }
        Ok(())
    }

    pub fn bind_runtime_context(
        &mut self,
        deployment_id: &str,
        job_id: &str,
        container_id: &str,
        now_ms: i64,
    ) -> Result<(), LedgerError> {
        if container_id.trim().is_empty() {
            return Err(LedgerError::InvalidState(
                "runtime context cannot bind an empty container id".to_string(),
            ));
        }
        self.transition_runtime_context(
            deployment_id,
            job_id,
            &["CREATING"],
            "BOUND",
            Some(Some(container_id)),
            None,
            now_ms,
        )
    }

    pub fn mark_runtime_context_creating(
        &mut self,
        deployment_id: &str,
        job_id: &str,
        now_ms: i64,
    ) -> Result<(), LedgerError> {
        let current = self
            .runtime_context_for_deployment(deployment_id)?
            .ok_or_else(|| {
                LedgerError::InvalidState(format!(
                    "runtime context for deployment {deployment_id} was not found"
                ))
            })?;
        if current.managed_volume.is_some()
            && (current.managed_volume_state != "CREATED" || !current.managed_volume_owned)
        {
            return Err(LedgerError::InvalidState(format!(
                "runtime context for deployment {deployment_id} cannot create a container before its managed volume is durably owned"
            )));
        }
        self.transition_runtime_context(
            deployment_id,
            job_id,
            &["PREPARED"],
            "CREATING",
            None,
            None,
            now_ms,
        )
    }

    pub fn activate_runtime_context(
        &mut self,
        deployment_id: &str,
        job_id: &str,
        now_ms: i64,
    ) -> Result<(), LedgerError> {
        self.transition_runtime_context(
            deployment_id,
            job_id,
            &["BOUND"],
            "ACTIVE",
            None,
            None,
            now_ms,
        )
    }

    pub fn begin_runtime_context_cleanup(
        &mut self,
        deployment_id: &str,
        now_ms: i64,
    ) -> Result<(), LedgerError> {
        let changed = self.connection.execute(
            "UPDATE runtime_context_runs
             SET state = 'CLEANUP_RUNNING', error_message = NULL, updated_at_ms = ?2
             WHERE deployment_id = ?1 AND state IN (
                'MATERIALIZING', 'PREPARED', 'CREATING', 'BOUND', 'ACTIVE',
                'CLEANUP_NEEDED', 'NEEDS_ATTENTION'
             )",
            params![deployment_id, now_ms],
        )?;
        if changed != 1 {
            return Err(LedgerError::InvalidState(format!(
                "runtime context for deployment {deployment_id} cannot begin cleanup"
            )));
        }
        Ok(())
    }

    pub fn finish_runtime_context_cleanup(
        &mut self,
        deployment_id: &str,
        now_ms: i64,
    ) -> Result<(), LedgerError> {
        let changed = self.connection.execute(
            "UPDATE runtime_context_runs
             SET state = 'CLEANED', container_id = NULL, error_message = NULL,
                 updated_at_ms = ?2
             WHERE deployment_id = ?1 AND state = 'CLEANUP_RUNNING'
               AND managed_volume_state IN ('NONE', 'CLEANED')
               AND managed_volume_owned = 0",
            params![deployment_id, now_ms],
        )?;
        if changed != 1 {
            return Err(LedgerError::InvalidState(format!(
                "runtime context for deployment {deployment_id} is not being cleaned"
            )));
        }
        Ok(())
    }

    pub fn mark_runtime_context_cleanup_needed(
        &mut self,
        deployment_id: &str,
        error_message: &str,
        now_ms: i64,
    ) -> Result<(), LedgerError> {
        let changed = self.connection.execute(
            "UPDATE runtime_context_runs
             SET state = 'CLEANUP_NEEDED', error_message = ?2, updated_at_ms = ?3
             WHERE deployment_id = ?1 AND state IN (
                'MATERIALIZING', 'PREPARED', 'CLEANUP_RUNNING'
             )",
            params![deployment_id, error_message, now_ms],
        )?;
        if changed != 1 {
            return Err(LedgerError::InvalidState(format!(
                "runtime context for deployment {deployment_id} cannot be marked for cleanup"
            )));
        }
        Ok(())
    }

    pub fn mark_runtime_context_needs_attention(
        &mut self,
        deployment_id: &str,
        error_message: &str,
        now_ms: i64,
    ) -> Result<(), LedgerError> {
        let changed = self.connection.execute(
            "UPDATE runtime_context_runs
             SET state = 'NEEDS_ATTENTION', error_message = ?2,
                 managed_volume_state = CASE
                     WHEN managed_volume_state IN ('CREATING', 'CREATED')
                     THEN 'NEEDS_ATTENTION'
                     ELSE managed_volume_state
                 END,
                 updated_at_ms = ?3
             WHERE deployment_id = ?1 AND state <> 'CLEANED'",
            params![deployment_id, error_message, now_ms],
        )?;
        if changed != 1 {
            return Err(LedgerError::InvalidState(format!(
                "runtime context for deployment {deployment_id} cannot enter NEEDS_ATTENTION"
            )));
        }
        Ok(())
    }

    pub fn runtime_context_for_container(
        &self,
        container_id: &str,
    ) -> Result<Option<RuntimeContextRun>, LedgerError> {
        self.runtime_context_query("container_id = ?1", container_id)
    }

    pub fn runtime_context_for_deployment(
        &self,
        deployment_id: &str,
    ) -> Result<Option<RuntimeContextRun>, LedgerError> {
        self.runtime_context_query("deployment_id = ?1", deployment_id)
    }

    pub fn pending_runtime_context_cleanups(&self) -> Result<Vec<RuntimeContextRun>, LedgerError> {
        let mut statement = self.connection.prepare(
            "SELECT deployment_id, job_id, context_json, state, container_id, error_message,
                    managed_context_json, previous_managed_context_json, binding_context_state,
                    managed_volume_spec_json, managed_volume_state, managed_volume_owned
             FROM runtime_context_runs WHERE state = 'CLEANUP_NEEDED'
             ORDER BY deployment_id",
        )?;
        let rows = statement.query_map([], runtime_context_row)?;
        rows.collect::<Result<Vec<_>, _>>()?
            .into_iter()
            .map(decode_runtime_context_run)
            .collect()
    }

    pub fn active_runtime_contexts(&self) -> Result<Vec<RuntimeContextRun>, LedgerError> {
        let mut statement = self.connection.prepare(
            "SELECT deployment_id, job_id, context_json, state, container_id, error_message,
                    managed_context_json, previous_managed_context_json, binding_context_state,
                    managed_volume_spec_json, managed_volume_state, managed_volume_owned
             FROM runtime_context_runs WHERE state = 'ACTIVE' AND binding_context_state = 'ACTIVE'
             ORDER BY deployment_id",
        )?;
        let rows = statement.query_map([], runtime_context_row)?;
        rows.collect::<Result<Vec<_>, _>>()?
            .into_iter()
            .map(decode_runtime_context_run)
            .collect()
    }

    #[allow(clippy::too_many_arguments)]
    fn transition_runtime_context(
        &mut self,
        deployment_id: &str,
        job_id: &str,
        from_states: &[&str],
        to_state: &str,
        container_id: Option<Option<&str>>,
        error_message: Option<&str>,
        now_ms: i64,
    ) -> Result<(), LedgerError> {
        let current = self
            .runtime_context_for_deployment(deployment_id)?
            .ok_or_else(|| {
                LedgerError::InvalidState(format!(
                    "runtime context for deployment {deployment_id} was not found"
                ))
            })?;
        if current.job_id != job_id || !from_states.contains(&current.state.as_str()) {
            return Err(LedgerError::InvalidState(format!(
                "runtime context for deployment {deployment_id} is {} for job {}, expected {:?} for job {job_id}",
                current.state, current.job_id, from_states
            )));
        }
        let next_container = match container_id {
            Some(value) => value.map(str::to_string),
            None => current.container_id,
        };
        let changed = self.connection.execute(
            "UPDATE runtime_context_runs
             SET state = ?3, container_id = ?4, error_message = ?5, updated_at_ms = ?6
             WHERE deployment_id = ?1 AND job_id = ?2 AND state = ?7",
            params![
                deployment_id,
                job_id,
                to_state,
                next_container,
                error_message,
                now_ms,
                current.state,
            ],
        )?;
        if changed != 1 {
            return Err(LedgerError::InvalidState(format!(
                "runtime context for deployment {deployment_id} changed concurrently"
            )));
        }
        Ok(())
    }

    fn runtime_context_query(
        &self,
        predicate: &str,
        value: &str,
    ) -> Result<Option<RuntimeContextRun>, LedgerError> {
        let sql = format!(
            "SELECT deployment_id, job_id, context_json, state, container_id, error_message,
                    managed_context_json, previous_managed_context_json, binding_context_state,
                    managed_volume_spec_json, managed_volume_state, managed_volume_owned
             FROM runtime_context_runs WHERE {predicate}"
        );
        self.connection
            .query_row(&sql, [value], runtime_context_row)
            .optional()?
            .map(decode_runtime_context_run)
            .transpose()
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

    #[allow(clippy::too_many_arguments)]
    pub fn begin_migration(
        &mut self,
        service_name: &str,
        version: &str,
        checksum: &str,
        image: &str,
        resource_claims_sha256: &str,
        identity_sha256: &str,
        job_id: &str,
        now_ms: i64,
    ) -> Result<MigrationDecision, LedgerError> {
        let identity = MigrationContainerIdentityV1 {
            job_id: job_id.to_string(),
            service_name: service_name.to_string(),
            version: version.to_string(),
            checksum: checksum.to_string(),
            image: image.to_string(),
            resource_claims_sha256: resource_claims_sha256.to_string(),
            identity_sha256: identity_sha256.to_string(),
        };
        identity
            .validate()
            .map_err(|error| LedgerError::InvalidState(error.to_string()))?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let existing = transaction
            .query_row(
                "SELECT checksum, image, resource_claims_sha256, identity_sha256,
                        state, job_id, container_id, error_message
                 FROM migration_runs
                 WHERE service_name = ?1 AND migration_version = ?2",
                params![service_name, version],
                |row| {
                    Ok(MigrationRun {
                        service_name: service_name.to_string(),
                        version: version.to_string(),
                        checksum: row.get(0)?,
                        image: row.get(1)?,
                        resource_claims_sha256: row.get(2)?,
                        identity_sha256: row.get(3)?,
                        state: row.get(4)?,
                        job_id: row.get(5)?,
                        container_id: row.get(6)?,
                        error_message: row.get(7)?,
                    })
                },
            )
            .optional()?;
        let decision = match existing {
            None => {
                transaction.execute(
                    "INSERT INTO migration_runs (
                        service_name, migration_version, checksum, image,
                        resource_claims_sha256, identity_sha256, state,
                        job_id, started_at_ms, updated_at_ms
                     ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'RUNNING', ?7, ?8, ?8)",
                    params![
                        service_name,
                        version,
                        checksum,
                        image,
                        resource_claims_sha256,
                        identity_sha256,
                        job_id,
                        now_ms
                    ],
                )?;
                MigrationDecision::Execute
            }
            Some(run)
                if run.checksum != checksum
                    || run.image != image
                    || (!run.resource_claims_sha256.is_empty()
                        && run.resource_claims_sha256 != resource_claims_sha256)
                    || (!run.identity_sha256.is_empty()
                        && run.identity_sha256 != identity_sha256) =>
            {
                return Err(LedgerError::MigrationConflict {
                    service_name: service_name.to_string(),
                    version: version.to_string(),
                });
            }
            Some(run) if run.state == "SUCCEEDED" => {
                MigrationDecision::AlreadyApplied(Box::new(run))
            }
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
                "SELECT checksum, image, resource_claims_sha256, identity_sha256,
                        state, job_id, container_id, error_message
                 FROM migration_runs
                 WHERE service_name = ?1 AND migration_version = ?2",
                params![service_name, version],
                |row| {
                    Ok(MigrationRun {
                        service_name: service_name.to_string(),
                        version: version.to_string(),
                        checksum: row.get(0)?,
                        image: row.get(1)?,
                        resource_claims_sha256: row.get(2)?,
                        identity_sha256: row.get(3)?,
                        state: row.get(4)?,
                        job_id: row.get(5)?,
                        container_id: row.get(6)?,
                        error_message: row.get(7)?,
                    })
                },
            )
            .optional()
            .map_err(Into::into)
    }

    /// Finds a migration using the complete immutable Docker identity. A row
    /// for the same service/version but a different job or digest is a
    /// conflict, never evidence that an observed container is registered.
    pub fn migration_registration(
        &self,
        identity: &MigrationContainerIdentityV1,
    ) -> Result<MigrationRegistration, LedgerError> {
        let Some(run) = self.migration(&identity.service_name, &identity.version)? else {
            return Ok(MigrationRegistration::Missing);
        };
        if run.job_id == identity.job_id
            && run.checksum == identity.checksum
            && run.image == identity.image
            && run.resource_claims_sha256 == identity.resource_claims_sha256
            && run.identity_sha256 == identity.identity_sha256
        {
            Ok(MigrationRegistration::Exact(run))
        } else {
            Ok(MigrationRegistration::Conflict(run))
        }
    }

    /// Persists an ambiguous orphan before Docker cleanup. This tombstone is
    /// intentionally keyed by service/version so any subsequent migration
    /// attempt is rejected until a human reconciles the database outcome.
    pub fn tombstone_unregistered_migration(
        &mut self,
        identity: &MigrationContainerIdentityV1,
        container_id: &str,
        evidence: &str,
        now_ms: i64,
    ) -> Result<MigrationRegistration, LedgerError> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let changed = transaction.execute(
            "INSERT INTO migration_runs (
                service_name, migration_version, checksum, image,
                resource_claims_sha256, identity_sha256, state, job_id,
                container_id, error_message, started_at_ms, completed_at_ms,
                updated_at_ms
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'NEEDS_ATTENTION', ?7,
                       ?8, ?9, ?10, ?10, ?10)
             ON CONFLICT(service_name, migration_version) DO NOTHING",
            params![
                identity.service_name,
                identity.version,
                identity.checksum,
                identity.image,
                identity.resource_claims_sha256,
                identity.identity_sha256,
                identity.job_id,
                container_id,
                evidence,
                now_ms,
            ],
        )?;
        transaction.commit()?;
        if changed == 1 {
            return self.migration_registration(identity);
        }
        self.migration_registration(identity)
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

type RuntimeContextRow = (
    String,
    String,
    String,
    String,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
    String,
    Option<String>,
    String,
    bool,
);

fn runtime_context_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<RuntimeContextRow> {
    Ok((
        row.get(0)?,
        row.get(1)?,
        row.get(2)?,
        row.get(3)?,
        row.get(4)?,
        row.get(5)?,
        row.get(6)?,
        row.get(7)?,
        row.get(8)?,
        row.get(9)?,
        row.get(10)?,
        row.get(11)?,
    ))
}

fn decode_runtime_context_run(row: RuntimeContextRow) -> Result<RuntimeContextRun, LedgerError> {
    Ok(RuntimeContextRun {
        deployment_id: row.0,
        job_id: row.1,
        context: serde_json::from_str(&row.2)?,
        state: row.3,
        container_id: row.4,
        error_message: row.5,
        managed_context: row
            .6
            .map(|value| serde_json::from_str(&value))
            .transpose()?,
        previous_managed_context: row
            .7
            .map(|value| serde_json::from_str(&value))
            .transpose()?,
        binding_context_state: row.8,
        managed_volume: row
            .9
            .map(|value| serde_json::from_str(&value))
            .transpose()?,
        managed_volume_state: row.10,
        managed_volume_owned: row.11,
    })
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

    fn runtime_context() -> RuntimeContext {
        let root = if cfg!(windows) {
            "C:\\ojos"
        } else {
            "/var/lib/ojos"
        };
        RuntimeContext {
            contract: orchestrator_runtime::RuntimeContract::judge_sandbox_v1(),
            runtime_policy_sha256: format!("sha256:{}", "a".repeat(64)),
            scratch_directory: format!("{root}/contexts/deployment-1/work"),
            cache_volume_name: "ojos-judge-cache-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_string(),
            service_context_directory: format!("{root}/contexts/deployment-1/service"),
        }
    }

    fn managed_volume() -> ManagedVolumeSpec {
        ManagedVolumeSpec {
            name: "ojos-judge-cache-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_string(),
            deployment_id: "deployment-1".to_string(),
            service_id: "judge-worker".to_string(),
            artifact_digest: format!("ghcr.io/acme/judge-worker@sha256:{}", "b".repeat(64)),
            runtime_contract: orchestrator_runtime::RuntimeContract::judge_sandbox_v1(),
            logical_name: orchestrator_runtime::JUDGE_CACHE_VOLUME_LOGICAL_NAME.to_string(),
            lifecycle: orchestrator_runtime::RELEASE_VOLUME_LIFECYCLE.to_string(),
            owner_instance_id: String::new(),
            target: "/var/lib/ojos-worker/cache".to_string(),
        }
    }

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
        let image = orchestrator_runtime::OciImageReference::parse(
            "ghcr.io/acme/migrate@sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
        )
        .unwrap();
        let resources = orchestrator_runtime::migration_resource_claims_sha256(&[]).unwrap();
        let identity = orchestrator_runtime::migration_identity_sha256(
            "demo",
            "0001",
            "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            &image,
            &resources,
        )
        .unwrap();
        assert_eq!(
            ledger
                .begin_migration(
                    "demo",
                    "0001",
                    "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                    "ghcr.io/acme/migrate@sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
                    &resources,
                    &identity,
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
                    &resources,
                    &identity,
                    "job-2",
                    20,
                )
                .unwrap(),
            MigrationDecision::AlreadyApplied(run) if run.state == "SUCCEEDED"
        ));
        assert!(matches!(
            ledger.begin_migration(
                "demo",
                "0001",
                "sha256:cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc",
                "ghcr.io/acme/migrate@sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
                &resources,
                &orchestrator_runtime::migration_identity_sha256(
                    "demo",
                    "0001",
                    "sha256:cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc",
                    &image,
                    &resources,
                )
                .unwrap(),
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
            let image = orchestrator_runtime::OciImageReference::parse(
                "ghcr.io/acme/migrate@sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            )
            .unwrap();
            let resources = orchestrator_runtime::migration_resource_claims_sha256(&[]).unwrap();
            let identity = orchestrator_runtime::migration_identity_sha256(
                "demo",
                "0001",
                "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                &image,
                &resources,
            )
            .unwrap();
            ledger
                .begin_migration(
                    "demo",
                    "0001",
                    "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                    "ghcr.io/acme/migrate@sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
                    &resources,
                    &identity,
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

    #[test]
    fn restart_schedules_prepared_context_cleanup_without_persisting_a_token() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("runtime-context-ledger.sqlite3");
        {
            let mut ledger = AgentLedger::open(&path).unwrap();
            ledger
                .begin("job-context", &JobKind::Install, "hash", "lease", 10)
                .unwrap();
            ledger
                .begin_runtime_context("job-context", "deployment-1", &runtime_context(), 11)
                .unwrap();
            ledger
                .mark_runtime_context_prepared("deployment-1", "job-context", 12)
                .unwrap();
            let stored: String = ledger
                .connection
                .query_row(
                    "SELECT context_json FROM runtime_context_runs WHERE deployment_id = 'deployment-1'",
                    [],
                    |row| row.get(0),
                )
                .unwrap();
            assert!(!stored.contains("access_token"));
            assert!(!stored.contains("fixture-token"));
        }

        let ledger = AgentLedger::open(&path).unwrap();
        let pending = ledger.pending_runtime_context_cleanups().unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].state, "CLEANUP_NEEDED");
        assert!(pending[0].container_id.is_none());
    }

    #[test]
    fn restart_reconciles_ambiguous_volume_create_without_losing_ownership_contract() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("runtime-volume-creating.sqlite3");
        {
            let mut ledger = AgentLedger::open(&path).unwrap();
            ledger
                .begin("job-context", &JobKind::Install, "hash", "lease", 10)
                .unwrap();
            ledger
                .begin_runtime_context("job-context", "deployment-1", &runtime_context(), 11)
                .unwrap();
            ledger
                .begin_managed_volume("deployment-1", "job-context", &managed_volume(), 13)
                .unwrap();
            // Simulate process death after Docker may have handled create but
            // before the Agent persisted CREATED/ownership.
        }

        let mut ledger = AgentLedger::open(&path).unwrap();
        let pending = ledger.pending_runtime_context_cleanups().unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].state, "CLEANUP_NEEDED");
        assert_eq!(pending[0].managed_volume_state, "CLEANUP_NEEDED");
        assert!(!pending[0].managed_volume_owned);
        assert_eq!(pending[0].managed_volume.as_ref(), Some(&managed_volume()));

        ledger
            .begin_runtime_context_cleanup("deployment-1", 20)
            .unwrap();
        assert_eq!(
            ledger
                .begin_managed_volume_cleanup("deployment-1", 21)
                .unwrap(),
            Some(managed_volume())
        );
        assert!(
            ledger
                .finish_runtime_context_cleanup("deployment-1", 22)
                .is_err(),
            "context cleanup cannot commit while the owned volume is unresolved"
        );
        ledger
            .finish_managed_volume_cleanup("deployment-1", 23)
            .unwrap();
        ledger
            .finish_runtime_context_cleanup("deployment-1", 24)
            .unwrap();
        let cleaned = ledger
            .runtime_context_for_deployment("deployment-1")
            .unwrap()
            .unwrap();
        assert_eq!(cleaned.state, "CLEANED");
        assert_eq!(cleaned.managed_volume_state, "CLEANED");
        assert!(!cleaned.managed_volume_owned);
    }

    #[test]
    fn restart_never_blindly_cleans_a_context_after_create_may_have_run() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("runtime-context-creating.sqlite3");
        {
            let mut ledger = AgentLedger::open(&path).unwrap();
            ledger
                .begin("job-context", &JobKind::Install, "hash", "lease", 10)
                .unwrap();
            ledger
                .begin_runtime_context("job-context", "deployment-1", &runtime_context(), 11)
                .unwrap();
            ledger
                .begin_managed_volume("deployment-1", "job-context", &managed_volume(), 12)
                .unwrap();
            ledger
                .mark_managed_volume_created("deployment-1", "job-context", 12)
                .unwrap();
            ledger
                .mark_runtime_context_prepared("deployment-1", "job-context", 12)
                .unwrap();
            ledger
                .mark_runtime_context_creating("deployment-1", "job-context", 13)
                .unwrap();
        }

        let ledger = AgentLedger::open(&path).unwrap();
        assert!(
            ledger
                .pending_runtime_context_cleanups()
                .unwrap()
                .is_empty()
        );
        let run = ledger
            .runtime_context_for_deployment("deployment-1")
            .unwrap()
            .unwrap();
        assert_eq!(run.state, "NEEDS_ATTENTION");
        assert_eq!(run.managed_volume_state, "NEEDS_ATTENTION");
        assert!(run.managed_volume_owned);
    }

    #[test]
    fn active_runtime_context_survives_restart_for_credential_recovery() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("runtime-context-active.sqlite3");
        {
            let mut ledger = AgentLedger::open(&path).unwrap();
            ledger
                .begin("job-context", &JobKind::Install, "hash", "lease", 10)
                .unwrap();
            ledger
                .begin_runtime_context("job-context", "deployment-1", &runtime_context(), 11)
                .unwrap();
            ledger
                .begin_managed_volume("deployment-1", "job-context", &managed_volume(), 12)
                .unwrap();
            ledger
                .mark_managed_volume_created("deployment-1", "job-context", 12)
                .unwrap();
            ledger
                .mark_runtime_context_prepared("deployment-1", "job-context", 12)
                .unwrap();
            ledger
                .mark_runtime_context_creating("deployment-1", "job-context", 13)
                .unwrap();
            ledger
                .bind_runtime_context("deployment-1", "job-context", "container-1", 14)
                .unwrap();
            ledger
                .activate_runtime_context("deployment-1", "job-context", 15)
                .unwrap();
            ledger
                .finish("job-context", &completion(CompletionStatus::Succeeded), 16)
                .unwrap();
        }

        let ledger = AgentLedger::open(&path).unwrap();
        let active = ledger.active_runtime_contexts().unwrap();
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].container_id.as_deref(), Some("container-1"));
        assert_eq!(active[0].state, "ACTIVE");
        assert_eq!(active[0].managed_volume_state, "CREATED");
        assert!(active[0].managed_volume_owned);
    }
}
