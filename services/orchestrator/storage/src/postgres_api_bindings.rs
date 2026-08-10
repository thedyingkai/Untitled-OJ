use crate::{PostgresError, PostgresOrchestratorStore, PostgresResult};
use orchestrator_legacy::{ApiBinding, ApiBindingState};
use r2d2_postgres::postgres::Transaction;
use std::collections::BTreeSet;

const API_BINDING_MUTATION_LOCK_KEY: i64 = i64::from_be_bytes(*b"OJOSBIND");

/// Serializes every PostgreSQL mutation of the shared API binding projection.
///
/// Deployment completion and Topology finalization replace overlapping views
/// of this table. PostgreSQL's default READ COMMITTED isolation does not make
/// concurrent DELETE-then-INSERT replacements atomic with respect to each
/// other, so every writer must take this transaction-scoped lock first.
pub(crate) fn lock_api_binding_mutations(transaction: &mut Transaction<'_>) -> PostgresResult<()> {
    transaction.query_one(
        "SELECT pg_advisory_xact_lock($1)",
        &[&API_BINDING_MUTATION_LOCK_KEY],
    )?;
    Ok(())
}

impl PostgresOrchestratorStore {
    pub fn put_api_binding(&self, binding: &ApiBinding) -> PostgresResult<()> {
        validate(binding)?;
        let payload = serde_json::to_string(binding)?;
        self.pool().with_client(|client| {
            let mut transaction = client.transaction()?;
            lock_api_binding_mutations(&mut transaction)?;
            transaction.execute(
                "INSERT INTO orchestrator_api_bindings(binding_id, consumer_deployment_id, requirement_name, provider_deployment_id, topology_id, topology_revision_id, api_id, binding_state, payload) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9::text::jsonb) ON CONFLICT(binding_id) DO UPDATE SET consumer_deployment_id = excluded.consumer_deployment_id, requirement_name = excluded.requirement_name, provider_deployment_id = excluded.provider_deployment_id, topology_id = excluded.topology_id, topology_revision_id = excluded.topology_revision_id, api_id = excluded.api_id, binding_state = excluded.binding_state, payload = excluded.payload, updated_at = clock_timestamp()",
                &[
                    &binding.binding_id,
                    &binding.consumer_deployment_id,
                    &binding.requirement_name,
                    &binding.provider_deployment_id,
                    &binding.topology_id,
                    &binding.topology_revision_id,
                    &binding.api_id,
                    &state(binding.state),
                    &payload,
                ],
            )?;
            transaction.commit()?;
            Ok(())
        })
    }

    pub fn replace_deployment_api_bindings(
        &self,
        deployment_id: &str,
        bindings: &[ApiBinding],
    ) -> PostgresResult<()> {
        validate_set(deployment_id, bindings)?;
        self.pool().with_client(|client| {
            let mut transaction = client.transaction()?;
            lock_api_binding_mutations(&mut transaction)?;
            transaction.execute(
                "DELETE FROM orchestrator_api_bindings WHERE consumer_deployment_id = $1",
                &[&deployment_id],
            )?;
            for binding in bindings {
                let payload = serde_json::to_string(binding)?;
                transaction.execute(
                    "INSERT INTO orchestrator_api_bindings(binding_id, consumer_deployment_id, requirement_name, provider_deployment_id, topology_id, topology_revision_id, api_id, binding_state, payload) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9::text::jsonb)",
                    &[
                        &binding.binding_id,
                        &binding.consumer_deployment_id,
                        &binding.requirement_name,
                        &binding.provider_deployment_id,
                        &binding.topology_id,
                        &binding.topology_revision_id,
                        &binding.api_id,
                        &state(binding.state),
                        &payload,
                    ],
                )?;
            }
            transaction.commit()?;
            Ok(())
        })
    }

    pub fn replace_topology_api_bindings(
        &self,
        topology_id: &str,
        bindings: &[ApiBinding],
    ) -> PostgresResult<()> {
        validate_topology_set(topology_id, bindings)?;
        self.pool().with_client(|client| {
            let mut transaction = client.transaction()?;
            lock_api_binding_mutations(&mut transaction)?;
            transaction.execute(
                "DELETE FROM orchestrator_api_bindings WHERE topology_id = $1",
                &[&topology_id],
            )?;
            for binding in bindings {
                let payload = serde_json::to_string(binding)?;
                transaction.execute(
                    "INSERT INTO orchestrator_api_bindings(binding_id, consumer_deployment_id, requirement_name, provider_deployment_id, topology_id, topology_revision_id, api_id, binding_state, payload) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9::text::jsonb)",
                    &[
                        &binding.binding_id,
                        &binding.consumer_deployment_id,
                        &binding.requirement_name,
                        &binding.provider_deployment_id,
                        &binding.topology_id,
                        &binding.topology_revision_id,
                        &binding.api_id,
                        &state(binding.state),
                        &payload,
                    ],
                )?;
            }
            transaction.commit()?;
            Ok(())
        })
    }

    pub fn api_binding(&self, binding_id: &str) -> PostgresResult<Option<ApiBinding>> {
        self.pool().with_client(|client| {
            client
                .query_opt(
                    "SELECT payload::text FROM orchestrator_api_bindings WHERE binding_id = $1",
                    &[&binding_id],
                )?
                .map(|row| decode(&row.get::<_, String>(0)))
                .transpose()
        })
    }

    pub fn api_bindings_for_deployment(
        &self,
        deployment_id: &str,
    ) -> PostgresResult<Vec<ApiBinding>> {
        self.query_api_bindings(
            "SELECT payload::text FROM orchestrator_api_bindings WHERE consumer_deployment_id = $1 ORDER BY binding_id",
            deployment_id,
        )
    }

    pub fn api_bindings_for_topology(&self, topology_id: &str) -> PostgresResult<Vec<ApiBinding>> {
        self.query_api_bindings(
            "SELECT payload::text FROM orchestrator_api_bindings WHERE topology_id = $1 ORDER BY consumer_deployment_id, binding_id",
            topology_id,
        )
    }

    pub fn delete_api_bindings_for_deployment(&self, deployment_id: &str) -> PostgresResult<usize> {
        self.pool().with_client(|client| {
            let mut transaction = client.transaction()?;
            lock_api_binding_mutations(&mut transaction)?;
            let deleted = transaction.execute(
                "DELETE FROM orchestrator_api_bindings WHERE consumer_deployment_id = $1",
                &[&deployment_id],
            )? as usize;
            transaction.commit()?;
            Ok(deleted)
        })
    }

    fn query_api_bindings(&self, sql: &str, parameter: &str) -> PostgresResult<Vec<ApiBinding>> {
        self.pool().with_client(|client| {
            client
                .query(sql, &[&parameter])?
                .into_iter()
                .map(|row| decode(&row.get::<_, String>(0)))
                .collect()
        })
    }
}

fn validate(binding: &ApiBinding) -> PostgresResult<()> {
    binding
        .validate()
        .map_err(|error| PostgresError::Invariant(error.to_string()))
}

fn validate_set(deployment_id: &str, bindings: &[ApiBinding]) -> PostgresResult<()> {
    if deployment_id.trim().is_empty() {
        return Err(PostgresError::Invariant(
            "API binding deployment_id must not be empty".to_string(),
        ));
    }
    let mut ids = BTreeSet::new();
    let mut names = BTreeSet::new();
    for binding in bindings {
        validate(binding)?;
        if binding.consumer_deployment_id != deployment_id {
            return Err(PostgresError::Invariant(format!(
                "API binding {} belongs to deployment {}, expected {deployment_id}",
                binding.binding_id, binding.consumer_deployment_id
            )));
        }
        if !ids.insert(binding.binding_id.as_str())
            || !names.insert(binding.requirement_name.as_str())
        {
            return Err(PostgresError::Invariant(
                "API binding set contains duplicate ids or requirement names".to_string(),
            ));
        }
    }
    Ok(())
}

fn validate_topology_set(topology_id: &str, bindings: &[ApiBinding]) -> PostgresResult<()> {
    if topology_id.trim().is_empty() {
        return Err(PostgresError::Invariant(
            "API binding topology_id must not be empty".to_string(),
        ));
    }
    let mut ids = BTreeSet::new();
    let mut requirements = BTreeSet::new();
    for binding in bindings {
        validate(binding)?;
        if binding.topology_id != topology_id {
            return Err(PostgresError::Invariant(format!(
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
            return Err(PostgresError::Invariant(
                "topology API binding set contains duplicate ids or consumer requirements"
                    .to_string(),
            ));
        }
    }
    Ok(())
}

fn decode(payload: &str) -> PostgresResult<ApiBinding> {
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
    fn writer<'a>(source: &'a str, start: &str, end: &str) -> &'a str {
        let body = source
            .split_once(start)
            .unwrap_or_else(|| panic!("missing writer {start}"))
            .1;
        body.split_once(end)
            .unwrap_or_else(|| panic!("missing writer boundary {end}"))
            .0
    }

    #[test]
    fn every_postgres_binding_projection_writer_takes_the_shared_lock() {
        let bindings = include_str!("postgres_api_bindings.rs");
        for (start, end) in [
            (
                "pub fn put_api_binding",
                "pub fn replace_deployment_api_bindings",
            ),
            (
                "pub fn replace_deployment_api_bindings",
                "pub fn replace_topology_api_bindings",
            ),
            ("pub fn replace_topology_api_bindings", "pub fn api_binding"),
            (
                "pub fn delete_api_bindings_for_deployment",
                "fn query_api_bindings",
            ),
        ] {
            assert!(
                writer(bindings, start, end)
                    .contains("lock_api_binding_mutations(&mut transaction)?"),
                "PostgreSQL binding writer {start} bypasses the shared transaction lock"
            );
        }

        let topology = include_str!("postgres_topology.rs");
        assert!(
            topology
                .split_once("fn finish_topology_apply_group_transaction")
                .expect("group topology finalizer")
                .1
                .contains("lock_api_binding_mutations(transaction)?"),
            "group Topology finalization bypasses the shared binding transaction lock"
        );
    }

    #[test]
    fn binding_mutation_lock_identity_is_stable() {
        assert_eq!(
            super::API_BINDING_MUTATION_LOCK_KEY.to_be_bytes(),
            *b"OJOSBIND"
        );
    }
}
