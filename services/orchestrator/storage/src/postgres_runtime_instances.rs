use crate::{PostgresError, PostgresOrchestratorStore, PostgresResult, StoredRuntimeInstance};
use orchestrator_runtime::{RuntimeDesiredState, RuntimeObservedState};

impl PostgresOrchestratorStore {
    pub fn put_runtime_instance(&self, value: &StoredRuntimeInstance) -> PostgresResult<()> {
        value
            .validate()
            .map_err(|error| PostgresError::Invariant(error.to_string()))?;
        let payload = serde_json::to_string(value)?;
        self.pool().with_client(|client| {
            client.execute(
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
            Ok(client.execute(
                "DELETE FROM orchestrator_runtime_instances WHERE deployment_id = $1",
                &[&deployment_id],
            )? > 0)
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

fn decode(payload: &str) -> PostgresResult<StoredRuntimeInstance> {
    let value: StoredRuntimeInstance = serde_json::from_str(payload)?;
    value
        .validate()
        .map_err(|error| PostgresError::Invariant(error.to_string()))?;
    Ok(value)
}

fn desired_state(state: &RuntimeDesiredState) -> &'static str {
    match state {
        RuntimeDesiredState::Running => "RUNNING",
        RuntimeDesiredState::Stopped => "STOPPED",
        RuntimeDesiredState::Removed => "REMOVED",
    }
}

fn observed_state(state: &RuntimeObservedState) -> &'static str {
    match state {
        RuntimeObservedState::Created => "CREATED",
        RuntimeObservedState::Running => "RUNNING",
        RuntimeObservedState::Stopped => "STOPPED",
        RuntimeObservedState::Exited => "EXITED",
        RuntimeObservedState::Missing => "MISSING",
        RuntimeObservedState::Unknown => "UNKNOWN",
    }
}
