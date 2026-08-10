use crate::{SqliteOrchestratorStore, StorageError, StorageResult};
use orchestrator_legacy::{ApiBinding, ApiBindingState};
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
                state(binding.state),
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
                    state(binding.state),
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
                    state(binding.state),
                    serde_json::to_string(binding)?,
                ],
            )?;
        }
        transaction.commit()?;
        Ok(())
    }

    pub fn api_binding(&self, binding_id: &str) -> StorageResult<Option<ApiBinding>> {
        let payload = self
            .connection()?
            .query_row(
                "SELECT payload FROM orchestrator_api_bindings WHERE binding_id = ?1",
                [binding_id],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        payload.map(|payload| decode(&payload)).transpose()
    }

    pub fn api_bindings_for_deployment(
        &self,
        deployment_id: &str,
    ) -> StorageResult<Vec<ApiBinding>> {
        query_bindings(
            &self.connection()?,
            "SELECT payload FROM orchestrator_api_bindings WHERE consumer_deployment_id = ?1 ORDER BY binding_id",
            deployment_id,
        )
    }

    pub fn api_bindings_for_topology(&self, topology_id: &str) -> StorageResult<Vec<ApiBinding>> {
        query_bindings(
            &self.connection()?,
            "SELECT payload FROM orchestrator_api_bindings WHERE topology_id = ?1 ORDER BY consumer_deployment_id, binding_id",
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
        .query_map([parameter], |row| row.get::<_, String>(0))?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    payloads
        .into_iter()
        .map(|payload| decode(&payload))
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

fn decode(payload: &str) -> StorageResult<ApiBinding> {
    let binding: ApiBinding = serde_json::from_str(payload)?;
    validate(&binding)?;
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

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn binding(id: &str, requirement: &str) -> ApiBinding {
        ApiBinding {
            binding_id: id.to_string(),
            requirement_name: requirement.to_string(),
            api_id: "storage.object.get".to_string(),
            api_version: "v1".to_string(),
            consumer_deployment_id: "consumer-1".to_string(),
            consumer_service_id: "consumer".to_string(),
            consumer_node_id: "node-b".to_string(),
            consumer_endpoint: String::new(),
            provider_deployment_id: "storage-1".to_string(),
            provider_service_id: "storage".to_string(),
            provider_node_id: "node-a".to_string(),
            provider_endpoint: "10.0.0.1:8080:storage".to_string(),
            provider_path: "/objects".to_string(),
            virtual_endpoint: "/internal/apis/storage.object.get".to_string(),
            protocol: "http".to_string(),
            methods: vec!["GET".to_string()],
            auth_mode: "service".to_string(),
            provider_auth_mode: "service".to_string(),
            permission: "storage.object.read".to_string(),
            timeout_ms: Some(5000),
            topology_id: "main".to_string(),
            topology_revision_id: "revision-1".to_string(),
            link_source_endpoint: "10.0.0.2:9000:consumer".to_string(),
            link_target_endpoint: "10.0.0.1:8080:storage".to_string(),
            credential_ref: String::new(),
            credential_generation: 1,
            context_generation: 1,
            desired_state: "ACTIVE".to_string(),
            observed_state: "RESOLVED".to_string(),
            health: "UNKNOWN".to_string(),
            drift: Vec::new(),
            last_operation_id: String::new(),
            state: ApiBindingState::Resolved,
            optional: false,
            reason: String::new(),
            created_at: "unix-ms:1".to_string(),
            updated_at: "unix-ms:1".to_string(),
        }
    }

    #[test]
    fn binding_set_is_atomic_and_survives_restart() {
        let directory = tempdir().unwrap();
        let database = directory.path().join("orchestrator.db");
        let first = binding("binding-1", "STORAGE_GET");
        {
            let store = SqliteOrchestratorStore::open(&database).unwrap();
            store
                .replace_deployment_api_bindings("consumer-1", std::slice::from_ref(&first))
                .unwrap();
            let second = binding("binding-2", "STORAGE_READ");
            store
                .replace_deployment_api_bindings("consumer-1", std::slice::from_ref(&second))
                .unwrap();
            assert_eq!(
                store.api_bindings_for_deployment("consumer-1").unwrap(),
                vec![second]
            );
        }
        let reopened = SqliteOrchestratorStore::open(database).unwrap();
        assert!(reopened.api_binding("binding-1").unwrap().is_none());
        assert_eq!(reopened.api_bindings_for_topology("main").unwrap().len(), 1);
    }

    #[test]
    fn topology_replace_rejects_duplicate_consumer_requirement_without_losing_prior_set() {
        let directory = tempdir().unwrap();
        let store =
            SqliteOrchestratorStore::open(directory.path().join("orchestrator.db")).unwrap();
        let original = binding("binding-original", "STORAGE_GET");
        store
            .replace_topology_api_bindings("main", std::slice::from_ref(&original))
            .unwrap();
        let duplicate = binding("binding-duplicate", "STORAGE_GET");
        let error = store
            .replace_topology_api_bindings("main", &[original.clone(), duplicate])
            .expect_err("duplicate consumer requirement must fail before mutation");
        assert!(matches!(error, StorageError::Invariant(_)));
        assert_eq!(
            store.api_bindings_for_topology("main").unwrap(),
            vec![original]
        );
    }

    #[test]
    fn database_unique_index_rejects_cross_topology_consumer_requirement() {
        let directory = tempdir().unwrap();
        let store =
            SqliteOrchestratorStore::open(directory.path().join("orchestrator.db")).unwrap();
        let first = binding("binding-main", "STORAGE_GET");
        store.put_api_binding(&first).unwrap();
        let mut second = binding("binding-secondary", "STORAGE_GET");
        second.topology_id = "secondary".to_string();
        second.topology_revision_id = "revision-2".to_string();
        assert!(store.put_api_binding(&second).is_err());
        assert_eq!(
            store.api_bindings_for_topology("main").unwrap(),
            vec![first]
        );
        assert!(
            store
                .api_bindings_for_topology("secondary")
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn topology_replace_allows_one_consumer_to_bind_distinct_requirements_across_topologies() {
        let directory = tempdir().unwrap();
        let store =
            SqliteOrchestratorStore::open(directory.path().join("orchestrator.db")).unwrap();
        let first = binding("binding-main", "STORAGE_GET");
        store
            .replace_topology_api_bindings("main", std::slice::from_ref(&first))
            .unwrap();
        let mut second = binding("binding-secondary", "JUDGE_CONTROL");
        second.topology_id = "secondary".to_string();
        second.topology_revision_id = "revision-2".to_string();
        store
            .replace_topology_api_bindings("secondary", std::slice::from_ref(&second))
            .unwrap();
        assert_eq!(
            store
                .api_bindings_for_deployment("consumer-1")
                .unwrap()
                .len(),
            2
        );
    }
}
