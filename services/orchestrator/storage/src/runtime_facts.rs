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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn runtime(node_id: &str) -> StoredRuntimeInstance {
        serde_json::from_value(json!({
            "node_id": node_id,
            "instance": {
                "deployment_id": "deployment-report",
                "service_id": "service-report",
                "release_version": "1.0.0",
                "container_id": "container-report",
                "artifact_digest": format!("registry.example/service@sha256:{}", "a".repeat(64)),
                "desired_state": "RUNNING",
                "observed_state": "RUNNING",
                "health": "HEALTHY"
            },
            "management_mode": "MANAGED",
            "endpoint": "",
            "last_observed_at_ms": 100,
            "updated_at": "unix-ms:100"
        }))
        .unwrap()
    }

    #[test]
    fn latest_runtime_facts_survive_a_sqlite_restart() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("runtime-facts.db");
        let value = StoredNodeRuntimeFacts {
            node_id: "node-1".to_string(),
            observed_at_ms: 100,
            received_at_ms: 110,
            facts: json!({"schema_version": 1, "runtime_policy_sha256": "sha256:test"}),
        };
        {
            let store = SqliteOrchestratorStore::open(&path).unwrap();
            store.put_node_runtime_facts(&value).unwrap();
        }
        let reopened = SqliteOrchestratorStore::open(&path).unwrap();
        assert_eq!(reopened.node_runtime_facts("node-1").unwrap(), Some(value));
    }

    #[test]
    fn staleness_uses_control_plane_receive_time() {
        let value = StoredNodeRuntimeFacts {
            node_id: "node-1".to_string(),
            observed_at_ms: 1,
            received_at_ms: 1_000,
            facts: json!({}),
        };
        assert!(!value.is_stale_at(61_000, 60_000));
        assert!(value.is_stale_at(61_001, 60_000));
    }

    #[test]
    fn report_and_runtime_projection_commit_atomically_after_full_validation() {
        let directory = tempfile::tempdir().unwrap();
        let store =
            SqliteOrchestratorStore::open(directory.path().join("atomic-report.db")).unwrap();
        let facts = StoredNodeRuntimeFacts {
            node_id: "node-report".to_string(),
            observed_at_ms: 100,
            received_at_ms: 101,
            facts: json!({"schema_version": 1, "report_id": "report-1"}),
        };
        let wrong = runtime("different-node");
        assert!(
            store
                .apply_node_runtime_report(
                    &facts,
                    Some(&["deployment-report".to_string()]),
                    &[(wrong.clone(), wrong)],
                )
                .is_err()
        );
        assert!(store.node_runtime_facts("node-report").unwrap().is_none());
        assert!(
            store
                .runtime_instance("deployment-report")
                .unwrap()
                .is_none()
        );

        let projected = runtime("node-report");
        store.put_runtime_instance(&projected).unwrap();
        let mut observed = projected.clone();
        observed.last_observed_at_ms = 100;
        store
            .apply_node_runtime_report(
                &facts,
                Some(&["deployment-report".to_string()]),
                &[(projected.clone(), observed.clone())],
            )
            .unwrap();
        assert_eq!(
            store.node_runtime_facts("node-report").unwrap(),
            Some(facts.clone())
        );
        assert_eq!(
            store.runtime_instance("deployment-report").unwrap(),
            Some(observed.clone())
        );
        store
            .apply_node_runtime_report(
                &facts,
                Some(&["deployment-report".to_string()]),
                &[(projected, observed.clone())],
            )
            .unwrap();
        assert_eq!(
            store.runtime_instance("deployment-report").unwrap(),
            Some(observed)
        );
    }

    #[test]
    fn stale_runtime_snapshot_cannot_overwrite_a_lifecycle_update() {
        let directory = tempfile::tempdir().unwrap();
        let store = SqliteOrchestratorStore::open(directory.path().join("runtime-cas.db")).unwrap();
        let expected = runtime("node-report");
        store.put_runtime_instance(&expected).unwrap();
        let mut lifecycle = expected.clone();
        lifecycle.instance.desired_state = orchestrator_runtime::RuntimeDesiredState::Stopped;
        lifecycle.instance.observed_state = orchestrator_runtime::RuntimeObservedState::Stopped;
        lifecycle.instance.health = "NONE".to_string();
        lifecycle.updated_at = "unix-ms:200".to_string();
        store.put_runtime_instance(&lifecycle).unwrap();

        let mut stale_projection = expected.clone();
        stale_projection.last_observed_at_ms = 150;
        stale_projection.updated_at = "unix-ms:150".to_string();
        let facts = StoredNodeRuntimeFacts {
            node_id: "node-report".to_string(),
            observed_at_ms: 150,
            received_at_ms: 151,
            facts: json!({"schema_version": 1, "report_id": "stale-runtime"}),
        };
        assert!(matches!(
            store.apply_node_runtime_report(
                &facts,
                Some(&["deployment-report".to_string()]),
                &[(expected, stale_projection)],
            ),
            Err(StorageError::Conflict(_))
        ));
        assert_eq!(
            store.runtime_instance("deployment-report").unwrap(),
            Some(lifecycle)
        );
        assert!(store.node_runtime_facts("node-report").unwrap().is_none());
    }

    #[test]
    fn complete_report_rejects_a_managed_deployment_set_changed_after_snapshot() {
        let directory = tempfile::tempdir().unwrap();
        let store =
            SqliteOrchestratorStore::open(directory.path().join("runtime-set-cas.db")).unwrap();
        let facts = StoredNodeRuntimeFacts {
            node_id: "node-report".to_string(),
            observed_at_ms: 100,
            received_at_ms: 101,
            facts: json!({"schema_version": 1, "report_id": "empty-snapshot"}),
        };

        // The report handler took an empty managed-runtime snapshot, then an
        // install completion inserted the deployment before the report's
        // transaction acquired the Node lock.
        store.put_runtime_instance(&runtime("node-report")).unwrap();
        assert!(matches!(
            store.apply_node_runtime_report(&facts, Some(&[]), &[]),
            Err(StorageError::Conflict(_))
        ));
        assert!(store.node_runtime_facts("node-report").unwrap().is_none());
    }

    #[test]
    fn stale_partial_report_replay_conflicts_after_a_newer_complete_report() {
        let directory = tempfile::tempdir().unwrap();
        let store =
            SqliteOrchestratorStore::open(directory.path().join("runtime-partial-barrier.db"))
                .unwrap();
        let partial = StoredNodeRuntimeFacts {
            node_id: "node-report".to_string(),
            observed_at_ms: 100,
            received_at_ms: 101,
            facts: json!({
                "schema_version": 1,
                "report_id": "partial-report",
                "inventory_complete": false
            }),
        };
        store
            .apply_node_runtime_report(&partial, None, &[])
            .unwrap();

        let expected = runtime("node-report");
        store.put_runtime_instance(&expected).unwrap();
        let mut projected = expected.clone();
        projected.last_observed_at_ms = 200;
        let complete = StoredNodeRuntimeFacts {
            node_id: "node-report".to_string(),
            observed_at_ms: 200,
            received_at_ms: 201,
            facts: json!({
                "schema_version": 1,
                "report_id": "complete-report",
                "inventory_complete": true
            }),
        };
        store
            .apply_node_runtime_report(
                &complete,
                Some(&["deployment-report".to_string()]),
                &[(expected, projected.clone())],
            )
            .unwrap();

        // This is the exact stale-read interleaving used by the backend's
        // partial-report synchronization barrier: after the newer report wins
        // the Node lock, replaying the old partial snapshot must conflict so
        // the caller re-reads and converges on the complete report.
        assert!(matches!(
            store.apply_node_runtime_report(&partial, None, &[]),
            Err(StorageError::Conflict(_))
        ));
        assert_eq!(
            store.node_runtime_facts("node-report").unwrap(),
            Some(complete)
        );
        assert_eq!(
            store.runtime_instance("deployment-report").unwrap(),
            Some(projected)
        );
    }
}
