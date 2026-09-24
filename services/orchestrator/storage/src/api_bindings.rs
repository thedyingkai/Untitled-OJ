use crate::{SqliteOrchestratorStore, StorageError, StorageResult};
use orchestrator_core::{ApiBinding, ApiBindingState};
use rusqlite::{OptionalExtension, TransactionBehavior, params};
use std::collections::BTreeSet;

impl SqliteOrchestratorStore {
    pub fn put_api_binding(&self, binding: &ApiBinding) -> StorageResult<()> {
        validate(binding)?;
        let payload = serde_json::to_string(binding)?;
        self.connection()?.execute(
            "INSERT INTO orchestrator_api_bindings(binding_id, consumer_deployment_id, requirement_name, provider_deployment_id, topology_id, topology_revision_id, api_id, binding_state, payload) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9) ON CONFLICT(binding_id) DO UPDATE SET consumer_deployment_id = excluded.consumer_deployment_id, requirement_name = excluded.requirement_name, provider_deployment_id = excluded.provider_deployment_id, topology_id = excluded.topology_id, topology_revision_id = excluded.topology_revision_id, api_id = excluded.api_id, binding_state = excluded.binding_state, payload = excluded.payload, updated_at = unixepoch()",
            params![
                binding.binding_id,
                binding.consumer_deployment_id,
                binding.requirement_name,
                binding.provider_deployment_id,
                binding.topology_id,
                binding.topology_revision_id,
                binding.api_id,
                state(binding.derived_state()),
                payload,
            ],
        )?;
        Ok(())
    }

    /// Atomically replaces the full desired binding set for a deployment. This
    /// is used by Store planning and Topology revisions so removed requirements
    /// cannot survive as stale effective routes.
    pub fn replace_deployment_api_bindings(
        &self,
        deployment_id: &str,
        bindings: &[ApiBinding],
    ) -> StorageResult<()> {
        validate_set(deployment_id, bindings)?;
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        transaction.execute(
            "DELETE FROM orchestrator_api_bindings WHERE consumer_deployment_id = ?1",
            [deployment_id],
        )?;
        for binding in bindings {
            transaction.execute(
                "INSERT INTO orchestrator_api_bindings(binding_id, consumer_deployment_id, requirement_name, provider_deployment_id, topology_id, topology_revision_id, api_id, binding_state, payload) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                params![
                    binding.binding_id,
                    binding.consumer_deployment_id,
                    binding.requirement_name,
                    binding.provider_deployment_id,
                    binding.topology_id,
                    binding.topology_revision_id,
                    binding.api_id,
                    state(binding.derived_state()),
                    serde_json::to_string(binding)?,
                ],
            )?;
        }
        transaction.commit()?;
        Ok(())
    }

    /// Atomically replaces the complete projection owned by one immutable
    /// topology revision. Rows for other topologies are untouched, while the
    /// database-wide consumer/requirement unique index prevents two applied
    /// topologies from granting the same workload requirement.
    pub fn replace_topology_api_bindings(
        &self,
        topology_id: &str,
        bindings: &[ApiBinding],
    ) -> StorageResult<()> {
        validate_topology_set(topology_id, bindings)?;
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        transaction.execute(
            "DELETE FROM orchestrator_api_bindings WHERE topology_id = ?1",
            [topology_id],
        )?;
        for binding in bindings {
            transaction.execute(
                "INSERT INTO orchestrator_api_bindings(binding_id, consumer_deployment_id, requirement_name, provider_deployment_id, topology_id, topology_revision_id, api_id, binding_state, payload) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                params![
                    binding.binding_id,
                    binding.consumer_deployment_id,
                    binding.requirement_name,
                    binding.provider_deployment_id,
                    binding.topology_id,
                    binding.topology_revision_id,
                    binding.api_id,
                    state(binding.derived_state()),
                    serde_json::to_string(binding)?,
                ],
            )?;
        }
        transaction.commit()?;
        Ok(())
    }

    pub fn api_binding(&self, binding_id: &str) -> StorageResult<Option<ApiBinding>> {
        let stored = self
            .connection()?
            .query_row(
                "SELECT binding_state, payload FROM orchestrator_api_bindings WHERE binding_id = ?1",
                [binding_id],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
            )
            .optional()?;
        stored
            .map(|(stored_state, payload)| decode(&stored_state, &payload))
            .transpose()
    }

    pub fn api_bindings_for_deployment(
        &self,
        deployment_id: &str,
    ) -> StorageResult<Vec<ApiBinding>> {
        query_bindings(
            &self.connection()?,
            "SELECT binding_state, payload FROM orchestrator_api_bindings WHERE consumer_deployment_id = ?1 ORDER BY binding_id",
            deployment_id,
        )
    }

    pub fn api_bindings_for_topology(&self, topology_id: &str) -> StorageResult<Vec<ApiBinding>> {
        query_bindings(
            &self.connection()?,
            "SELECT binding_state, payload FROM orchestrator_api_bindings WHERE topology_id = ?1 ORDER BY consumer_deployment_id, binding_id",
            topology_id,
        )
    }

    pub fn delete_api_bindings_for_deployment(&self, deployment_id: &str) -> StorageResult<usize> {
        Ok(self.connection()?.execute(
            "DELETE FROM orchestrator_api_bindings WHERE consumer_deployment_id = ?1",
            [deployment_id],
        )?)
    }
}

fn query_bindings(
    connection: &rusqlite::Connection,
    sql: &str,
    parameter: &str,
) -> StorageResult<Vec<ApiBinding>> {
    let mut statement = connection.prepare(sql)?;
    let payloads = statement
        .query_map([parameter], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    payloads
        .into_iter()
        .map(|(stored_state, payload)| decode(&stored_state, &payload))
        .collect()
}

fn validate(binding: &ApiBinding) -> StorageResult<()> {
    binding
        .validate()
        .map_err(|error| StorageError::Invariant(error.to_string()))
}

fn validate_set(deployment_id: &str, bindings: &[ApiBinding]) -> StorageResult<()> {
    if deployment_id.trim().is_empty() {
        return Err(StorageError::Invariant(
            "API binding deployment_id must not be empty".to_string(),
        ));
    }
    let mut ids = BTreeSet::new();
    let mut names = BTreeSet::new();
    for binding in bindings {
        validate(binding)?;
        if binding.consumer_deployment_id != deployment_id {
            return Err(StorageError::Invariant(format!(
                "API binding {} belongs to deployment {}, expected {deployment_id}",
                binding.binding_id, binding.consumer_deployment_id
            )));
        }
        if !ids.insert(binding.binding_id.as_str())
            || !names.insert(binding.requirement_name.as_str())
        {
            return Err(StorageError::Invariant(
                "API binding set contains duplicate ids or requirement names".to_string(),
            ));
        }
    }
    Ok(())
}

fn validate_topology_set(topology_id: &str, bindings: &[ApiBinding]) -> StorageResult<()> {
    if topology_id.trim().is_empty() {
        return Err(StorageError::Invariant(
            "API binding topology_id must not be empty".to_string(),
        ));
    }
    let mut ids = BTreeSet::new();
    let mut requirements = BTreeSet::new();
    for binding in bindings {
        validate(binding)?;
        if binding.topology_id != topology_id {
            return Err(StorageError::Invariant(format!(
                "API binding {} belongs to topology {}, expected {topology_id}",
                binding.binding_id, binding.topology_id
            )));
        }
        if !ids.insert(binding.binding_id.as_str())
            || !requirements.insert((
                binding.consumer_deployment_id.as_str(),
                binding.requirement_name.as_str(),
            ))
        {
            return Err(StorageError::Invariant(
                "topology API binding set contains duplicate ids or consumer requirements"
                    .to_string(),
            ));
        }
    }
    Ok(())
}

fn decode(stored_state: &str, payload: &str) -> StorageResult<ApiBinding> {
    let binding: ApiBinding = serde_json::from_str(payload)?;
    validate(&binding)?;
    let derived = state(binding.derived_state());
    if stored_state != derived {
        return Err(StorageError::Invariant(format!(
            "API binding {} indexed state {stored_state} disagrees with derived state {derived}",
            binding.binding_id
        )));
    }
    Ok(binding)
}

fn state(state: ApiBindingState) -> &'static str {
    match state {
        ApiBindingState::Pending => "PENDING",
        ApiBindingState::Resolved => "RESOLVED",
        ApiBindingState::Active => "ACTIVE",
        ApiBindingState::Unbound => "UNBOUND",
        ApiBindingState::Revoked => "REVOKED",
        ApiBindingState::Error => "ERROR",
    }
}
