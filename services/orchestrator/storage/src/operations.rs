use crate::SqliteOrchestratorStore;
use crate::jobs::lease_episode_identity;
use orchestrator_control_plane::{
    DurableOperation, OperationRepository, OperationStoreError, validate_durable_operation,
    validate_durable_operation_update,
};
use rusqlite::{OptionalExtension, TransactionBehavior, params};
use std::collections::{BTreeMap, BTreeSet};

pub(crate) const STALE_OPERATION_AFTER_MS: i64 = 300_000;
pub(crate) const EXPIRED_LEASE_COUNTER: &str = "expired_job_lease_transitions";
pub(crate) const LONG_OPERATION_COUNTER: &str = "operation_over_300_seconds_transitions";

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ControlPlaneAnomalyCounters {
    pub expired_job_lease_transitions_total: u64,
    pub operation_over_300_seconds_transitions_total: u64,
}

#[derive(Debug, Clone)]
pub struct SqliteOperationStore {
    storage: SqliteOrchestratorStore,
}

impl SqliteOperationStore {
    pub fn new(storage: SqliteOrchestratorStore) -> Self {
        Self { storage }
    }

    pub fn storage(&self) -> &SqliteOrchestratorStore {
        &self.storage
    }

    pub fn anomaly_counters(&self) -> Result<ControlPlaneAnomalyCounters, OperationStoreError> {
        let connection = self.storage.connection().map_err(storage_error)?;
        read_anomaly_counters(&connection)
    }

    /// Moves counters and active episode identities from the legacy generic
    /// state record into the v9 transactional evidence tables. Identities are
    /// seeded only when they still match the current durable row, preventing
    /// a crash between legacy observation and upgrade from being recounted.
    pub fn migrate_legacy_anomaly_state(
        &self,
        expired_leases: u64,
        long_operations: u64,
        expired_lease_episodes: &BTreeMap<String, String>,
        long_operation_episodes: &BTreeSet<String>,
    ) -> Result<ControlPlaneAnomalyCounters, OperationStoreError> {
        let mut connection = self.storage.connection().map_err(storage_error)?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(sqlite_error)?;
        read_anomaly_counters(&transaction)?;
        set_counter_floor(&transaction, EXPIRED_LEASE_COUNTER, expired_leases)?;
        set_counter_floor(&transaction, LONG_OPERATION_COUNTER, long_operations)?;
        for (job_id, expected_identity) in expired_lease_episodes {
            let payload = transaction
                .query_row(
                    "SELECT payload FROM orchestrator_jobs WHERE job_id = ?1",
                    [job_id],
                    |row| row.get::<_, String>(0),
                )
                .optional()
                .map_err(sqlite_error)?;
            let Some(payload) = payload else { continue };
            let job: orchestrator_control_plane::Job =
                serde_json::from_str(&payload).map_err(json_error)?;
            if lease_episode_identity(&job).as_ref() != Some(expected_identity) {
                continue;
            }
            transaction
                .execute(
                    "INSERT INTO orchestrator_active_expired_lease_anomalies(job_id, lease_identity) VALUES (?1, ?2) ON CONFLICT(job_id) DO UPDATE SET lease_identity = excluded.lease_identity",
                    params![job_id, expected_identity],
                )
                .map_err(sqlite_error)?;
        }
        for expected_episode in long_operation_episodes {
            let operation_id = expected_episode.rsplitn(3, ':').nth(2).unwrap_or_default();
            let payload = transaction
                .query_row(
                    "SELECT payload FROM orchestrator_durable_operations WHERE operation_id = ?1",
                    [operation_id],
                    |row| row.get::<_, String>(0),
                )
                .optional()
                .map_err(sqlite_error)?;
            let Some(payload) = payload else { continue };
            let operation = decode(&payload)?;
            if operation_episode_identity(&operation) != *expected_episode
                || !operation_is_active(&operation)
            {
                continue;
            }
            transaction
                .execute(
                    "INSERT INTO orchestrator_active_operation_anomalies(episode_id, operation_id, generation, started_at_ms) VALUES (?1, ?2, ?3, ?4) ON CONFLICT(episode_id) DO NOTHING",
                    params![
                        expected_episode,
                        operation.operation_id,
                        revision_i64(u64::from(operation.generation))?,
                        operation.started_at_ms,
                    ],
                )
                .map_err(sqlite_error)?;
        }
        let counters = read_anomaly_counters(&transaction)?;
        transaction.commit().map_err(sqlite_error)?;
        Ok(counters)
    }

    /// Rechecks every candidate while holding the same write lock used by
    /// Operation CAS. Therefore an active observation and a terminal commit
    /// serialize around the marker/counter update and can never double count
    /// or miss an episode because wall-clock timestamps committed out of order.
    pub fn observe_active_operation_anomalies(
        &self,
        candidates: &[DurableOperation],
        now_ms: i64,
    ) -> Result<ControlPlaneAnomalyCounters, OperationStoreError> {
        let mut connection = self.storage.connection().map_err(storage_error)?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(sqlite_error)?;
        let mut inserted = 0_u64;
        for candidate in candidates {
            let payload = transaction
                .query_row(
                    "SELECT payload FROM orchestrator_durable_operations WHERE operation_id = ?1",
                    [&candidate.operation_id],
                    |row| row.get::<_, String>(0),
                )
                .optional()
                .map_err(sqlite_error)?;
            let Some(payload) = payload else {
                continue;
            };
            let current = decode(&payload)?;
            if operation_episode_identity(&current) != operation_episode_identity(candidate)
                || !active_operation_exceeded_limit(&current, now_ms)
            {
                continue;
            }
            let generation = revision_i64(u64::from(current.generation))?;
            inserted = inserted.saturating_add(
                transaction
                    .execute(
                        "INSERT INTO orchestrator_active_operation_anomalies(episode_id, operation_id, generation, started_at_ms) VALUES (?1, ?2, ?3, ?4) ON CONFLICT(episode_id) DO NOTHING",
                        params![
                            operation_episode_identity(&current),
                            current.operation_id,
                            generation,
                            current.started_at_ms,
                        ],
                    )
                    .map_err(sqlite_error)? as u64,
            );
        }
        increment_counter(&transaction, LONG_OPERATION_COUNTER, inserted)?;
        let counters = read_anomaly_counters(&transaction)?;
        transaction.commit().map_err(sqlite_error)?;
        Ok(counters)
    }

    pub fn anomaly_candidates(&self) -> Result<Vec<DurableOperation>, OperationStoreError> {
        let connection = self.storage.connection().map_err(storage_error)?;
        let mut statement = connection
            .prepare(
                "SELECT payload FROM orchestrator_durable_operations WHERE status IN ('ENQUEUING', 'RUNNING', 'CANCELLING') ORDER BY operation_id",
            )
            .map_err(sqlite_error)?;
        statement
            .query_map([], |row| row.get::<_, String>(0))
            .map_err(sqlite_error)?
            .map(|payload| {
                payload
                    .map_err(sqlite_error)
                    .and_then(|value| decode(&value))
            })
            .collect()
    }
}

impl OperationRepository for SqliteOperationStore {
    fn create(
        &mut self,
        operation: DurableOperation,
    ) -> Result<DurableOperation, OperationStoreError> {
        validate_durable_operation(&operation)?;
        let payload = serde_json::to_string(&operation).map_err(json_error)?;
        let changed = self
            .storage
            .connection()
            .map_err(storage_error)?
            .execute(
                "INSERT INTO orchestrator_durable_operations(operation_id, revision, status, payload, created_at_ms, updated_at_ms) VALUES (?1, ?2, ?3, ?4, ?5, ?6) ON CONFLICT(operation_id) DO NOTHING",
                params![
                    operation.operation_id,
                    revision_i64(operation.revision)?,
                    status(&operation),
                    payload,
                    operation.created_at_ms,
                    operation.updated_at_ms,
                ],
            )
            .map_err(sqlite_error)?;
        if changed != 1 {
            return Err(OperationStoreError::AlreadyExists(operation.operation_id));
        }
        Ok(operation)
    }

    fn get(&self, operation_id: &str) -> Result<Option<DurableOperation>, OperationStoreError> {
        let payload = self
            .storage
            .connection()
            .map_err(storage_error)?
            .query_row(
                "SELECT payload FROM orchestrator_durable_operations WHERE operation_id = ?1",
                [operation_id],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(sqlite_error)?;
        payload.map(|payload| decode(&payload)).transpose()
    }

    fn compare_and_swap(
        &mut self,
        expected_revision: u64,
        operation: DurableOperation,
    ) -> Result<DurableOperation, OperationStoreError> {
        let mut connection = self.storage.connection().map_err(storage_error)?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(sqlite_error)?;
        let payload = transaction
            .query_row(
                "SELECT payload FROM orchestrator_durable_operations WHERE operation_id = ?1",
                [&operation.operation_id],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(sqlite_error)?
            .ok_or_else(|| OperationStoreError::NotFound(operation.operation_id.clone()))?;
        let current = decode(&payload)?;
        validate_durable_operation_update(&current, expected_revision, &operation)?;
        record_anomaly_transition(&transaction, &current, &operation)?;
        let changed = transaction
            .execute(
                "UPDATE orchestrator_durable_operations SET revision = ?2, status = ?3, payload = ?4, updated_at_ms = ?5 WHERE operation_id = ?1 AND revision = ?6",
                params![
                    operation.operation_id,
                    revision_i64(operation.revision)?,
                    status(&operation),
                    serde_json::to_string(&operation).map_err(json_error)?,
                    operation.updated_at_ms,
                    revision_i64(expected_revision)?,
                ],
            )
            .map_err(sqlite_error)?;
        if changed != 1 {
            let actual = transaction
                .query_row(
                    "SELECT revision FROM orchestrator_durable_operations WHERE operation_id = ?1",
                    [&operation.operation_id],
                    |row| row.get::<_, i64>(0),
                )
                .map_err(sqlite_error)?;
            return Err(OperationStoreError::RevisionConflict {
                expected: expected_revision,
                actual: u64::try_from(actual).unwrap_or_default(),
            });
        }
        transaction.commit().map_err(sqlite_error)?;
        Ok(operation)
    }

    fn recoverable(&self) -> Result<Vec<DurableOperation>, OperationStoreError> {
        let connection = self.storage.connection().map_err(storage_error)?;
        let mut statement = connection
            .prepare(
                "SELECT payload FROM orchestrator_durable_operations WHERE status IN ('CONFIRMED', 'ENQUEUING', 'RUNNING', 'CANCELLING') ORDER BY updated_at_ms, operation_id",
            )
            .map_err(sqlite_error)?;
        let payloads = statement
            .query_map([], |row| row.get::<_, String>(0))
            .map_err(sqlite_error)?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(sqlite_error)?;
        payloads
            .into_iter()
            .map(|payload| decode(&payload))
            .collect()
    }

    fn list(&self) -> Result<Vec<DurableOperation>, OperationStoreError> {
        let connection = self.storage.connection().map_err(storage_error)?;
        let mut statement = connection
            .prepare(
                "SELECT payload FROM orchestrator_durable_operations ORDER BY created_at_ms DESC, operation_id DESC",
            )
            .map_err(sqlite_error)?;
        let payloads = statement
            .query_map([], |row| row.get::<_, String>(0))
            .map_err(sqlite_error)?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(sqlite_error)?;
        payloads
            .into_iter()
            .map(|payload| decode(&payload))
            .collect()
    }
}

pub(crate) fn operation_episode_identity(operation: &DurableOperation) -> String {
    format!(
        "{}:{}:{}",
        operation.operation_id,
        operation.generation,
        operation
            .started_at_ms
            .map_or_else(|| "not-started".to_string(), |value| value.to_string())
    )
}

pub(crate) fn active_operation_exceeded_limit(operation: &DurableOperation, now_ms: i64) -> bool {
    if !operation_is_active(operation) {
        return false;
    }
    let started_at_ms = operation.started_at_ms.unwrap_or(operation.updated_at_ms);
    now_ms.saturating_sub(started_at_ms) > STALE_OPERATION_AFTER_MS
        || now_ms.saturating_sub(operation.updated_at_ms) > STALE_OPERATION_AFTER_MS
}

fn operation_is_active(operation: &DurableOperation) -> bool {
    matches!(
        operation.status,
        orchestrator_control_plane::DurableOperationStatus::Enqueuing
            | orchestrator_control_plane::DurableOperationStatus::Running
            | orchestrator_control_plane::DurableOperationStatus::Cancelling
    )
}

pub(crate) fn terminal_operation_exceeded_limit(operation: &DurableOperation) -> bool {
    if !operation.status.is_terminal() {
        return false;
    }
    let (Some(started_at_ms), Some(finished_at_ms)) =
        (operation.started_at_ms, operation.finished_at_ms)
    else {
        return false;
    };
    started_at_ms > 0
        && finished_at_ms >= started_at_ms
        && finished_at_ms.saturating_sub(started_at_ms) > STALE_OPERATION_AFTER_MS
}

fn record_anomaly_transition(
    transaction: &rusqlite::Transaction<'_>,
    current: &DurableOperation,
    next: &DurableOperation,
) -> Result<(), OperationStoreError> {
    if current.status.is_terminal() {
        return Ok(());
    }
    let current_episode = operation_episode_identity(current);
    let next_episode = operation_episode_identity(next);
    let identity_changed = current_episode != next_episode;
    let old_episode_exceeded_limit =
        identity_changed && active_operation_exceeded_limit(current, next.updated_at_ms);
    let current_episode_was_observed = if identity_changed {
        let observed = transaction
            .execute(
                "DELETE FROM orchestrator_active_operation_anomalies WHERE episode_id = ?1",
                [&current_episode],
            )
            .map_err(sqlite_error)?
            == 1;
        // Remove any stale marker from an older identity for this Operation.
        transaction
            .execute(
                "DELETE FROM orchestrator_active_operation_anomalies WHERE operation_id = ?1",
                [&current.operation_id],
            )
            .map_err(sqlite_error)?;
        observed
    } else if next.status.is_terminal() {
        transaction
            .execute(
                "DELETE FROM orchestrator_active_operation_anomalies WHERE episode_id = ?1",
                [&current_episode],
            )
            .map_err(sqlite_error)?
            == 1
    } else {
        false
    };
    if identity_changed && old_episode_exceeded_limit && !current_episode_was_observed {
        increment_counter(transaction, LONG_OPERATION_COUNTER, 1)?;
    }
    let terminal_episode_was_observed = !identity_changed && current_episode_was_observed;
    if next.status.is_terminal()
        && !terminal_episode_was_observed
        && terminal_operation_exceeded_limit(next)
    {
        increment_counter(transaction, LONG_OPERATION_COUNTER, 1)?;
    }
    Ok(())
}

pub(crate) fn increment_counter(
    connection: &rusqlite::Connection,
    key: &str,
    delta: u64,
) -> Result<(), OperationStoreError> {
    if delta == 0 {
        return Ok(());
    }
    let delta = i64::try_from(delta).map_err(|_| {
        OperationStoreError::Persistence("anomaly counter delta exceeds SQLite range".to_string())
    })?;
    let changed = connection
        .execute(
            "UPDATE orchestrator_control_plane_anomaly_counters SET counter_value = counter_value + ?2 WHERE counter_key = ?1 AND counter_value <= ?3",
            params![key, delta, i64::MAX - delta],
        )
        .map_err(sqlite_error)?;
    if changed != 1 {
        return Err(OperationStoreError::Persistence(format!(
            "anomaly counter {key} is missing or exhausted"
        )));
    }
    Ok(())
}

fn set_counter_floor(
    connection: &rusqlite::Connection,
    key: &str,
    floor: u64,
) -> Result<(), OperationStoreError> {
    let floor = i64::try_from(floor).map_err(|_| {
        OperationStoreError::Persistence("anomaly counter floor exceeds SQLite range".to_string())
    })?;
    let changed = connection
        .execute(
            "UPDATE orchestrator_control_plane_anomaly_counters SET counter_value = MAX(counter_value, ?2) WHERE counter_key = ?1",
            params![key, floor],
        )
        .map_err(sqlite_error)?;
    if changed != 1 {
        return Err(OperationStoreError::Persistence(format!(
            "anomaly counter {key} is missing"
        )));
    }
    Ok(())
}

fn read_anomaly_counters(
    connection: &rusqlite::Connection,
) -> Result<ControlPlaneAnomalyCounters, OperationStoreError> {
    let value = |key: &str| -> Result<u64, OperationStoreError> {
        let value = connection
            .query_row(
                "SELECT counter_value FROM orchestrator_control_plane_anomaly_counters WHERE counter_key = ?1",
                [key],
                |row| row.get::<_, i64>(0),
            )
            .map_err(sqlite_error)?;
        u64::try_from(value).map_err(|_| {
            OperationStoreError::Persistence(format!("anomaly counter {key} is negative"))
        })
    };
    Ok(ControlPlaneAnomalyCounters {
        expired_job_lease_transitions_total: value(EXPIRED_LEASE_COUNTER)?,
        operation_over_300_seconds_transitions_total: value(LONG_OPERATION_COUNTER)?,
    })
}

fn decode(payload: &str) -> Result<DurableOperation, OperationStoreError> {
    let operation: DurableOperation = serde_json::from_str(payload).map_err(json_error)?;
    validate_durable_operation(&operation)?;
    Ok(operation)
}

fn revision_i64(revision: u64) -> Result<i64, OperationStoreError> {
    i64::try_from(revision).map_err(|_| {
        OperationStoreError::Persistence("operation revision exceeds SQLite range".to_string())
    })
}

fn status(operation: &DurableOperation) -> String {
    serde_json::to_value(operation.status)
        .ok()
        .and_then(|value| value.as_str().map(str::to_string))
        .unwrap_or_else(|| "UNKNOWN".to_string())
}

fn storage_error(error: crate::StorageError) -> OperationStoreError {
    OperationStoreError::Persistence(error.to_string())
}

fn sqlite_error(error: rusqlite::Error) -> OperationStoreError {
    OperationStoreError::Persistence(error.to_string())
}

fn json_error(error: serde_json::Error) -> OperationStoreError {
    OperationStoreError::Persistence(error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use orchestrator_control_plane::{
        DurableOperationMode, DurableOperationStatus, JobKind, MemoryJobStore,
        OPERATION_SCHEMA_VERSION, OperationCoordinator, PlanOperation, PlannedJob,
    };
    use serde_json::json;
    use tempfile::tempdir;

    fn plan() -> PlanOperation {
        PlanOperation {
            operation_id: "op-1".to_string(),
            action: "release.install".to_string(),
            target_type: "Release".to_string(),
            target_id: "judge".to_string(),
            request: json!({}),
            jobs: vec![PlannedJob {
                step_id: "install".to_string(),
                node_id: "node-1".to_string(),
                kind: JobKind::Install,
                depends_on: vec![],
                condition: Default::default(),
                payload: json!({"spec": {}}),
                max_attempts: 3,
            }],
        }
    }

    fn operation(
        operation_id: &str,
        status: DurableOperationStatus,
        updated_at_ms: i64,
        started_at_ms: Option<i64>,
    ) -> DurableOperation {
        DurableOperation {
            schema_version: OPERATION_SCHEMA_VERSION,
            operation_id: operation_id.to_string(),
            mode: DurableOperationMode::Apply,
            rollback_of_operation_id: None,
            action: "deployment.start".to_string(),
            target_type: "Deployment".to_string(),
            target_id: "deployment-1".to_string(),
            status,
            request: json!({}),
            plan_sha256: "0".repeat(64),
            planned_jobs: vec![],
            job_bindings: vec![],
            pending_step_ids: vec![],
            attention_job_ids: vec![],
            generation: 1,
            revision: 1,
            result: json!({}),
            error_message: String::new(),
            created_at_ms: 1,
            updated_at_ms,
            confirmed_at_ms: Some(1),
            started_at_ms,
            finished_at_ms: None,
        }
    }

    fn finish(operation: &DurableOperation, finished_at_ms: i64) -> DurableOperation {
        let mut next = operation.clone();
        next.status = DurableOperationStatus::Succeeded;
        next.revision += 1;
        next.updated_at_ms = finished_at_ms;
        next.finished_at_ms = Some(finished_at_ms);
        next
    }

    #[test]
    fn operation_cas_and_recovery_survive_restart() {
        let directory = tempdir().unwrap();
        let database = directory.path().join("db.sqlite");
        {
            let storage = SqliteOrchestratorStore::open(&database).unwrap();
            let mut operations = SqliteOperationStore::new(storage.clone());
            let mut jobs = MemoryJobStore::default();
            let mut coordinator = OperationCoordinator::new(&mut operations, &mut jobs);
            coordinator.plan(plan(), 1).unwrap();
            coordinator.confirm("op-1", 2).unwrap();
            let running = coordinator.enqueue("op-1", 3).unwrap();
            assert_eq!(running.status, DurableOperationStatus::Running);
        }
        let storage = SqliteOrchestratorStore::open(&database).unwrap();
        let operations = SqliteOperationStore::new(storage);
        let recovered = operations.recoverable().unwrap();
        assert_eq!(recovered.len(), 1);
        assert_eq!(recovered[0].operation_id, "op-1");
    }

    #[test]
    fn active_observation_and_terminal_commit_count_an_episode_exactly_once() {
        let directory = tempdir().unwrap();
        let storage = SqliteOrchestratorStore::open(directory.path().join("db.sqlite")).unwrap();
        let mut operations = SqliteOperationStore::new(storage);
        let running = operation("op-observed", DurableOperationStatus::Running, 1, Some(1));
        operations.create(running.clone()).unwrap();

        let first = operations
            .observe_active_operation_anomalies(std::slice::from_ref(&running), 300_002)
            .unwrap();
        let repeated = operations
            .observe_active_operation_anomalies(std::slice::from_ref(&running), 300_003)
            .unwrap();
        assert_eq!(first.operation_over_300_seconds_transitions_total, 1);
        assert_eq!(repeated.operation_over_300_seconds_transitions_total, 1);

        operations
            .compare_and_swap(1, finish(&running, 300_004))
            .unwrap();
        assert_eq!(
            operations
                .anomaly_counters()
                .unwrap()
                .operation_over_300_seconds_transitions_total,
            1
        );
        let marker_count: i64 = operations
            .storage()
            .connection()
            .unwrap()
            .query_row(
                "SELECT COUNT(*) FROM orchestrator_active_operation_anomalies",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(marker_count, 0);
    }

    #[test]
    fn terminal_commits_are_counted_independent_of_finish_timestamp_order() {
        let directory = tempdir().unwrap();
        let storage = SqliteOrchestratorStore::open(directory.path().join("db.sqlite")).unwrap();
        let mut operations = SqliteOperationStore::new(storage);
        let later = operation("op-later", DurableOperationStatus::Running, 1, Some(1));
        let earlier = operation("op-earlier", DurableOperationStatus::Running, 1, Some(1));
        operations.create(later.clone()).unwrap();
        operations.create(earlier.clone()).unwrap();

        operations
            .compare_and_swap(1, finish(&later, 500_000))
            .unwrap();
        operations
            .compare_and_swap(1, finish(&earlier, 400_000))
            .unwrap();
        assert_eq!(
            operations
                .anomaly_counters()
                .unwrap()
                .operation_over_300_seconds_transitions_total,
            2
        );
    }

    #[test]
    fn identity_change_removes_old_marker_and_new_episode_counts_once() {
        let directory = tempdir().unwrap();
        let storage = SqliteOrchestratorStore::open(directory.path().join("db.sqlite")).unwrap();
        let mut operations = SqliteOperationStore::new(storage);
        let enqueuing = operation("op-identity", DurableOperationStatus::Enqueuing, 1, None);
        operations.create(enqueuing.clone()).unwrap();
        operations
            .observe_active_operation_anomalies(std::slice::from_ref(&enqueuing), 300_002)
            .unwrap();

        let mut running = enqueuing.clone();
        running.status = DurableOperationStatus::Running;
        running.revision = 2;
        running.started_at_ms = Some(300_002);
        running.updated_at_ms = 300_002;
        operations.compare_and_swap(1, running.clone()).unwrap();
        let marker_count: i64 = operations
            .storage()
            .connection()
            .unwrap()
            .query_row(
                "SELECT COUNT(*) FROM orchestrator_active_operation_anomalies",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(marker_count, 0);

        operations
            .observe_active_operation_anomalies(std::slice::from_ref(&running), 600_003)
            .unwrap();
        operations
            .compare_and_swap(2, finish(&running, 600_004))
            .unwrap();
        assert_eq!(
            operations
                .anomaly_counters()
                .unwrap()
                .operation_over_300_seconds_transitions_total,
            2
        );
        let marker_count: i64 = operations
            .storage()
            .connection()
            .unwrap()
            .query_row(
                "SELECT COUNT(*) FROM orchestrator_active_operation_anomalies",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(marker_count, 0);
    }

    #[test]
    fn identity_change_counts_an_unobserved_old_episode_only_after_the_exact_limit() {
        let directory = tempdir().unwrap();
        let storage = SqliteOrchestratorStore::open(directory.path().join("db.sqlite")).unwrap();
        let mut operations = SqliteOperationStore::new(storage);

        let exact = operation("op-exact", DurableOperationStatus::Running, 1, Some(1));
        operations.create(exact.clone()).unwrap();
        let mut exact_next = exact.clone();
        exact_next.revision = 2;
        exact_next.generation = 2;
        exact_next.updated_at_ms = 300_001;
        exact_next.started_at_ms = Some(300_001);
        operations.compare_and_swap(1, exact_next).unwrap();
        assert_eq!(
            operations
                .anomaly_counters()
                .unwrap()
                .operation_over_300_seconds_transitions_total,
            0
        );

        let over = operation("op-over", DurableOperationStatus::Running, 1, Some(1));
        operations.create(over.clone()).unwrap();
        let mut over_next = over.clone();
        over_next.revision = 2;
        over_next.generation = 2;
        over_next.updated_at_ms = 300_002;
        over_next.started_at_ms = Some(300_002);
        operations.compare_and_swap(1, over_next).unwrap();
        assert_eq!(
            operations
                .anomaly_counters()
                .unwrap()
                .operation_over_300_seconds_transitions_total,
            1
        );

        let observed = operation(
            "op-observed-old",
            DurableOperationStatus::Running,
            1,
            Some(1),
        );
        operations.create(observed.clone()).unwrap();
        operations
            .observe_active_operation_anomalies(std::slice::from_ref(&observed), 300_002)
            .unwrap();
        let mut observed_next = observed.clone();
        observed_next.revision = 2;
        observed_next.generation = 2;
        observed_next.updated_at_ms = 300_003;
        observed_next.started_at_ms = Some(300_003);
        operations.compare_and_swap(1, observed_next).unwrap();
        assert_eq!(
            operations
                .anomaly_counters()
                .unwrap()
                .operation_over_300_seconds_transitions_total,
            2
        );
        let marker_count: i64 = operations
            .storage()
            .connection()
            .unwrap()
            .query_row(
                "SELECT COUNT(*) FROM orchestrator_active_operation_anomalies",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(marker_count, 0);
    }

    #[test]
    fn active_candidate_query_never_materializes_retained_terminal_history() {
        let directory = tempdir().unwrap();
        let storage = SqliteOrchestratorStore::open(directory.path().join("db.sqlite")).unwrap();
        let mut connection = storage.connection().unwrap();
        let transaction = connection.transaction().unwrap();
        for index in 0..2_048 {
            transaction
                .execute(
                    "INSERT INTO orchestrator_durable_operations(operation_id, revision, status, payload, created_at_ms, updated_at_ms) VALUES (?1, 1, 'SUCCEEDED', '{}', 1, 1)",
                    [format!("retained-terminal-{index}")],
                )
                .unwrap();
        }
        transaction.commit().unwrap();
        let mut operations = SqliteOperationStore::new(storage);
        let active = operation("only-active", DurableOperationStatus::Running, 1, Some(1));
        operations.create(active.clone()).unwrap();

        assert_eq!(operations.anomaly_candidates().unwrap(), vec![active]);
    }
}
