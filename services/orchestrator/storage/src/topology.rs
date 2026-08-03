use crate::{SqliteOrchestratorStore, StorageError, StorageResult};
use orchestrator_legacy::{
    TopologyReconciliationState, TopologyRevision, TopologySpec, TopologyStatus,
};
use rusqlite::{OptionalExtension, Transaction, TransactionBehavior, params};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TopologyHeads {
    pub topology_id: String,
    pub draft_revision_id: String,
    pub applied_revision_id: Option<String>,
    pub applying_revision_id: Option<String>,
    pub applying_operation_id: Option<String>,
    pub last_operation_id: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TopologyApplyOutcome {
    Succeeded,
    Failed,
    Degraded,
}

impl SqliteOrchestratorStore {
    pub fn create_initial_topology_revision(
        &self,
        spec: TopologySpec,
        created_at: impl Into<String>,
        created_by: impl Into<String>,
        message: impl Into<String>,
    ) -> StorageResult<TopologyRevision> {
        let revision = TopologyRevision::initial(spec, created_at, created_by, message)
            .map_err(domain_error)?;
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let exists = transaction.query_row(
            "SELECT EXISTS(SELECT 1 FROM orchestrator_topology_heads WHERE topology_id = ?1)",
            [revision.topology_id()],
            |row| row.get::<_, bool>(0),
        )?;
        if exists {
            return Err(StorageError::Conflict(format!(
                "topology {} already exists",
                revision.topology_id()
            )));
        }
        insert_revision(&transaction, &revision)?;
        transaction.execute(
            "INSERT INTO orchestrator_topology_heads(topology_id, draft_revision_id) VALUES (?1, ?2)",
            params![revision.topology_id(), revision.revision_id()],
        )?;
        let status = TopologyStatus::draft(
            revision.topology_id(),
            Some(revision.revision_id().to_string()),
            revision.created_at(),
        )
        .map_err(domain_error)?;
        upsert_status(&transaction, &status)?;
        transaction.commit()?;
        Ok(revision)
    }

    pub fn create_next_topology_revision(
        &self,
        topology_id: &str,
        expected_draft_revision_id: &str,
        spec: TopologySpec,
        created_at: impl Into<String>,
        created_by: impl Into<String>,
        message: impl Into<String>,
    ) -> StorageResult<TopologyRevision> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let heads = load_heads(&transaction, topology_id)?.ok_or_else(|| {
            StorageError::Invariant(format!("topology {topology_id} does not exist"))
        })?;
        ensure_mutable_head(&heads, expected_draft_revision_id)?;
        let current = load_revision(&transaction, topology_id, expected_draft_revision_id)?
            .ok_or_else(|| {
                StorageError::Invariant(format!(
                    "draft revision {expected_draft_revision_id} is missing"
                ))
            })?;
        let revision = current
            .next(spec, created_at, created_by, message)
            .map_err(domain_error)?;
        insert_revision(&transaction, &revision)?;
        let changed = transaction.execute(
            "UPDATE orchestrator_topology_heads SET draft_revision_id = ?3, updated_at = unixepoch() WHERE topology_id = ?1 AND draft_revision_id = ?2 AND applying_revision_id IS NULL",
            params![topology_id, expected_draft_revision_id, revision.revision_id()],
        )?;
        if changed != 1 {
            return Err(StorageError::Conflict(format!(
                "topology {topology_id} draft changed concurrently"
            )));
        }
        let status = TopologyStatus::draft(
            topology_id,
            Some(revision.revision_id().to_string()),
            revision.created_at(),
        )
        .map_err(domain_error)?;
        upsert_status(&transaction, &status)?;
        transaction.commit()?;
        Ok(revision)
    }

    pub fn create_topology_rollback_revision(
        &self,
        topology_id: &str,
        expected_draft_revision_id: &str,
        target_revision_id: &str,
        created_at: impl Into<String>,
        created_by: impl Into<String>,
        message: impl Into<String>,
    ) -> StorageResult<TopologyRevision> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let heads = load_heads(&transaction, topology_id)?.ok_or_else(|| {
            StorageError::Invariant(format!("topology {topology_id} does not exist"))
        })?;
        ensure_mutable_head(&heads, expected_draft_revision_id)?;
        let current = load_revision(&transaction, topology_id, expected_draft_revision_id)?
            .ok_or_else(|| {
                StorageError::Invariant(format!(
                    "draft revision {expected_draft_revision_id} is missing"
                ))
            })?;
        let target =
            load_revision(&transaction, topology_id, target_revision_id)?.ok_or_else(|| {
                StorageError::Invariant(format!("rollback target {target_revision_id} is missing"))
            })?;
        let revision = current
            .rollback_to(&target, created_at, created_by, message)
            .map_err(domain_error)?;
        insert_revision(&transaction, &revision)?;
        let changed = transaction.execute(
            "UPDATE orchestrator_topology_heads SET draft_revision_id = ?3, updated_at = unixepoch() WHERE topology_id = ?1 AND draft_revision_id = ?2 AND applying_revision_id IS NULL",
            params![topology_id, expected_draft_revision_id, revision.revision_id()],
        )?;
        if changed != 1 {
            return Err(StorageError::Conflict(format!(
                "topology {topology_id} draft changed concurrently"
            )));
        }
        let status = TopologyStatus::draft(
            topology_id,
            Some(revision.revision_id().to_string()),
            revision.created_at(),
        )
        .map_err(domain_error)?;
        upsert_status(&transaction, &status)?;
        transaction.commit()?;
        Ok(revision)
    }

    pub fn begin_topology_apply(
        &self,
        topology_id: &str,
        expected_draft_revision_id: &str,
        operation_id: &str,
        updated_at: &str,
    ) -> StorageResult<TopologyRevision> {
        if operation_id.trim().is_empty() {
            return Err(StorageError::Invariant(
                "operation_id must not be empty".to_string(),
            ));
        }
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let revision = load_revision(&transaction, topology_id, expected_draft_revision_id)?
            .ok_or_else(|| {
                StorageError::Invariant(format!(
                    "draft revision {expected_draft_revision_id} is missing"
                ))
            })?;
        let changed = transaction.execute(
            "UPDATE orchestrator_topology_heads SET applying_revision_id = ?2, applying_operation_id = ?3, last_operation_id = ?3, updated_at = unixepoch() WHERE topology_id = ?1 AND draft_revision_id = ?2 AND applying_revision_id IS NULL",
            params![topology_id, expected_draft_revision_id, operation_id],
        )?;
        if changed != 1 {
            return Err(StorageError::Conflict(format!(
                "topology {topology_id} is already applying or its draft changed"
            )));
        }
        let mut status = TopologyStatus::draft(
            topology_id,
            Some(expected_draft_revision_id.to_string()),
            updated_at,
        )
        .map_err(domain_error)?;
        status.state = TopologyReconciliationState::Reconciling;
        status.last_operation_id = Some(operation_id.to_string());
        status.validate().map_err(domain_error)?;
        upsert_status(&transaction, &status)?;
        transaction.commit()?;
        Ok(revision)
    }

    pub fn finish_topology_apply(
        &self,
        topology_id: &str,
        revision_id: &str,
        operation_id: &str,
        outcome: TopologyApplyOutcome,
        updated_at: &str,
    ) -> StorageResult<TopologyHeads> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let heads = load_heads(&transaction, topology_id)?.ok_or_else(|| {
            StorageError::Invariant(format!("topology {topology_id} does not exist"))
        })?;
        if heads.applying_revision_id.as_deref() != Some(revision_id)
            || heads.applying_operation_id.as_deref() != Some(operation_id)
        {
            return Err(StorageError::Conflict(format!(
                "topology {topology_id} apply lease does not match operation {operation_id}"
            )));
        }
        let previous_applied = heads.applied_revision_id.clone();
        let applied_revision = match outcome {
            TopologyApplyOutcome::Succeeded => Some(revision_id),
            TopologyApplyOutcome::Failed | TopologyApplyOutcome::Degraded => {
                previous_applied.as_deref()
            }
        };
        let changed = transaction.execute(
            "UPDATE orchestrator_topology_heads SET applied_revision_id = ?4, applying_revision_id = NULL, applying_operation_id = NULL, last_operation_id = ?3, updated_at = unixepoch() WHERE topology_id = ?1 AND applying_revision_id = ?2 AND applying_operation_id = ?3",
            params![topology_id, revision_id, operation_id, applied_revision],
        )?;
        if changed != 1 {
            return Err(StorageError::Conflict(format!(
                "topology {topology_id} apply completion raced with another writer"
            )));
        }
        let status = TopologyStatus {
            topology_id: topology_id.to_string(),
            desired_revision_id: Some(revision_id.to_string()),
            observed_revision_id: match outcome {
                TopologyApplyOutcome::Succeeded => Some(revision_id.to_string()),
                TopologyApplyOutcome::Failed | TopologyApplyOutcome::Degraded => previous_applied,
            },
            state: match outcome {
                TopologyApplyOutcome::Succeeded => TopologyReconciliationState::InSync,
                TopologyApplyOutcome::Failed => TopologyReconciliationState::Failed,
                TopologyApplyOutcome::Degraded => TopologyReconciliationState::Degraded,
            },
            deployments: Vec::new(),
            endpoints: Vec::new(),
            links: Vec::new(),
            drift: Vec::new(),
            last_operation_id: Some(operation_id.to_string()),
            updated_at: updated_at.to_string(),
        };
        status.validate().map_err(domain_error)?;
        upsert_status(&transaction, &status)?;
        let completed = load_heads(&transaction, topology_id)?.ok_or_else(|| {
            StorageError::Invariant(format!("topology {topology_id} head disappeared"))
        })?;
        transaction.commit()?;
        Ok(completed)
    }

    pub fn topology_heads(&self, topology_id: &str) -> StorageResult<Option<TopologyHeads>> {
        let connection = self.connection()?;
        load_heads(&connection, topology_id)
    }

    pub fn list_topology_heads(&self) -> StorageResult<Vec<TopologyHeads>> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT topology_id, draft_revision_id, applied_revision_id, applying_revision_id, applying_operation_id, last_operation_id FROM orchestrator_topology_heads ORDER BY topology_id",
        )?;
        statement
            .query_map([], |row| {
                Ok(TopologyHeads {
                    topology_id: row.get(0)?,
                    draft_revision_id: row.get(1)?,
                    applied_revision_id: row.get(2)?,
                    applying_revision_id: row.get(3)?,
                    applying_operation_id: row.get(4)?,
                    last_operation_id: row.get(5)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(StorageError::from)
    }

    pub fn topology_revision(
        &self,
        topology_id: &str,
        revision_id: &str,
    ) -> StorageResult<Option<TopologyRevision>> {
        let connection = self.connection()?;
        load_revision(&connection, topology_id, revision_id)
    }

    pub fn topology_revisions(&self, topology_id: &str) -> StorageResult<Vec<TopologyRevision>> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT payload FROM orchestrator_topology_revisions WHERE topology_id = ?1 ORDER BY revision_number DESC",
        )?;
        let payloads = statement
            .query_map([topology_id], |row| row.get::<_, String>(0))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        payloads
            .into_iter()
            .map(|payload| deserialize_revision(&payload))
            .collect()
    }

    pub fn topology_status(&self, topology_id: &str) -> StorageResult<Option<TopologyStatus>> {
        let payload = self
            .connection()?
            .query_row(
                "SELECT payload FROM orchestrator_topology_status WHERE topology_id = ?1",
                [topology_id],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        payload
            .map(|payload| {
                let status: TopologyStatus = serde_json::from_str(&payload)?;
                status.validate().map_err(domain_error)?;
                Ok(status)
            })
            .transpose()
    }

    pub fn put_topology_status(&self, status: &TopologyStatus) -> StorageResult<()> {
        status.validate().map_err(domain_error)?;
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        ensure_status_revision(
            &transaction,
            &status.topology_id,
            status.desired_revision_id.as_deref(),
        )?;
        ensure_status_revision(
            &transaction,
            &status.topology_id,
            status.observed_revision_id.as_deref(),
        )?;
        upsert_status(&transaction, status)?;
        transaction.commit()?;
        Ok(())
    }

    /// Persists an observation only while the applied head still matches the
    /// revision that was observed and no apply owns the topology.  Provider
    /// I/O happens before this transaction, so this compare-and-set prevents a
    /// stale reconciler result from overwriting a newer `RECONCILING` status.
    pub fn put_reconciled_topology_status(
        &self,
        status: &TopologyStatus,
        expected_applied_revision_id: &str,
    ) -> StorageResult<()> {
        status.validate().map_err(domain_error)?;
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let heads = load_heads(&transaction, &status.topology_id)?.ok_or_else(|| {
            StorageError::Invariant(format!("topology {} does not exist", status.topology_id))
        })?;
        if heads.applied_revision_id.as_deref() != Some(expected_applied_revision_id)
            || heads.applying_revision_id.is_some()
            || status.desired_revision_id.as_deref() != Some(expected_applied_revision_id)
        {
            return Err(StorageError::Conflict(format!(
                "topology {} changed while its provider state was observed",
                status.topology_id
            )));
        }
        ensure_status_revision(
            &transaction,
            &status.topology_id,
            status.desired_revision_id.as_deref(),
        )?;
        ensure_status_revision(
            &transaction,
            &status.topology_id,
            status.observed_revision_id.as_deref(),
        )?;
        upsert_status(&transaction, status)?;
        transaction.commit()?;
        Ok(())
    }
}

fn ensure_mutable_head(heads: &TopologyHeads, expected: &str) -> StorageResult<()> {
    if heads.draft_revision_id != expected {
        return Err(StorageError::Conflict(format!(
            "expected draft {expected}, current draft is {}",
            heads.draft_revision_id
        )));
    }
    if heads.applying_revision_id.is_some() {
        return Err(StorageError::Conflict(format!(
            "topology {} has an apply in progress",
            heads.topology_id
        )));
    }
    Ok(())
}

fn insert_revision(
    transaction: &Transaction<'_>,
    revision: &TopologyRevision,
) -> StorageResult<()> {
    revision.verify().map_err(domain_error)?;
    let revision_number = i64::try_from(revision.revision_number()).map_err(|_| {
        StorageError::Invariant("revision number exceeds SQLite INTEGER range".to_string())
    })?;
    transaction.execute(
        "INSERT INTO orchestrator_topology_revisions(topology_id, revision_number, revision_id, parent_revision_id, rollback_of_revision_id, content_sha256, payload) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            revision.topology_id(),
            revision_number,
            revision.revision_id(),
            revision.parent_revision_id(),
            revision.rollback_of_revision_id(),
            revision.content_sha256(),
            serde_json::to_string(revision)?,
        ],
    )?;
    Ok(())
}

fn load_revision(
    connection: &rusqlite::Connection,
    topology_id: &str,
    revision_id: &str,
) -> StorageResult<Option<TopologyRevision>> {
    let payload = connection
        .query_row(
            "SELECT payload FROM orchestrator_topology_revisions WHERE topology_id = ?1 AND revision_id = ?2",
            params![topology_id, revision_id],
            |row| row.get::<_, String>(0),
        )
        .optional()?;
    payload
        .map(|value| deserialize_revision(&value))
        .transpose()
}

fn deserialize_revision(payload: &str) -> StorageResult<TopologyRevision> {
    let revision: TopologyRevision = serde_json::from_str(payload)?;
    revision.verify().map_err(domain_error)?;
    Ok(revision)
}

fn load_heads(
    connection: &rusqlite::Connection,
    topology_id: &str,
) -> StorageResult<Option<TopologyHeads>> {
    connection
        .query_row(
            "SELECT draft_revision_id, applied_revision_id, applying_revision_id, applying_operation_id, last_operation_id FROM orchestrator_topology_heads WHERE topology_id = ?1",
            [topology_id],
            |row| {
                Ok(TopologyHeads {
                    topology_id: topology_id.to_string(),
                    draft_revision_id: row.get(0)?,
                    applied_revision_id: row.get(1)?,
                    applying_revision_id: row.get(2)?,
                    applying_operation_id: row.get(3)?,
                    last_operation_id: row.get(4)?,
                })
            },
        )
        .optional()
        .map_err(StorageError::from)
}

fn ensure_status_revision(
    transaction: &Transaction<'_>,
    topology_id: &str,
    revision_id: Option<&str>,
) -> StorageResult<()> {
    if let Some(revision_id) = revision_id
        && load_revision(transaction, topology_id, revision_id)?.is_none()
    {
        return Err(StorageError::Invariant(format!(
            "status references revision {revision_id} outside topology {topology_id}"
        )));
    }
    Ok(())
}

fn upsert_status(transaction: &Transaction<'_>, status: &TopologyStatus) -> StorageResult<()> {
    transaction.execute(
        "INSERT INTO orchestrator_topology_status(topology_id, desired_revision_id, observed_revision_id, payload) VALUES (?1, ?2, ?3, ?4) ON CONFLICT(topology_id) DO UPDATE SET desired_revision_id = excluded.desired_revision_id, observed_revision_id = excluded.observed_revision_id, payload = excluded.payload, updated_at = unixepoch()",
        params![
            status.topology_id,
            status.desired_revision_id,
            status.observed_revision_id,
            serde_json::to_string(status)?,
        ],
    )?;
    Ok(())
}

fn domain_error(error: orchestrator_legacy::OrchestratorError) -> StorageError {
    StorageError::Domain(error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use orchestrator_legacy::{TopologyEndpointSpec, TopologyLinkSpec};
    use serde_json::json;
    use tempfile::tempdir;

    fn spec(topology_id: &str, note: &str) -> TopologySpec {
        TopologySpec::new(
            topology_id,
            "127.0.0.1:8080:gateway",
            "private",
            vec![
                TopologyEndpointSpec {
                    endpoint: "127.0.0.1:8080:gateway".to_string(),
                    service_id: "gateway".to_string(),
                    protocol: "https".to_string(),
                    health_path: "/healthz".to_string(),
                    display_name: "Gateway".to_string(),
                    note: note.to_string(),
                    config: json!({}),
                },
                TopologyEndpointSpec {
                    endpoint: "127.0.0.1:8081:worker".to_string(),
                    service_id: "worker".to_string(),
                    protocol: "https".to_string(),
                    health_path: "/healthz".to_string(),
                    display_name: "Worker".to_string(),
                    note: String::new(),
                    config: json!({}),
                },
            ],
            vec![TopologyLinkSpec {
                source_endpoint: "127.0.0.1:8080:gateway".to_string(),
                target_endpoint: "127.0.0.1:8081:worker".to_string(),
                protocol: "https".to_string(),
                auth_mode: "internal".to_string(),
                scope: "worker.invoke".to_string(),
                enabled: true,
                config_ref: String::new(),
                secret_ref: String::new(),
                policy: json!({}),
            }],
        )
        .unwrap()
    }

    fn store() -> (tempfile::TempDir, SqliteOrchestratorStore) {
        let directory = tempdir().unwrap();
        let store =
            SqliteOrchestratorStore::open(directory.path().join("orchestrator.db")).unwrap();
        (directory, store)
    }

    #[test]
    fn revision_is_immutable_and_uses_optimistic_concurrency() {
        let (_directory, store) = store();
        let first = store
            .create_initial_topology_revision(spec("primary", "first"), "t1", "admin", "initial")
            .unwrap();
        let second = store
            .create_next_topology_revision(
                "primary",
                first.revision_id(),
                spec("primary", "second"),
                "t2",
                "admin",
                "edit",
            )
            .unwrap();
        assert!(matches!(
            store.create_next_topology_revision(
                "primary",
                first.revision_id(),
                spec("primary", "stale"),
                "t3",
                "admin",
                "stale",
            ),
            Err(StorageError::Conflict(_))
        ));
        assert_eq!(store.topology_revisions("primary").unwrap().len(), 2);
        assert_eq!(
            store
                .topology_revision("primary", first.revision_id())
                .unwrap(),
            Some(first)
        );
        assert_eq!(
            store
                .topology_heads("primary")
                .unwrap()
                .unwrap()
                .draft_revision_id,
            second.revision_id()
        );
    }

    #[test]
    fn applied_head_advances_only_after_success() {
        let (_directory, store) = store();
        let first = store
            .create_initial_topology_revision(spec("primary", "first"), "t1", "admin", "initial")
            .unwrap();
        store
            .begin_topology_apply("primary", first.revision_id(), "op-1", "t2")
            .unwrap();
        let failed = store
            .finish_topology_apply(
                "primary",
                first.revision_id(),
                "op-1",
                TopologyApplyOutcome::Failed,
                "t3",
            )
            .unwrap();
        assert_eq!(failed.applied_revision_id, None);

        store
            .begin_topology_apply("primary", first.revision_id(), "op-2", "t4")
            .unwrap();
        let succeeded = store
            .finish_topology_apply(
                "primary",
                first.revision_id(),
                "op-2",
                TopologyApplyOutcome::Succeeded,
                "t5",
            )
            .unwrap();
        assert_eq!(
            succeeded.applied_revision_id.as_deref(),
            Some(first.revision_id())
        );
        assert_eq!(
            store.topology_status("primary").unwrap().unwrap().state,
            TopologyReconciliationState::InSync
        );
    }

    #[test]
    fn rollback_creates_a_new_revision() {
        let (_directory, store) = store();
        let first = store
            .create_initial_topology_revision(spec("primary", "first"), "t1", "admin", "initial")
            .unwrap();
        let second = store
            .create_next_topology_revision(
                "primary",
                first.revision_id(),
                spec("primary", "second"),
                "t2",
                "admin",
                "edit",
            )
            .unwrap();
        let rollback = store
            .create_topology_rollback_revision(
                "primary",
                second.revision_id(),
                first.revision_id(),
                "t3",
                "admin",
                "rollback",
            )
            .unwrap();
        assert_eq!(rollback.revision_number(), 3);
        assert_eq!(
            rollback.rollback_of_revision_id(),
            Some(first.revision_id())
        );
        assert_eq!(rollback.spec(), first.spec());
    }

    #[test]
    fn stale_reconciler_cannot_overwrite_a_new_apply_status() {
        let (_directory, store) = store();
        let first = store
            .create_initial_topology_revision(spec("primary", "first"), "t1", "admin", "initial")
            .unwrap();
        store
            .begin_topology_apply("primary", first.revision_id(), "op-1", "t2")
            .unwrap();
        store
            .finish_topology_apply(
                "primary",
                first.revision_id(),
                "op-1",
                TopologyApplyOutcome::Succeeded,
                "t3",
            )
            .unwrap();
        let mut observed = store.topology_status("primary").unwrap().unwrap();
        observed.updated_at = "t4".to_string();
        store
            .put_reconciled_topology_status(&observed, first.revision_id())
            .unwrap();

        let second = store
            .create_next_topology_revision(
                "primary",
                first.revision_id(),
                spec("primary", "second"),
                "t5",
                "admin",
                "edit",
            )
            .unwrap();
        store
            .begin_topology_apply("primary", second.revision_id(), "op-2", "t6")
            .unwrap();
        assert!(matches!(
            store.put_reconciled_topology_status(&observed, first.revision_id()),
            Err(StorageError::Conflict(_))
        ));
        let current = store.topology_status("primary").unwrap().unwrap();
        assert_eq!(current.state, TopologyReconciliationState::Reconciling);
        assert_eq!(
            current.desired_revision_id.as_deref(),
            Some(second.revision_id())
        );
    }
}
