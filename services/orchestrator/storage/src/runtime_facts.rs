use crate::{SqliteOrchestratorStore, StorageError, StorageResult, StoredRuntimeInstance};
use rusqlite::{OptionalExtension, TransactionBehavior};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeSet;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct StoredNodeRuntimeFacts {
    pub node_id: String,
    pub observed_at_ms: i64,
    pub received_at_ms: i64,
    pub facts: Value,
}

impl StoredNodeRuntimeFacts {
    pub fn validate(&self) -> StorageResult<()> {
        if self.node_id.trim().is_empty() || self.node_id.len() > 128 {
            return Err(StorageError::Domain(
                "node runtime facts require a bounded node_id".to_string(),
            ));
        }
        if self.observed_at_ms < 0 || self.received_at_ms < 0 || !self.facts.is_object() {
            return Err(StorageError::Domain(
                "node runtime facts timestamps and JSON payload are invalid".to_string(),
            ));
        }
        Ok(())
    }

    pub fn is_stale_at(&self, now_ms: i64, stale_after_ms: i64) -> bool {
        stale_after_ms <= 0 || now_ms.saturating_sub(self.received_at_ms) > stale_after_ms
    }
}

impl SqliteOrchestratorStore {
    pub fn put_node_runtime_facts(&self, value: &StoredNodeRuntimeFacts) -> StorageResult<()> {
        value.validate()?;
        let payload = serde_json::to_string(&value.facts)?;
        let connection = self.connection()?;
        connection.execute(
            "INSERT INTO orchestrator_node_runtime_facts
                 (node_id, observed_at_ms, received_at_ms, payload)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(node_id) DO UPDATE SET
                 observed_at_ms = excluded.observed_at_ms,
                 received_at_ms = excluded.received_at_ms,
                 payload = excluded.payload",
            rusqlite::params![
                value.node_id,
                value.observed_at_ms,
                value.received_at_ms,
                payload
            ],
        )?;
        Ok(())
    }

    pub fn node_runtime_facts(
        &self,
        node_id: &str,
    ) -> StorageResult<Option<StoredNodeRuntimeFacts>> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT observed_at_ms, received_at_ms, payload
             FROM orchestrator_node_runtime_facts WHERE node_id = ?1",
        )?;
        let mut rows = statement.query([node_id])?;
        let Some(row) = rows.next()? else {
            return Ok(None);
        };
        let payload: String = row.get(2)?;
        Ok(Some(StoredNodeRuntimeFacts {
            node_id: node_id.to_string(),
            observed_at_ms: row.get(0)?,
            received_at_ms: row.get(1)?,
            facts: serde_json::from_str(&payload)?,
        }))
    }

    /// Atomically replaces the authenticated Node report and all runtime
    /// projections derived from that same fully validated report.
    pub fn apply_node_runtime_report(
        &self,
        value: &StoredNodeRuntimeFacts,
        expected_managed_deployment_ids: Option<&[String]>,
        runtime_instances: &[(StoredRuntimeInstance, StoredRuntimeInstance)],
    ) -> StorageResult<()> {
        value.validate()?;
        for (expected, projected) in runtime_instances {
            expected.validate()?;
            projected.validate()?;
            if expected.node_id != value.node_id
                || projected.node_id != value.node_id
                || expected.instance.deployment_id != projected.instance.deployment_id
            {
                return Err(StorageError::Invariant(
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
            return Err(StorageError::Invariant(
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
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let previous = transaction
            .query_row(
                "SELECT observed_at_ms, payload FROM orchestrator_node_runtime_facts WHERE node_id = ?1",
                [&value.node_id],
                |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?)),
            )
            .optional()?;
        if let Some((previous_observed_at_ms, previous_payload)) = previous {
            let previous_facts: Value = serde_json::from_str(&previous_payload)?;
            let previous_report_id = previous_facts
                .get("report_id")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let report_id = value
                .facts
                .get("report_id")
                .and_then(Value::as_str)
                .unwrap_or_default();
            if previous_report_id == report_id {
                if previous_facts == value.facts {
                    // Continue: a lifecycle projection may have been created
                    // after this report was first accepted and now needs to
                    // catch up to the already-durable inventory.
                } else {
                    return Err(StorageError::Conflict(
                        "runtime report_id was reused with different content".to_string(),
                    ));
                }
            } else if value.observed_at_ms <= previous_observed_at_ms {
                return Err(StorageError::Conflict(
                    "runtime report is not newer than the accepted Node report".to_string(),
                ));
            }
        }
        if let Some(expected_deployments) = expected_managed_deployment_ids.as_ref() {
            let current_deployments = {
                let mut statement = transaction.prepare(
                    "SELECT payload FROM orchestrator_runtime_instances WHERE node_id = ?1",
                )?;
                let rows = statement.query_map([&value.node_id], |row| row.get::<_, String>(0))?;
                let mut current = BTreeSet::new();
                for payload in rows {
                    let payload = payload?;
                    let runtime: StoredRuntimeInstance = serde_json::from_value(
                        crate::runtime_instances::normalize_legacy_runtime_payload(&payload)?,
                    )?;
                    if runtime.management_mode == crate::RuntimeManagementMode::Managed {
                        current.insert(runtime.instance.deployment_id);
                    }
                }
                current
            };
            if &current_deployments != expected_deployments {
                return Err(StorageError::Conflict(
                    "managed runtime deployment set changed while applying its Node report"
                        .to_string(),
                ));
            }
        }
        transaction.execute(
            "INSERT INTO orchestrator_node_runtime_facts
                 (node_id, observed_at_ms, received_at_ms, payload)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(node_id) DO UPDATE SET
                 observed_at_ms = excluded.observed_at_ms,
                 received_at_ms = excluded.received_at_ms,
                 payload = excluded.payload",
            rusqlite::params![
                value.node_id,
                value.observed_at_ms,
                value.received_at_ms,
                facts_payload
            ],
        )?;
        for (expected, projected, payload) in runtime_payloads {
            let current_payload = transaction
                .query_row(
                    "SELECT payload FROM orchestrator_runtime_instances WHERE deployment_id = ?1",
                    [&expected.instance.deployment_id],
                    |row| row.get::<_, String>(0),
                )
                .optional()?
                .ok_or_else(|| {
                    StorageError::Conflict(format!(
                        "runtime deployment {} disappeared while applying its Node report",
                        expected.instance.deployment_id
                    ))
                })?;
            let current: StoredRuntimeInstance = serde_json::from_value(
                crate::runtime_instances::normalize_legacy_runtime_payload(&current_payload)?,
            )?;
            if &current == projected {
                // Exact report replay is idempotent. The first application
                // already installed this projection, so there is no row
                // mutation to repeat. This also lets a handler resume after a
                // crash between the atomic storage commit and its response.
                continue;
            }
            if &current != expected {
                return Err(StorageError::Conflict(format!(
                    "runtime deployment {} changed while applying its Node report",
                    expected.instance.deployment_id
                )));
            }
            transaction.execute(
                "UPDATE orchestrator_runtime_instances SET node_id = ?2, service_id = ?3, desired_state = ?4, observed_state = ?5, payload = ?6, updated_at = unixepoch() WHERE deployment_id = ?1",
                rusqlite::params![
                    projected.instance.deployment_id,
                    projected.node_id,
                    projected.instance.service_id,
                    crate::runtime_instances::desired_state(&projected.instance.desired_state),
                    crate::runtime_instances::observed_state(&projected.instance.observed_state),
                    payload,
                ],
            )?;
        }
        transaction.commit()?;
        Ok(())
    }
}
