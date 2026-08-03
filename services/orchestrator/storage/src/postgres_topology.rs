use crate::{
    PostgresError, PostgresOrchestratorStore, PostgresResult, TopologyApplyOutcome, TopologyHeads,
};
use orchestrator_legacy::{
    TopologyReconciliationState, TopologyRevision, TopologySpec, TopologyStatus,
};
use r2d2_postgres::postgres::{GenericClient, Transaction};

impl PostgresOrchestratorStore {
    pub fn create_initial_topology_revision(
        &self,
        spec: TopologySpec,
        created_at: impl Into<String>,
        created_by: impl Into<String>,
        message: impl Into<String>,
    ) -> PostgresResult<TopologyRevision> {
        let revision = TopologyRevision::initial(spec, created_at, created_by, message)
            .map_err(domain_error)?;
        let mut connection = self.pool().connection()?;
        let mut transaction = connection.transaction()?;
        // There is no head row to lock yet. A stable transaction advisory lock
        // closes the concurrent-create gap for this topology id.
        transaction.query_one(
            "SELECT pg_advisory_xact_lock(hashtextextended($1, 0))",
            &[&revision.topology_id()],
        )?;
        if load_heads(&mut transaction, revision.topology_id(), false)?.is_some() {
            return Err(PostgresError::Conflict(format!(
                "topology {} already exists",
                revision.topology_id()
            )));
        }
        insert_revision(&mut transaction, &revision)?;
        transaction.execute(
            "INSERT INTO orchestrator_topology_heads(topology_id, draft_revision_id) VALUES ($1, $2)",
            &[&revision.topology_id(), &revision.revision_id()],
        )?;
        let status = TopologyStatus::draft(
            revision.topology_id(),
            Some(revision.revision_id().to_string()),
            revision.created_at(),
        )
        .map_err(domain_error)?;
        upsert_status(&mut transaction, &status)?;
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
    ) -> PostgresResult<TopologyRevision> {
        let mut connection = self.pool().connection()?;
        let mut transaction = connection.transaction()?;
        let heads = load_heads(&mut transaction, topology_id, true)?.ok_or_else(|| {
            PostgresError::Invariant(format!("topology {topology_id} does not exist"))
        })?;
        ensure_mutable_head(&heads, expected_draft_revision_id)?;
        let current = load_revision(&mut transaction, topology_id, expected_draft_revision_id)?
            .ok_or_else(|| {
                PostgresError::Invariant(format!(
                    "draft revision {expected_draft_revision_id} is missing"
                ))
            })?;
        let revision = current
            .next(spec, created_at, created_by, message)
            .map_err(domain_error)?;
        insert_revision(&mut transaction, &revision)?;
        let changed = transaction.execute(
            "UPDATE orchestrator_topology_heads SET draft_revision_id = $3, updated_at = clock_timestamp() WHERE topology_id = $1 AND draft_revision_id = $2 AND applying_revision_id IS NULL",
            &[&topology_id, &expected_draft_revision_id, &revision.revision_id()],
        )?;
        if changed != 1 {
            return Err(PostgresError::Conflict(format!(
                "topology {topology_id} draft changed concurrently"
            )));
        }
        let status = TopologyStatus::draft(
            topology_id,
            Some(revision.revision_id().to_string()),
            revision.created_at(),
        )
        .map_err(domain_error)?;
        upsert_status(&mut transaction, &status)?;
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
    ) -> PostgresResult<TopologyRevision> {
        let mut connection = self.pool().connection()?;
        let mut transaction = connection.transaction()?;
        let heads = load_heads(&mut transaction, topology_id, true)?.ok_or_else(|| {
            PostgresError::Invariant(format!("topology {topology_id} does not exist"))
        })?;
        ensure_mutable_head(&heads, expected_draft_revision_id)?;
        let current = load_revision(&mut transaction, topology_id, expected_draft_revision_id)?
            .ok_or_else(|| {
                PostgresError::Invariant(format!(
                    "draft revision {expected_draft_revision_id} is missing"
                ))
            })?;
        let target =
            load_revision(&mut transaction, topology_id, target_revision_id)?.ok_or_else(|| {
                PostgresError::Invariant(format!("rollback target {target_revision_id} is missing"))
            })?;
        let revision = current
            .rollback_to(&target, created_at, created_by, message)
            .map_err(domain_error)?;
        insert_revision(&mut transaction, &revision)?;
        let changed = transaction.execute(
            "UPDATE orchestrator_topology_heads SET draft_revision_id = $3, updated_at = clock_timestamp() WHERE topology_id = $1 AND draft_revision_id = $2 AND applying_revision_id IS NULL",
            &[&topology_id, &expected_draft_revision_id, &revision.revision_id()],
        )?;
        if changed != 1 {
            return Err(PostgresError::Conflict(format!(
                "topology {topology_id} draft changed concurrently"
            )));
        }
        let status = TopologyStatus::draft(
            topology_id,
            Some(revision.revision_id().to_string()),
            revision.created_at(),
        )
        .map_err(domain_error)?;
        upsert_status(&mut transaction, &status)?;
        transaction.commit()?;
        Ok(revision)
    }

    pub fn begin_topology_apply(
        &self,
        topology_id: &str,
        expected_draft_revision_id: &str,
        operation_id: &str,
        updated_at: &str,
    ) -> PostgresResult<TopologyRevision> {
        if operation_id.trim().is_empty() {
            return Err(PostgresError::Invariant(
                "operation_id must not be empty".to_string(),
            ));
        }
        let mut connection = self.pool().connection()?;
        let mut transaction = connection.transaction()?;
        let heads = load_heads(&mut transaction, topology_id, true)?.ok_or_else(|| {
            PostgresError::Invariant(format!("topology {topology_id} does not exist"))
        })?;
        ensure_mutable_head(&heads, expected_draft_revision_id)?;
        let revision = load_revision(&mut transaction, topology_id, expected_draft_revision_id)?
            .ok_or_else(|| {
                PostgresError::Invariant(format!(
                    "draft revision {expected_draft_revision_id} is missing"
                ))
            })?;
        let changed = transaction.execute(
            "UPDATE orchestrator_topology_heads SET applying_revision_id = $2, applying_operation_id = $3, last_operation_id = $3, updated_at = clock_timestamp() WHERE topology_id = $1 AND draft_revision_id = $2 AND applying_revision_id IS NULL",
            &[&topology_id, &expected_draft_revision_id, &operation_id],
        )?;
        if changed != 1 {
            return Err(PostgresError::Conflict(format!(
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
        upsert_status(&mut transaction, &status)?;
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
    ) -> PostgresResult<TopologyHeads> {
        let mut connection = self.pool().connection()?;
        let mut transaction = connection.transaction()?;
        let heads = load_heads(&mut transaction, topology_id, true)?.ok_or_else(|| {
            PostgresError::Invariant(format!("topology {topology_id} does not exist"))
        })?;
        if heads.applying_revision_id.as_deref() != Some(revision_id)
            || heads.applying_operation_id.as_deref() != Some(operation_id)
        {
            return Err(PostgresError::Conflict(format!(
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
            "UPDATE orchestrator_topology_heads SET applied_revision_id = $4, applying_revision_id = NULL, applying_operation_id = NULL, last_operation_id = $3, updated_at = clock_timestamp() WHERE topology_id = $1 AND applying_revision_id = $2 AND applying_operation_id = $3",
            &[&topology_id, &revision_id, &operation_id, &applied_revision],
        )?;
        if changed != 1 {
            return Err(PostgresError::Conflict(format!(
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
        upsert_status(&mut transaction, &status)?;
        let completed = load_heads(&mut transaction, topology_id, false)?.ok_or_else(|| {
            PostgresError::Invariant(format!("topology {topology_id} head disappeared"))
        })?;
        transaction.commit()?;
        Ok(completed)
    }

    pub fn topology_heads(&self, topology_id: &str) -> PostgresResult<Option<TopologyHeads>> {
        self.pool()
            .with_client(|client| load_heads(client, topology_id, false))
    }

    pub fn list_topology_heads(&self) -> PostgresResult<Vec<TopologyHeads>> {
        self.pool().with_client(|client| {
            Ok(client
                .query(
                    "SELECT topology_id, draft_revision_id, applied_revision_id, applying_revision_id, applying_operation_id, last_operation_id FROM orchestrator_topology_heads ORDER BY topology_id",
                    &[],
                )?
                .into_iter()
                .map(|row| TopologyHeads {
                    topology_id: row.get(0),
                    draft_revision_id: row.get(1),
                    applied_revision_id: row.get(2),
                    applying_revision_id: row.get(3),
                    applying_operation_id: row.get(4),
                    last_operation_id: row.get(5),
                })
                .collect())
        })
    }

    pub fn topology_revision(
        &self,
        topology_id: &str,
        revision_id: &str,
    ) -> PostgresResult<Option<TopologyRevision>> {
        self.pool()
            .with_client(|client| load_revision(client, topology_id, revision_id))
    }

    pub fn topology_revisions(&self, topology_id: &str) -> PostgresResult<Vec<TopologyRevision>> {
        self.pool().with_client(|client| {
            client
                .query(
                    "SELECT payload::text FROM orchestrator_topology_revisions WHERE topology_id = $1 ORDER BY revision_number DESC",
                    &[&topology_id],
                )?
                .into_iter()
                .map(|row| deserialize_revision(&row.get::<_, String>(0)))
                .collect()
        })
    }

    pub fn topology_status(&self, topology_id: &str) -> PostgresResult<Option<TopologyStatus>> {
        self.pool().with_client(|client| {
            client
                .query_opt(
                    "SELECT payload::text FROM orchestrator_topology_status WHERE topology_id = $1",
                    &[&topology_id],
                )?
                .map(|row| deserialize_status(&row.get::<_, String>(0)))
                .transpose()
        })
    }

    pub fn put_topology_status(&self, status: &TopologyStatus) -> PostgresResult<()> {
        status.validate().map_err(domain_error)?;
        let mut connection = self.pool().connection()?;
        let mut transaction = connection.transaction()?;
        ensure_status_revision(
            &mut transaction,
            &status.topology_id,
            status.desired_revision_id.as_deref(),
        )?;
        ensure_status_revision(
            &mut transaction,
            &status.topology_id,
            status.observed_revision_id.as_deref(),
        )?;
        upsert_status(&mut transaction, status)?;
        transaction.commit()?;
        Ok(())
    }

    /// PostgreSQL counterpart of the SQLite observation CAS.  The head row is
    /// locked only for the final status write; all provider calls have already
    /// completed before this method is entered.
    pub fn put_reconciled_topology_status(
        &self,
        status: &TopologyStatus,
        expected_applied_revision_id: &str,
    ) -> PostgresResult<()> {
        status.validate().map_err(domain_error)?;
        let mut connection = self.pool().connection()?;
        let mut transaction = connection.transaction()?;
        let heads = load_heads(&mut transaction, &status.topology_id, true)?.ok_or_else(|| {
            PostgresError::Invariant(format!("topology {} does not exist", status.topology_id))
        })?;
        if heads.applied_revision_id.as_deref() != Some(expected_applied_revision_id)
            || heads.applying_revision_id.is_some()
            || status.desired_revision_id.as_deref() != Some(expected_applied_revision_id)
        {
            return Err(PostgresError::Conflict(format!(
                "topology {} changed while its provider state was observed",
                status.topology_id
            )));
        }
        ensure_status_revision(
            &mut transaction,
            &status.topology_id,
            status.desired_revision_id.as_deref(),
        )?;
        ensure_status_revision(
            &mut transaction,
            &status.topology_id,
            status.observed_revision_id.as_deref(),
        )?;
        upsert_status(&mut transaction, status)?;
        transaction.commit()?;
        Ok(())
    }
}

fn ensure_mutable_head(heads: &TopologyHeads, expected: &str) -> PostgresResult<()> {
    if heads.draft_revision_id != expected {
        return Err(PostgresError::Conflict(format!(
            "expected draft {expected}, current draft is {}",
            heads.draft_revision_id
        )));
    }
    if heads.applying_revision_id.is_some() {
        return Err(PostgresError::Conflict(format!(
            "topology {} has an apply in progress",
            heads.topology_id
        )));
    }
    Ok(())
}

fn insert_revision(
    transaction: &mut Transaction<'_>,
    revision: &TopologyRevision,
) -> PostgresResult<()> {
    revision.verify().map_err(domain_error)?;
    let revision_number = i64::try_from(revision.revision_number()).map_err(|_| {
        PostgresError::Invariant("revision number exceeds PostgreSQL BIGINT range".to_string())
    })?;
    let payload = serde_json::to_string(revision)?;
    transaction.execute(
        "INSERT INTO orchestrator_topology_revisions(topology_id, revision_number, revision_id, parent_revision_id, rollback_of_revision_id, content_sha256, payload) VALUES ($1, $2, $3, $4, $5, $6, $7::text::jsonb)",
        &[
            &revision.topology_id(),
            &revision_number,
            &revision.revision_id(),
            &revision.parent_revision_id(),
            &revision.rollback_of_revision_id(),
            &revision.content_sha256(),
            &payload,
        ],
    )?;
    Ok(())
}

fn load_revision(
    client: &mut impl GenericClient,
    topology_id: &str,
    revision_id: &str,
) -> PostgresResult<Option<TopologyRevision>> {
    client
        .query_opt(
            "SELECT payload::text FROM orchestrator_topology_revisions WHERE topology_id = $1 AND revision_id = $2",
            &[&topology_id, &revision_id],
        )?
        .map(|row| deserialize_revision(&row.get::<_, String>(0)))
        .transpose()
}

fn deserialize_revision(payload: &str) -> PostgresResult<TopologyRevision> {
    let revision: TopologyRevision = serde_json::from_str(payload)?;
    revision.verify().map_err(domain_error)?;
    Ok(revision)
}

fn deserialize_status(payload: &str) -> PostgresResult<TopologyStatus> {
    let status: TopologyStatus = serde_json::from_str(payload)?;
    status.validate().map_err(domain_error)?;
    Ok(status)
}

fn load_heads(
    client: &mut impl GenericClient,
    topology_id: &str,
    for_update: bool,
) -> PostgresResult<Option<TopologyHeads>> {
    let sql = if for_update {
        "SELECT draft_revision_id, applied_revision_id, applying_revision_id, applying_operation_id, last_operation_id FROM orchestrator_topology_heads WHERE topology_id = $1 FOR UPDATE"
    } else {
        "SELECT draft_revision_id, applied_revision_id, applying_revision_id, applying_operation_id, last_operation_id FROM orchestrator_topology_heads WHERE topology_id = $1"
    };
    Ok(client
        .query_opt(sql, &[&topology_id])?
        .map(|row| TopologyHeads {
            topology_id: topology_id.to_string(),
            draft_revision_id: row.get(0),
            applied_revision_id: row.get(1),
            applying_revision_id: row.get(2),
            applying_operation_id: row.get(3),
            last_operation_id: row.get(4),
        }))
}

fn ensure_status_revision(
    client: &mut impl GenericClient,
    topology_id: &str,
    revision_id: Option<&str>,
) -> PostgresResult<()> {
    if let Some(revision_id) = revision_id
        && load_revision(client, topology_id, revision_id)?.is_none()
    {
        return Err(PostgresError::Invariant(format!(
            "status references revision {revision_id} outside topology {topology_id}"
        )));
    }
    Ok(())
}

fn upsert_status(client: &mut impl GenericClient, status: &TopologyStatus) -> PostgresResult<()> {
    let payload = serde_json::to_string(status)?;
    client.execute(
        "INSERT INTO orchestrator_topology_status(topology_id, desired_revision_id, observed_revision_id, payload) VALUES ($1, $2, $3, $4::text::jsonb) ON CONFLICT(topology_id) DO UPDATE SET desired_revision_id = excluded.desired_revision_id, observed_revision_id = excluded.observed_revision_id, payload = excluded.payload, updated_at = clock_timestamp()",
        &[
            &status.topology_id,
            &status.desired_revision_id,
            &status.observed_revision_id,
            &payload,
        ],
    )?;
    Ok(())
}

fn domain_error(error: orchestrator_legacy::OrchestratorError) -> PostgresError {
    PostgresError::Domain(error.to_string())
}
