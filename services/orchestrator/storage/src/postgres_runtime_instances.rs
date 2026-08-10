use crate::{PostgresError, PostgresOrchestratorStore, PostgresResult, StoredRuntimeInstance};
use orchestrator_runtime::{RuntimeDesiredState, RuntimeObservedState};
use std::collections::BTreeSet;

const RUNTIME_DEPLOYMENT_LOCK_PREFIX: &str = "orchestrator-runtime-deployment:";

impl PostgresOrchestratorStore {
    pub fn put_runtime_instance(&self, value: &StoredRuntimeInstance) -> PostgresResult<()> {
        value
            .validate()
            .map_err(|error| PostgresError::Invariant(error.to_string()))?;
        let payload = serde_json::to_string(value)?;
        self.pool().with_client(|client| {
            let mut transaction = client.transaction()?;
            let deployment_lock = format!(
                "{RUNTIME_DEPLOYMENT_LOCK_PREFIX}{}",
                value.instance.deployment_id
            );
            advisory_xact_lock(&mut transaction, &deployment_lock)?;

            // Runtime-set mutations and complete Node reports share the raw
            // node-id advisory lock. PostgreSQL cannot predicate-lock an empty
            // SELECT FOR UPDATE result, so this lock is what prevents a new
            // managed deployment from appearing between a report's exact-set
            // check and commit. The deployment lock above serializes moves and
            // lets us discover the old node before taking both node locks.
            let previous_node = transaction
                .query_opt(
                    "SELECT node_id FROM orchestrator_runtime_instances WHERE deployment_id = $1",
                    &[&value.instance.deployment_id],
                )?
                .map(|row| row.get::<_, String>(0));
            let mut node_locks = BTreeSet::from([value.node_id.clone()]);
            if let Some(previous_node) = previous_node {
                node_locks.insert(previous_node);
            }
            for node_id in node_locks {
                advisory_xact_lock(&mut transaction, &node_id)?;
            }

            transaction.execute(
                "INSERT INTO orchestrator_runtime_instances(deployment_id, node_id, service_id, desired_state, observed_state, payload) VALUES ($1, $2, $3, $4, $5, $6::text::jsonb) ON CONFLICT(deployment_id) DO UPDATE SET node_id = excluded.node_id, service_id = excluded.service_id, desired_state = excluded.desired_state, observed_state = excluded.observed_state, payload = excluded.payload, updated_at = clock_timestamp()",
                &[
                    &value.instance.deployment_id,
                    &value.node_id,
                    &value.instance.service_id,
                    &desired_state(&value.instance.desired_state),
                    &observed_state(&value.instance.observed_state),
                    &payload,
                ],
            )?;
            transaction.commit()?;
            Ok(())
        })
    }

    pub fn runtime_instance(
        &self,
        deployment_id: &str,
    ) -> PostgresResult<Option<StoredRuntimeInstance>> {
        self.pool().with_client(|client| {
            client
                .query_opt(
                    "SELECT payload::text FROM orchestrator_runtime_instances WHERE deployment_id = $1",
                    &[&deployment_id],
                )?
                .map(|row| decode(&row.get::<_, String>(0)))
                .transpose()
        })
    }

    pub fn runtime_instances(
        &self,
        node_id: Option<&str>,
    ) -> PostgresResult<Vec<StoredRuntimeInstance>> {
        self.pool().with_client(|client| {
            let rows = match node_id {
                Some(node_id) => client.query(
                    "SELECT payload::text FROM orchestrator_runtime_instances WHERE node_id = $1 ORDER BY service_id, deployment_id",
                    &[&node_id],
                )?,
                None => client.query(
                    "SELECT payload::text FROM orchestrator_runtime_instances ORDER BY node_id, service_id, deployment_id",
                    &[],
                )?,
            };
            rows.into_iter()
                .map(|row| decode(&row.get::<_, String>(0)))
                .collect()
        })
    }

    pub fn delete_runtime_instance(&self, deployment_id: &str) -> PostgresResult<bool> {
        self.pool().with_client(|client| {
            let mut transaction = client.transaction()?;
            let deployment_lock = format!("{RUNTIME_DEPLOYMENT_LOCK_PREFIX}{deployment_id}");
            advisory_xact_lock(&mut transaction, &deployment_lock)?;
            let Some(node_id) = transaction
                .query_opt(
                    "SELECT node_id FROM orchestrator_runtime_instances WHERE deployment_id = $1",
                    &[&deployment_id],
                )?
                .map(|row| row.get::<_, String>(0))
            else {
                transaction.commit()?;
                return Ok(false);
            };
            advisory_xact_lock(&mut transaction, &node_id)?;
            let deleted = transaction.execute(
                "DELETE FROM orchestrator_runtime_instances WHERE deployment_id = $1",
                &[&deployment_id],
            )? > 0;
            transaction.commit()?;
            Ok(deleted)
        })
    }

    pub fn replace_runtime_instance(
        &self,
        replaced_deployment_id: &str,
        value: &StoredRuntimeInstance,
    ) -> PostgresResult<()> {
        value
            .validate()
            .map_err(|error| PostgresError::Invariant(error.to_string()))?;
        if replaced_deployment_id.trim().is_empty()
            || replaced_deployment_id == value.instance.deployment_id
        {
            return Err(PostgresError::Invariant(
                "runtime replacement requires distinct non-empty deployment ids".to_string(),
            ));
        }
        let payload = serde_json::to_string(value)?;
        self.pool().with_client(|client| {
            let mut transaction = client.transaction()?;
            let deployment_locks = BTreeSet::from([
                format!("{RUNTIME_DEPLOYMENT_LOCK_PREFIX}{replaced_deployment_id}"),
                format!(
                    "{RUNTIME_DEPLOYMENT_LOCK_PREFIX}{}",
                    value.instance.deployment_id
                ),
            ]);
            for deployment_lock in deployment_locks {
                advisory_xact_lock(&mut transaction, &deployment_lock)?;
            }

            let mut node_locks = BTreeSet::from([value.node_id.clone()]);
            for deployment_id in [replaced_deployment_id, &value.instance.deployment_id] {
                if let Some(row) = transaction.query_opt(
                    "SELECT node_id FROM orchestrator_runtime_instances WHERE deployment_id = $1",
                    &[&deployment_id],
                )? {
                    node_locks.insert(row.get::<_, String>(0));
                }
            }
            for node_id in node_locks {
                advisory_xact_lock(&mut transaction, &node_id)?;
            }

            transaction.execute(
                "DELETE FROM orchestrator_runtime_instances WHERE deployment_id = $1",
                &[&replaced_deployment_id],
            )?;
            transaction.execute(
                "INSERT INTO orchestrator_runtime_instances(deployment_id, node_id, service_id, desired_state, observed_state, payload) VALUES ($1, $2, $3, $4, $5, $6::text::jsonb) ON CONFLICT(deployment_id) DO UPDATE SET node_id = excluded.node_id, service_id = excluded.service_id, desired_state = excluded.desired_state, observed_state = excluded.observed_state, payload = excluded.payload, updated_at = clock_timestamp()",
                &[
                    &value.instance.deployment_id,
                    &value.node_id,
                    &value.instance.service_id,
                    &desired_state(&value.instance.desired_state),
                    &observed_state(&value.instance.observed_state),
                    &payload,
                ],
            )?;
            transaction.commit()?;
            Ok(())
        })
    }
}

fn advisory_xact_lock(
    transaction: &mut r2d2_postgres::postgres::Transaction<'_>,
    key: &str,
) -> Result<(), r2d2_postgres::postgres::Error> {
    transaction.query_one(
        "SELECT pg_advisory_xact_lock(hashtextextended($1, 0))",
        &[&key],
    )?;
    Ok(())
}

fn decode(payload: &str) -> PostgresResult<StoredRuntimeInstance> {
    let value: StoredRuntimeInstance = serde_json::from_value(
        crate::runtime_instances::normalize_legacy_runtime_payload(payload)?,
    )?;
    value
        .validate()
        .map_err(|error| PostgresError::Invariant(error.to_string()))?;
    Ok(value)
}

pub(crate) fn desired_state(state: &RuntimeDesiredState) -> &'static str {
    match state {
        RuntimeDesiredState::Running => "RUNNING",
        RuntimeDesiredState::Stopped => "STOPPED",
        RuntimeDesiredState::Removed => "REMOVED",
    }
}

pub(crate) fn observed_state(state: &RuntimeObservedState) -> &'static str {
    match state {
        RuntimeObservedState::Created => "CREATED",
        RuntimeObservedState::Running => "RUNNING",
        RuntimeObservedState::Stopped => "STOPPED",
        RuntimeObservedState::Exited => "EXITED",
        RuntimeObservedState::Missing => "MISSING",
        RuntimeObservedState::Unknown => "UNKNOWN",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn postgres_payload_decode_preserves_external_probe_contract() {
        let payload = json!({
            "node_id": "external",
            "instance": {
                "deployment_id": "deployment-external",
                "service_id": "external-api",
                "release_version": "1.0.0",
                "container_id": "",
                "artifact_digest": format!("sha256:{}", "a".repeat(64)),
                "desired_state": "RUNNING",
                "observed_state": "RUNNING",
                "health": "HEALTHY"
            },
            "management_mode": "EXTERNAL",
            "endpoint": "https://external.example",
            "external_probe_protocol": "https",
            "external_probe_health_path": "/healthz/ready",
            "last_observed_at_ms": 123456,
            "updated_at": "unix-ms:123456"
        })
        .to_string();
        let decoded = decode(&payload).unwrap();
        assert_eq!(decoded.external_probe_protocol, "https");
        assert_eq!(decoded.external_probe_health_path, "/healthz/ready");
        assert_eq!(decoded.last_observed_at_ms, 123_456);
        assert_eq!(
            decode(&serde_json::to_string(&decoded).unwrap()).unwrap(),
            decoded
        );
    }
}
