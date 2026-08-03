use crate::operations::{
    ControlPlaneAnomalyCounters, EXPIRED_LEASE_COUNTER, LONG_OPERATION_COUNTER,
    active_operation_exceeded_limit, operation_episode_identity, terminal_operation_exceeded_limit,
};
use crate::{PostgresOrchestratorStore, PostgresPool};
use orchestrator_control_plane::{
    DurableOperation, OperationRepository, OperationStoreError, validate_durable_operation,
    validate_durable_operation_update,
};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone)]
pub struct PostgresOperationStore {
    pool: PostgresPool,
}

impl PostgresOperationStore {
    pub fn new(storage: PostgresOrchestratorStore) -> Self {
        Self {
            pool: storage.pool().clone(),
        }
    }

    pub fn from_pool(pool: PostgresPool) -> Self {
        Self { pool }
    }

    pub fn pool(&self) -> &PostgresPool {
        &self.pool
    }

    pub fn anomaly_counters(&self) -> Result<ControlPlaneAnomalyCounters, OperationStoreError> {
        let mut connection = self.pool.connection().map_err(postgres_error)?;
        read_anomaly_counters(&mut *connection)
    }

    pub fn migrate_legacy_anomaly_state(
        &self,
        expired_leases: u64,
        long_operations: u64,
        expired_lease_episodes: &BTreeMap<String, String>,
        long_operation_episodes: &BTreeSet<String>,
    ) -> Result<ControlPlaneAnomalyCounters, OperationStoreError> {
        let mut connection = self.pool.connection().map_err(postgres_error)?;
        let mut transaction = connection.transaction().map_err(database_error)?;
        read_anomaly_counters(&mut transaction)?;
        set_counter_floor(&mut transaction, EXPIRED_LEASE_COUNTER, expired_leases)?;
        set_counter_floor(&mut transaction, LONG_OPERATION_COUNTER, long_operations)?;
        for (job_id, expected_identity) in expired_lease_episodes {
            let row = transaction
                .query_opt(
                    "SELECT payload::text FROM orchestrator_jobs WHERE job_id = $1 FOR UPDATE",
                    &[job_id],
                )
                .map_err(database_error)?;
            let Some(row) = row else { continue };
            let job: orchestrator_control_plane::Job =
                serde_json::from_str(&row.get::<_, String>(0)).map_err(json_error)?;
            if crate::jobs::lease_episode_identity(&job).as_ref() != Some(expected_identity) {
                continue;
            }
            transaction
                .execute(
                    "INSERT INTO orchestrator_active_expired_lease_anomalies(job_id, lease_identity) VALUES ($1, $2) ON CONFLICT(job_id) DO UPDATE SET lease_identity = excluded.lease_identity",
                    &[job_id, expected_identity],
                )
                .map_err(database_error)?;
        }
        for expected_episode in long_operation_episodes {
            let operation_id = expected_episode.rsplitn(3, ':').nth(2).unwrap_or_default();
            let row = transaction
                .query_opt(
                    "SELECT payload::text FROM orchestrator_durable_operations WHERE operation_id = $1 FOR UPDATE",
                    &[&operation_id],
                )
                .map_err(database_error)?;
            let Some(row) = row else { continue };
            let operation = decode(&row.get::<_, String>(0))?;
            if operation_episode_identity(&operation) != *expected_episode
                || !matches!(
                    operation.status,
                    orchestrator_control_plane::DurableOperationStatus::Enqueuing
                        | orchestrator_control_plane::DurableOperationStatus::Running
                        | orchestrator_control_plane::DurableOperationStatus::Cancelling
                )
            {
                continue;
            }
            let generation = revision_i64(u64::from(operation.generation))?;
            transaction
                .execute(
                    "INSERT INTO orchestrator_active_operation_anomalies(episode_id, operation_id, generation, started_at_ms) VALUES ($1, $2, $3, $4) ON CONFLICT(episode_id) DO NOTHING",
                    &[expected_episode, &operation.operation_id, &generation, &operation.started_at_ms],
                )
                .map_err(database_error)?;
        }
        let counters = read_anomaly_counters(&mut transaction)?;
        transaction.commit().map_err(database_error)?;
        Ok(counters)
    }

    pub fn observe_active_operation_anomalies(
        &self,
        candidates: &[DurableOperation],
        now_ms: i64,
    ) -> Result<ControlPlaneAnomalyCounters, OperationStoreError> {
        let mut ordered = candidates.iter().collect::<Vec<_>>();
        ordered.sort_by(|left, right| left.operation_id.cmp(&right.operation_id));
        let mut connection = self.pool.connection().map_err(postgres_error)?;
        let mut transaction = connection.transaction().map_err(database_error)?;
        let mut inserted = 0_u64;
        for candidate in ordered {
            let row = transaction
                .query_opt(
                    "SELECT payload::text FROM orchestrator_durable_operations WHERE operation_id = $1 FOR UPDATE",
                    &[&candidate.operation_id],
                )
                .map_err(database_error)?;
            let Some(row) = row else {
                continue;
            };
            let current = decode(&row.get::<_, String>(0))?;
            if operation_episode_identity(&current) != operation_episode_identity(candidate)
                || !active_operation_exceeded_limit(&current, now_ms)
            {
                continue;
            }
            let generation = revision_i64(u64::from(current.generation))?;
            inserted = inserted.saturating_add(
                transaction
                    .execute(
                        "INSERT INTO orchestrator_active_operation_anomalies(episode_id, operation_id, generation, started_at_ms) VALUES ($1, $2, $3, $4) ON CONFLICT(episode_id) DO NOTHING",
                        &[
                            &operation_episode_identity(&current),
                            &current.operation_id,
                            &generation,
                            &current.started_at_ms,
                        ],
                    )
                    .map_err(database_error)?,
            );
        }
        increment_counter(&mut transaction, LONG_OPERATION_COUNTER, inserted)?;
        let counters = read_anomaly_counters(&mut transaction)?;
        transaction.commit().map_err(database_error)?;
        Ok(counters)
    }

    pub fn anomaly_candidates(&self) -> Result<Vec<DurableOperation>, OperationStoreError> {
        self.read_operations(
            "SELECT payload::text FROM orchestrator_durable_operations WHERE status IN ('ENQUEUING', 'RUNNING', 'CANCELLING') ORDER BY operation_id",
        )
    }
}

impl OperationRepository for PostgresOperationStore {
    fn create(
        &mut self,
        operation: DurableOperation,
    ) -> Result<DurableOperation, OperationStoreError> {
        validate_durable_operation(&operation)?;
        let payload = serde_json::to_string(&operation).map_err(json_error)?;
        let revision = revision_i64(operation.revision)?;
        let changed = self
            .pool
            .with_client(|client| {
                Ok(client.execute(
                    "INSERT INTO orchestrator_durable_operations(operation_id, revision, status, payload, created_at_ms, updated_at_ms) VALUES ($1, $2, $3, $4::text::jsonb, $5, $6) ON CONFLICT(operation_id) DO NOTHING",
                    &[
                        &operation.operation_id,
                        &revision,
                        &status(&operation),
                        &payload,
                        &operation.created_at_ms,
                        &operation.updated_at_ms,
                    ],
                )?)
            })
            .map_err(postgres_error)?;
        if changed != 1 {
            return Err(OperationStoreError::AlreadyExists(operation.operation_id));
        }
        Ok(operation)
    }

    fn get(&self, operation_id: &str) -> Result<Option<DurableOperation>, OperationStoreError> {
        self.pool
            .with_client(|client| {
                client
                    .query_opt(
                        "SELECT payload::text FROM orchestrator_durable_operations WHERE operation_id = $1",
                        &[&operation_id],
                    )?
                    .map(|row| {
                        serde_json::from_str::<DurableOperation>(&row.get::<_, String>(0))
                            .map_err(Into::into)
                    })
                    .transpose()
            })
            .map_err(postgres_error)?
            .map(|operation| {
                validate_durable_operation(&operation)?;
                Ok(operation)
            })
            .transpose()
    }

    fn compare_and_swap(
        &mut self,
        expected_revision: u64,
        operation: DurableOperation,
    ) -> Result<DurableOperation, OperationStoreError> {
        let mut connection = self.pool.connection().map_err(postgres_error)?;
        let mut transaction = connection.transaction().map_err(database_error)?;
        let row = transaction
            .query_opt(
                "SELECT payload::text FROM orchestrator_durable_operations WHERE operation_id = $1 FOR UPDATE",
                &[&operation.operation_id],
            )
            .map_err(database_error)?
            .ok_or_else(|| OperationStoreError::NotFound(operation.operation_id.clone()))?;
        let current = decode(&row.get::<_, String>(0))?;
        validate_durable_operation_update(&current, expected_revision, &operation)?;
        record_anomaly_transition(&mut transaction, &current, &operation)?;
        let payload = serde_json::to_string(&operation).map_err(json_error)?;
        let revision = revision_i64(operation.revision)?;
        let expected = revision_i64(expected_revision)?;
        let changed = transaction
            .execute(
                "UPDATE orchestrator_durable_operations SET revision = $2, status = $3, payload = $4::text::jsonb, updated_at_ms = $5 WHERE operation_id = $1 AND revision = $6",
                &[
                    &operation.operation_id,
                    &revision,
                    &status(&operation),
                    &payload,
                    &operation.updated_at_ms,
                    &expected,
                ],
            )
            .map_err(database_error)?;
        if changed != 1 {
            let actual: i64 = transaction
                .query_one(
                    "SELECT revision FROM orchestrator_durable_operations WHERE operation_id = $1",
                    &[&operation.operation_id],
                )
                .map_err(database_error)?
                .get(0);
            return Err(OperationStoreError::RevisionConflict {
                expected: expected_revision,
                actual: u64::try_from(actual).unwrap_or_default(),
            });
        }
        transaction.commit().map_err(database_error)?;
        Ok(operation)
    }

    fn recoverable(&self) -> Result<Vec<DurableOperation>, OperationStoreError> {
        self.read_operations(
            "SELECT payload::text FROM orchestrator_durable_operations WHERE status IN ('CONFIRMED', 'ENQUEUING', 'RUNNING', 'CANCELLING') ORDER BY updated_at_ms, operation_id",
        )
    }

    fn list(&self) -> Result<Vec<DurableOperation>, OperationStoreError> {
        self.read_operations(
            "SELECT payload::text FROM orchestrator_durable_operations ORDER BY created_at_ms DESC, operation_id DESC",
        )
    }
}

fn record_anomaly_transition(
    transaction: &mut r2d2_postgres::postgres::Transaction<'_>,
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
                "DELETE FROM orchestrator_active_operation_anomalies WHERE episode_id = $1",
                &[&current_episode],
            )
            .map_err(database_error)?
            == 1;
        // Remove any stale marker from an older identity for this Operation.
        transaction
            .execute(
                "DELETE FROM orchestrator_active_operation_anomalies WHERE operation_id = $1",
                &[&current.operation_id],
            )
            .map_err(database_error)?;
        observed
    } else if next.status.is_terminal() {
        transaction
            .execute(
                "DELETE FROM orchestrator_active_operation_anomalies WHERE episode_id = $1",
                &[&current_episode],
            )
            .map_err(database_error)?
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
    transaction: &mut impl r2d2_postgres::postgres::GenericClient,
    key: &str,
    delta: u64,
) -> Result<(), OperationStoreError> {
    if delta == 0 {
        return Ok(());
    }
    let delta = i64::try_from(delta).map_err(|_| {
        OperationStoreError::Persistence(
            "anomaly counter delta exceeds PostgreSQL BIGINT".to_string(),
        )
    })?;
    let changed = transaction
        .execute(
            "UPDATE orchestrator_control_plane_anomaly_counters SET counter_value = counter_value + $2 WHERE counter_key = $1 AND counter_value <= $3",
            &[&key, &delta, &(i64::MAX - delta)],
        )
        .map_err(database_error)?;
    if changed != 1 {
        return Err(OperationStoreError::Persistence(format!(
            "anomaly counter {key} is missing or exhausted"
        )));
    }
    Ok(())
}

fn set_counter_floor(
    transaction: &mut impl r2d2_postgres::postgres::GenericClient,
    key: &str,
    floor: u64,
) -> Result<(), OperationStoreError> {
    let floor = i64::try_from(floor).map_err(|_| {
        OperationStoreError::Persistence(
            "anomaly counter floor exceeds PostgreSQL BIGINT".to_string(),
        )
    })?;
    let changed = transaction
        .execute(
            "UPDATE orchestrator_control_plane_anomaly_counters SET counter_value = GREATEST(counter_value, $2) WHERE counter_key = $1",
            &[&key, &floor],
        )
        .map_err(database_error)?;
    if changed != 1 {
        return Err(OperationStoreError::Persistence(format!(
            "anomaly counter {key} is missing"
        )));
    }
    Ok(())
}

fn read_anomaly_counters(
    client: &mut impl r2d2_postgres::postgres::GenericClient,
) -> Result<ControlPlaneAnomalyCounters, OperationStoreError> {
    let rows = client.query(
        "SELECT counter_key, counter_value FROM orchestrator_control_plane_anomaly_counters WHERE counter_key IN ($1, $2)",
        &[&EXPIRED_LEASE_COUNTER, &LONG_OPERATION_COUNTER],
    ).map_err(database_error)?;
    parse_anomaly_counter_rows(
        rows.into_iter()
            .map(|row| (row.get::<_, String>(0), row.get::<_, i64>(1))),
    )
}

fn parse_anomaly_counter_rows(
    rows: impl IntoIterator<Item = (String, i64)>,
) -> Result<ControlPlaneAnomalyCounters, OperationStoreError> {
    let mut counters = ControlPlaneAnomalyCounters::default();
    let mut expired_seen = false;
    let mut operation_seen = false;
    for (key, value) in rows {
        let value = u64::try_from(value).map_err(|_| {
            OperationStoreError::Persistence(format!("anomaly counter {key} is negative"))
        })?;
        match key.as_str() {
            EXPIRED_LEASE_COUNTER => {
                expired_seen = true;
                counters.expired_job_lease_transitions_total = value;
            }
            LONG_OPERATION_COUNTER => {
                operation_seen = true;
                counters.operation_over_300_seconds_transitions_total = value;
            }
            _ => {}
        }
    }
    if !expired_seen || !operation_seen {
        return Err(OperationStoreError::Persistence(
            "required control-plane anomaly counter row is missing".to_string(),
        ));
    }
    Ok(counters)
}

impl PostgresOperationStore {
    fn read_operations(&self, sql: &str) -> Result<Vec<DurableOperation>, OperationStoreError> {
        let payloads = self
            .pool
            .with_client(|client| {
                Ok(client
                    .query(sql, &[])?
                    .into_iter()
                    .map(|row| row.get::<_, String>(0))
                    .collect::<Vec<_>>())
            })
            .map_err(postgres_error)?;
        payloads
            .into_iter()
            .map(|payload| decode(&payload))
            .collect()
    }
}

fn decode(payload: &str) -> Result<DurableOperation, OperationStoreError> {
    let operation: DurableOperation = serde_json::from_str(payload).map_err(json_error)?;
    validate_durable_operation(&operation)?;
    Ok(operation)
}

fn revision_i64(revision: u64) -> Result<i64, OperationStoreError> {
    i64::try_from(revision).map_err(|_| {
        OperationStoreError::Persistence(
            "operation revision exceeds PostgreSQL BIGINT range".to_string(),
        )
    })
}

fn status(operation: &DurableOperation) -> String {
    serde_json::to_value(operation.status)
        .ok()
        .and_then(|value| value.as_str().map(str::to_string))
        .unwrap_or_else(|| "UNKNOWN".to_string())
}

fn postgres_error(error: crate::PostgresError) -> OperationStoreError {
    OperationStoreError::Persistence(error.to_string())
}

fn database_error(error: r2d2_postgres::postgres::Error) -> OperationStoreError {
    OperationStoreError::Persistence(error.to_string())
}

fn json_error(error: serde_json::Error) -> OperationStoreError {
    OperationStoreError::Persistence(error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn anomaly_counter_rows_fail_closed_when_missing_or_negative() {
        let missing =
            parse_anomaly_counter_rows([(EXPIRED_LEASE_COUNTER.to_string(), 0)]).unwrap_err();
        assert!(missing.to_string().contains("row is missing"));

        let negative = parse_anomaly_counter_rows([
            (EXPIRED_LEASE_COUNTER.to_string(), -1),
            (LONG_OPERATION_COUNTER.to_string(), 0),
        ])
        .unwrap_err();
        assert!(negative.to_string().contains("is negative"));
    }
}
