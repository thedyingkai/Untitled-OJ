use crate::{
    PostgresError, PostgresOrchestratorStore, PostgresResult, StoredNodeRuntimeFacts,
    StoredRuntimeInstance,
};
use std::collections::BTreeSet;

impl PostgresOrchestratorStore {
    pub fn put_node_runtime_facts(&self, value: &StoredNodeRuntimeFacts) -> PostgresResult<()> {
        value
            .validate()
            .map_err(|error| PostgresError::Domain(error.to_string()))?;
        let payload = serde_json::to_string(&value.facts)?;
        self.pool().with_client(|client| {
            client.execute(
                "INSERT INTO orchestrator_node_runtime_facts
                     (node_id, observed_at_ms, received_at_ms, payload)
                 VALUES ($1, $2, $3, $4::text::jsonb)
                 ON CONFLICT(node_id) DO UPDATE SET
                     observed_at_ms = excluded.observed_at_ms,
                     received_at_ms = excluded.received_at_ms,
                     payload = excluded.payload",
                &[
                    &value.node_id,
                    &value.observed_at_ms,
                    &value.received_at_ms,
                    &payload,
                ],
            )?;
            Ok(())
        })
    }

    pub fn node_runtime_facts(
        &self,
        node_id: &str,
    ) -> PostgresResult<Option<StoredNodeRuntimeFacts>> {
        self.pool().with_client(|client| {
            let Some(row) = client.query_opt(
                "SELECT observed_at_ms, received_at_ms, payload::text
                 FROM orchestrator_node_runtime_facts WHERE node_id = $1",
                &[&node_id],
            )?
            else {
                return Ok(None);
            };
            Ok(Some(StoredNodeRuntimeFacts {
                node_id: node_id.to_string(),
                observed_at_ms: row.get::<_, i64>(0),
                received_at_ms: row.get::<_, i64>(1),
                facts: serde_json::from_str(&row.get::<_, String>(2))?,
            }))
        })
    }

    pub fn apply_node_runtime_report(
        &self,
        value: &StoredNodeRuntimeFacts,
        expected_managed_deployment_ids: Option<&[String]>,
        runtime_instances: &[(StoredRuntimeInstance, StoredRuntimeInstance)],
    ) -> PostgresResult<()> {
        value
            .validate()
            .map_err(|error| PostgresError::Domain(error.to_string()))?;
        for (expected, projected) in runtime_instances {
            expected
                .validate()
                .map_err(|error| PostgresError::Invariant(error.to_string()))?;
            projected
                .validate()
                .map_err(|error| PostgresError::Invariant(error.to_string()))?;
            if expected.node_id != value.node_id
                || projected.node_id != value.node_id
                || expected.instance.deployment_id != projected.instance.deployment_id
            {
                return Err(PostgresError::Invariant(
                    "runtime report update must retain one deployment assigned to the reporting Node"
                        .to_string(),
                ));
            }
        }
        let expected_managed_deployment_count =
            expected_managed_deployment_ids.map_or(0, <[String]>::len);
        let expected_managed_deployment_ids = expected_managed_deployment_ids
            .map(|deployment_ids| deployment_ids.iter().cloned().collect::<BTreeSet<_>>());
        if let Some(expected_deployments) = expected_managed_deployment_ids.as_ref()
            && (expected_deployments.len() != expected_managed_deployment_count
                || runtime_instances.iter().any(|(expected, _)| {
                    !expected_deployments.contains(&expected.instance.deployment_id)
                }))
        {
            return Err(PostgresError::Invariant(
                "complete runtime report deployment snapshot is duplicate or incomplete"
                    .to_string(),
            ));
        }
        let facts_payload = serde_json::to_string(&value.facts)?;
        let runtime_payloads = runtime_instances
            .iter()
            .map(|(expected, projected)| {
                serde_json::to_string(projected).map(|payload| (expected, projected, payload))
            })
            .collect::<Result<Vec<_>, _>>()?;
        self.pool().with_client(|client| {
            let mut transaction = client.transaction()?;
            transaction.query_one(
                "SELECT pg_advisory_xact_lock(hashtextextended($1, 0))",
                &[&value.node_id],
            )?;
            if let Some(row) = transaction.query_opt(
                "SELECT observed_at_ms, payload::text FROM orchestrator_node_runtime_facts WHERE node_id = $1 FOR UPDATE",
                &[&value.node_id],
            )? {
                let previous_observed_at_ms = row.get::<_, i64>(0);
                let previous_facts: serde_json::Value =
                    serde_json::from_str(&row.get::<_, String>(1))?;
                let previous_report_id = previous_facts
                    .get("report_id")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default();
                let report_id = value
                    .facts
                    .get("report_id")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default();
                if previous_report_id == report_id {
                    if previous_facts == value.facts {
                        // Continue so an already-accepted report can catch up
                        // a lifecycle projection created after its first write.
                    } else {
                        return Err(PostgresError::Conflict(
                            "runtime report_id was reused with different content".to_string(),
                        ));
                    }
                } else if value.observed_at_ms <= previous_observed_at_ms {
                    return Err(PostgresError::Conflict(
                        "runtime report is not newer than the accepted Node report".to_string(),
                    ));
                }
            }
            if let Some(expected_deployments) = expected_managed_deployment_ids.as_ref() {
                let mut current_deployments = BTreeSet::new();
                for row in transaction.query(
                    "SELECT payload::text FROM orchestrator_runtime_instances WHERE node_id = $1 FOR UPDATE",
                    &[&value.node_id],
                )? {
                    let payload = row.get::<_, String>(0);
                    let runtime: StoredRuntimeInstance = serde_json::from_value(
                        crate::runtime_instances::normalize_legacy_runtime_payload(&payload)?,
                    )?;
                    if runtime.management_mode == crate::RuntimeManagementMode::Managed {
                        current_deployments.insert(runtime.instance.deployment_id);
                    }
                }
                if &current_deployments != expected_deployments {
                    return Err(PostgresError::Conflict(
                        "managed runtime deployment set changed while applying its Node report"
                            .to_string(),
                    ));
                }
            }
            transaction.execute(
                "INSERT INTO orchestrator_node_runtime_facts
                     (node_id, observed_at_ms, received_at_ms, payload)
                 VALUES ($1, $2, $3, $4::text::jsonb)
                 ON CONFLICT(node_id) DO UPDATE SET
                     observed_at_ms = excluded.observed_at_ms,
                     received_at_ms = excluded.received_at_ms,
                     payload = excluded.payload",
                &[
                    &value.node_id,
                    &value.observed_at_ms,
                    &value.received_at_ms,
                    &facts_payload,
                ],
            )?;
            for (expected, projected, payload) in &runtime_payloads {
                let row = transaction
                    .query_opt(
                        "SELECT payload::text FROM orchestrator_runtime_instances WHERE deployment_id = $1 FOR UPDATE",
                        &[&expected.instance.deployment_id],
                    )?
                    .ok_or_else(|| {
                        PostgresError::Conflict(format!(
                            "runtime deployment {} disappeared while applying its Node report",
                            expected.instance.deployment_id
                        ))
                    })?;
                let current: StoredRuntimeInstance = serde_json::from_value(
                    crate::runtime_instances::normalize_legacy_runtime_payload(
                        &row.get::<_, String>(0),
                    )?,
                )?;
                if &current == *projected {
                    // Exact report replay is an idempotent no-op for a row
                    // that already contains the projected value.
                    continue;
                }
                if &current != *expected {
                    return Err(PostgresError::Conflict(format!(
                        "runtime deployment {} changed while applying its Node report",
                        expected.instance.deployment_id
                    )));
                }
                transaction.execute(
                    "UPDATE orchestrator_runtime_instances SET node_id = $2, service_id = $3, desired_state = $4, observed_state = $5, payload = $6::text::jsonb, updated_at = clock_timestamp() WHERE deployment_id = $1",
                    &[
                        &projected.instance.deployment_id,
                        &projected.node_id,
                        &projected.instance.service_id,
                        &crate::postgres_runtime_instances::desired_state(
                            &projected.instance.desired_state,
                        ),
                        &crate::postgres_runtime_instances::observed_state(
                            &projected.instance.observed_state,
                        ),
                        payload,
                    ],
                )?;
            }
            transaction.commit()?;
            Ok(())
        })
    }
}
