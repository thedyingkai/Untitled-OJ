use crate::{SqliteOrchestratorStore, StorageError, StorageResult};
use orchestrator_control_plane::{Job, JobError, JobStatus, ResolveExpiredSuccessRequest};
pub use orchestrator_core::binding_projection::TopologyApplyGroupMember;
use orchestrator_legacy::{
    ApiBindingState, TopologyReconciliationState, TopologyRevision, TopologySpec, TopologyStatus,
};
use rusqlite::{OptionalExtension, Transaction, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

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
        let validated = current
            .next(spec, created_at, created_by, message)
            .map_err(domain_error)?;
        let revision_number = next_persisted_revision_number(&transaction, topology_id)?;
        let revision = if validated.revision_number() == revision_number {
            validated
        } else {
            TopologyRevision::from_parts(
                revision_number,
                Some(current.revision_id().to_string()),
                None,
                validated.spec().clone(),
                validated.created_at(),
                validated.created_by(),
                validated.message(),
            )
            .map_err(domain_error)?
        };
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
        let validated = current
            .rollback_to(&target, created_at, created_by, message)
            .map_err(domain_error)?;
        let revision_number = next_persisted_revision_number(&transaction, topology_id)?;
        let revision = if validated.revision_number() == revision_number {
            validated
        } else {
            TopologyRevision::from_parts(
                revision_number,
                Some(current.revision_id().to_string()),
                Some(target.revision_id().to_string()),
                validated.spec().clone(),
                validated.created_at(),
                validated.created_by(),
                validated.message(),
            )
            .map_err(domain_error)?
        };
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
        let completed = finish_topology_apply_transaction(
            &transaction,
            topology_id,
            revision_id,
            operation_id,
            outcome,
            updated_at,
        )?;
        transaction.commit()?;
        Ok(completed)
    }

    /// Completes a topology apply only while the exact durable Job lease is
    /// still current. The Job fence and topology-head CAS share one immediate
    /// transaction, so lease recovery cannot race a late apply into visibility.
    #[allow(clippy::too_many_arguments)]
    pub fn finish_topology_apply_fenced(
        &self,
        topology_id: &str,
        revision_id: &str,
        operation_id: &str,
        outcome: TopologyApplyOutcome,
        updated_at: &str,
        job_id: &str,
        lease_token: &str,
        now_ms: i64,
    ) -> StorageResult<TopologyHeads> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        ensure_active_topology_job_lease(&transaction, job_id, operation_id, lease_token, now_ms)?;
        let completed = finish_topology_apply_transaction(
            &transaction,
            topology_id,
            revision_id,
            operation_id,
            outcome,
            updated_at,
        )?;
        transaction.commit()?;
        Ok(completed)
    }

    /// Commits every member of one deployment-wide binding generation in one
    /// database transaction. No applied head or active binding can become
    /// visible unless every member still owns its exact apply CAS.
    pub fn finish_topology_apply_group(
        &self,
        members: &[TopologyApplyGroupMember],
        operation_id: &str,
        updated_at: &str,
    ) -> StorageResult<Vec<TopologyHeads>> {
        validate_apply_group(members, operation_id)?;
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let completed = finish_topology_apply_group_transaction(
            &transaction,
            members,
            operation_id,
            updated_at,
        )?;
        transaction.commit()?;
        Ok(completed)
    }

    /// Atomically validates the finalizer Job lease and publishes every member
    /// of a deployment-wide Binding generation.
    pub fn finish_topology_apply_group_fenced(
        &self,
        members: &[TopologyApplyGroupMember],
        operation_id: &str,
        updated_at: &str,
        job_id: &str,
        lease_token: &str,
        now_ms: i64,
    ) -> StorageResult<Vec<TopologyHeads>> {
        validate_apply_group(members, operation_id)?;
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        ensure_active_topology_job_lease(&transaction, job_id, operation_id, lease_token, now_ms)?;
        let completed = finish_topology_apply_group_transaction(
            &transaction,
            members,
            operation_id,
            updated_at,
        )?;
        transaction.commit()?;
        Ok(completed)
    }

    /// Resolves an expired topology-finalizer Job only when every member's
    /// durable head proves that the whole group committed. The tentative Job
    /// success is rolled back when any head is absent, still applying, or no
    /// longer belongs to the expected Operation.
    pub fn resolve_expired_topology_apply_group_success(
        &self,
        members: &[TopologyApplyGroupMember],
        operation_id: &str,
        job_id: &str,
        now_ms: i64,
        result: serde_json::Value,
    ) -> StorageResult<Option<Job>> {
        validate_apply_group(members, operation_id)?;
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let job = crate::jobs::resolve_expired_success_in_transaction(
            &transaction,
            &ResolveExpiredSuccessRequest {
                job_id: job_id.to_string(),
                now_ms,
                result,
            },
        )
        .map_err(topology_job_resolution_error)?;
        if job.job_id != job_id || job.operation_id != operation_id {
            return Ok(None);
        }
        let mut ordered_members = members.iter().collect::<Vec<_>>();
        ordered_members.sort_by(|left, right| left.topology_id.cmp(&right.topology_id));
        for member in ordered_members {
            let Some(heads) = load_heads(&transaction, &member.topology_id)? else {
                return Ok(None);
            };
            if heads.applying_revision_id.is_some()
                || heads.applying_operation_id.is_some()
                || heads.applied_revision_id.as_deref() != Some(member.revision_id.as_str())
                || heads.last_operation_id.as_deref() != Some(operation_id)
            {
                return Ok(None);
            }
        }
        transaction.commit()?;
        Ok(Some(job))
    }

    /// Compensate a revision that already reached the applied head as one
    /// member of a grouped saga. The CAS includes the candidate head and the
    /// originating Operation so a late compensation can never rewind a newer
    /// user apply.
    pub fn compensate_completed_topology_apply(
        &self,
        topology_id: &str,
        revision_id: &str,
        previous_revision_id: &str,
        operation_id: &str,
        updated_at: &str,
    ) -> StorageResult<TopologyHeads> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        ensure_compensation_revisions(
            &transaction,
            topology_id,
            revision_id,
            previous_revision_id,
            operation_id,
        )?;
        let changed = transaction.execute(
            "UPDATE orchestrator_topology_heads SET draft_revision_id = ?3, applied_revision_id = ?3, applying_revision_id = NULL, applying_operation_id = NULL, last_operation_id = ?4, updated_at = unixepoch() WHERE topology_id = ?1 AND draft_revision_id = ?2 AND applied_revision_id = ?2 AND applying_revision_id IS NULL AND applying_operation_id IS NULL AND last_operation_id = ?4",
            params![topology_id, revision_id, previous_revision_id, operation_id],
        )?;
        if changed != 1 {
            return Err(StorageError::Conflict(format!(
                "topology {topology_id} completed apply no longer belongs to operation {operation_id}"
            )));
        }
        upsert_compensated_status(
            &transaction,
            topology_id,
            previous_revision_id,
            operation_id,
            updated_at,
        )?;
        let completed = load_heads(&transaction, topology_id)?.ok_or_else(|| {
            StorageError::Invariant(format!("topology {topology_id} head disappeared"))
        })?;
        transaction.commit()?;
        Ok(completed)
    }

    /// Atomically publishes a successful provider/binding ABORT while the
    /// candidate still owns the apply lease. A normal failed apply deliberately
    /// keeps its draft retryable; only this explicit compensated transition
    /// rewinds both durable heads to the previously applied revision.
    pub fn complete_compensated_topology_abort(
        &self,
        topology_id: &str,
        candidate_revision_id: &str,
        previous_revision_id: &str,
        operation_id: &str,
        updated_at: &str,
    ) -> StorageResult<TopologyHeads> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        ensure_compensation_revisions(
            &transaction,
            topology_id,
            candidate_revision_id,
            previous_revision_id,
            operation_id,
        )?;
        let changed = transaction.execute(
            "UPDATE orchestrator_topology_heads SET draft_revision_id = ?3, applied_revision_id = ?3, applying_revision_id = NULL, applying_operation_id = NULL, last_operation_id = ?4, updated_at = unixepoch() WHERE topology_id = ?1 AND draft_revision_id = ?2 AND applying_revision_id = ?2 AND applying_operation_id = ?4 AND applied_revision_id = ?3",
            params![
                topology_id,
                candidate_revision_id,
                previous_revision_id,
                operation_id
            ],
        )?;
        if changed != 1 {
            return Err(StorageError::Conflict(format!(
                "topology {topology_id} compensated abort no longer owns candidate {candidate_revision_id} for operation {operation_id}"
            )));
        }
        upsert_compensated_status(
            &transaction,
            topology_id,
            previous_revision_id,
            operation_id,
            updated_at,
        )?;
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

fn ensure_active_topology_job_lease(
    transaction: &Transaction<'_>,
    job_id: &str,
    operation_id: &str,
    lease_token: &str,
    now_ms: i64,
) -> StorageResult<()> {
    if job_id.trim().is_empty() || lease_token.trim().is_empty() {
        return Err(StorageError::Invariant(
            "job_id and lease_token must not be empty".to_string(),
        ));
    }
    let row = transaction
        .query_row(
            "SELECT status, lease_expires_at_ms, payload FROM orchestrator_jobs WHERE job_id = ?1",
            [job_id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Option<i64>>(1)?,
                    row.get::<_, String>(2)?,
                ))
            },
        )
        .optional()?;
    let Some((stored_status, stored_expiry, payload)) = row else {
        return Err(stale_topology_job_lease(job_id));
    };
    let job: Job = serde_json::from_str(&payload)?;
    let active_status = matches!(job.status, JobStatus::Leased | JobStatus::CancelRequested);
    let stored_status_matches = matches!(stored_status.as_str(), "LEASED" | "CANCEL_REQUESTED")
        && stored_status == topology_job_status_label(&job.status);
    let active_expiry = job
        .lease_expires_at_ms
        .is_some_and(|lease_expires_at_ms| lease_expires_at_ms > now_ms);
    if job.job_id != job_id
        || job.operation_id != operation_id
        || !active_status
        || !stored_status_matches
        || job.lease_token.as_deref() != Some(lease_token)
        || job.lease_expires_at_ms != stored_expiry
        || !active_expiry
    {
        return Err(stale_topology_job_lease(job_id));
    }
    Ok(())
}

fn stale_topology_job_lease(job_id: &str) -> StorageError {
    StorageError::Conflict(format!(
        "topology apply job {job_id} lease is stale or does not match"
    ))
}

fn topology_job_resolution_error(error: JobError) -> StorageError {
    match error {
        JobError::Persistence(message) => StorageError::Invariant(message),
        other => StorageError::Conflict(other.to_string()),
    }
}

fn topology_job_status_label(status: &JobStatus) -> &'static str {
    match status {
        JobStatus::Queued => "QUEUED",
        JobStatus::Leased => "LEASED",
        JobStatus::RetryWait => "RETRY_WAIT",
        JobStatus::CancelRequested => "CANCEL_REQUESTED",
        JobStatus::Succeeded => "SUCCEEDED",
        JobStatus::Failed => "FAILED",
        JobStatus::Cancelled => "CANCELLED",
        JobStatus::NeedsAttention => "NEEDS_ATTENTION",
    }
}

fn finish_topology_apply_transaction(
    transaction: &Transaction<'_>,
    topology_id: &str,
    revision_id: &str,
    operation_id: &str,
    outcome: TopologyApplyOutcome,
    updated_at: &str,
) -> StorageResult<TopologyHeads> {
    let heads = load_heads(transaction, topology_id)?
        .ok_or_else(|| StorageError::Invariant(format!("topology {topology_id} does not exist")))?;
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
    upsert_status(transaction, &status)?;
    load_heads(transaction, topology_id)?
        .ok_or_else(|| StorageError::Invariant(format!("topology {topology_id} head disappeared")))
}

fn finish_topology_apply_group_transaction(
    transaction: &Transaction<'_>,
    members: &[TopologyApplyGroupMember],
    operation_id: &str,
    updated_at: &str,
) -> StorageResult<Vec<TopologyHeads>> {
    for member in members {
        let heads = load_heads(transaction, &member.topology_id)?.ok_or_else(|| {
            StorageError::Invariant(format!("topology {} does not exist", member.topology_id))
        })?;
        if heads.applying_revision_id.as_deref() != Some(member.revision_id.as_str())
            || heads.applying_operation_id.as_deref() != Some(operation_id)
        {
            return Err(StorageError::Conflict(format!(
                "topology {} group apply lease does not match operation {operation_id}",
                member.topology_id
            )));
        }
    }
    // Delete all owned projections before inserting any, so a requirement
    // moved between sibling topologies never trips the database-wide
    // (consumer, requirement) unique constraint midway through the group.
    for member in members {
        transaction.execute(
            "DELETE FROM orchestrator_api_bindings WHERE topology_id = ?1",
            [&member.topology_id],
        )?;
    }
    for member in members {
        for binding in &member.active_bindings {
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
                    binding_state_label(binding.derived_state()),
                    serde_json::to_string(binding)?,
                ],
            )?;
        }
    }
    let mut completed = Vec::with_capacity(members.len());
    for member in members {
        let changed = transaction.execute(
            "UPDATE orchestrator_topology_heads SET applied_revision_id = ?2, applying_revision_id = NULL, applying_operation_id = NULL, last_operation_id = ?3, updated_at = unixepoch() WHERE topology_id = ?1 AND applying_revision_id = ?2 AND applying_operation_id = ?3",
            params![member.topology_id, member.revision_id, operation_id],
        )?;
        if changed != 1 {
            return Err(StorageError::Conflict(format!(
                "topology {} group completion raced with another writer",
                member.topology_id
            )));
        }
        let status = TopologyStatus {
            topology_id: member.topology_id.clone(),
            desired_revision_id: Some(member.revision_id.clone()),
            observed_revision_id: Some(member.revision_id.clone()),
            state: TopologyReconciliationState::InSync,
            deployments: Vec::new(),
            endpoints: Vec::new(),
            links: Vec::new(),
            drift: Vec::new(),
            last_operation_id: Some(operation_id.to_string()),
            updated_at: updated_at.to_string(),
        };
        status.validate().map_err(domain_error)?;
        upsert_status(transaction, &status)?;
        completed.push(
            load_heads(transaction, &member.topology_id)?.ok_or_else(|| {
                StorageError::Invariant(format!("topology {} head disappeared", member.topology_id))
            })?,
        );
    }
    Ok(completed)
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

fn next_persisted_revision_number(
    transaction: &Transaction<'_>,
    topology_id: &str,
) -> StorageResult<u64> {
    let maximum = transaction.query_row(
        "SELECT MAX(revision_number) FROM orchestrator_topology_revisions WHERE topology_id = ?1",
        [topology_id],
        |row| row.get::<_, Option<i64>>(0),
    )?;
    let maximum = maximum.ok_or_else(|| {
        StorageError::Invariant(format!("topology {topology_id} has no revision history"))
    })?;
    u64::try_from(maximum)
        .map_err(|_| StorageError::Invariant("negative topology revision number".to_string()))?
        .checked_add(1)
        .ok_or_else(|| StorageError::Invariant("topology revision number overflow".to_string()))
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

fn ensure_compensation_revisions(
    transaction: &Transaction<'_>,
    topology_id: &str,
    candidate_revision_id: &str,
    previous_revision_id: &str,
    operation_id: &str,
) -> StorageResult<()> {
    if topology_id.trim().is_empty()
        || candidate_revision_id.trim().is_empty()
        || previous_revision_id.trim().is_empty()
        || operation_id.trim().is_empty()
        || candidate_revision_id == previous_revision_id
    {
        return Err(StorageError::Invariant(
            "compensated topology abort requires distinct non-empty candidate/previous revisions and a non-empty operation_id"
                .to_string(),
        ));
    }
    if load_revision(transaction, topology_id, candidate_revision_id)?.is_none() {
        return Err(StorageError::Invariant(format!(
            "candidate topology revision {candidate_revision_id} is missing"
        )));
    }
    if load_revision(transaction, topology_id, previous_revision_id)?.is_none() {
        return Err(StorageError::Invariant(format!(
            "previous topology revision {previous_revision_id} is missing; an initial topology apply cannot be automatically rewound"
        )));
    }
    Ok(())
}

fn upsert_compensated_status(
    transaction: &Transaction<'_>,
    topology_id: &str,
    previous_revision_id: &str,
    operation_id: &str,
    updated_at: &str,
) -> StorageResult<()> {
    let status = TopologyStatus {
        topology_id: topology_id.to_string(),
        desired_revision_id: Some(previous_revision_id.to_string()),
        observed_revision_id: Some(previous_revision_id.to_string()),
        state: TopologyReconciliationState::InSync,
        deployments: Vec::new(),
        endpoints: Vec::new(),
        links: Vec::new(),
        drift: Vec::new(),
        last_operation_id: Some(operation_id.to_string()),
        updated_at: updated_at.to_string(),
    };
    status.validate().map_err(domain_error)?;
    upsert_status(transaction, &status)
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

fn validate_apply_group(
    members: &[TopologyApplyGroupMember],
    operation_id: &str,
) -> StorageResult<()> {
    if members.is_empty() || operation_id.trim().is_empty() {
        return Err(StorageError::Invariant(
            "topology apply group and operation_id must not be empty".to_string(),
        ));
    }
    let mut topology_ids = BTreeSet::new();
    let mut binding_ids = BTreeSet::new();
    let mut requirements = BTreeSet::new();
    for member in members {
        if member.topology_id.trim().is_empty()
            || member.revision_id.trim().is_empty()
            || !topology_ids.insert(member.topology_id.as_str())
        {
            return Err(StorageError::Invariant(
                "topology apply group members must have unique non-empty identities".to_string(),
            ));
        }
        for binding in &member.active_bindings {
            binding
                .validate()
                .map_err(|error| StorageError::Invariant(error.to_string()))?;
            let terminal_state_matches = match binding.desired_state.as_str() {
                "ACTIVE" => {
                    binding.state == ApiBindingState::Active
                        && binding.observed_state == "ACTIVE"
                        && binding.health == "HEALTHY"
                }
                "REVOKED" => {
                    binding.state == ApiBindingState::Revoked && binding.observed_state == "REVOKED"
                }
                _ => false,
            };
            if binding.topology_id != member.topology_id
                || binding.topology_revision_id != member.revision_id
                || !terminal_state_matches
                || binding.last_operation_id != operation_id
                || binding.credential_generation != binding.context_generation
                || !binding_ids.insert(binding.binding_id.as_str())
                || !requirements.insert((
                    binding.consumer_deployment_id.as_str(),
                    binding.requirement_name.as_str(),
                ))
            {
                return Err(StorageError::Invariant(
                    "topology apply group contains a mismatched, non-terminal, unstaged, or duplicate binding"
                        .to_string(),
                ));
            }
        }
    }
    Ok(())
}

fn binding_state_label(state: ApiBindingState) -> &'static str {
    match state {
        ApiBindingState::Pending => "PENDING",
        ApiBindingState::Resolved => "RESOLVED",
        ApiBindingState::Active => "ACTIVE",
        ApiBindingState::Unbound => "UNBOUND",
        ApiBindingState::Revoked => "REVOKED",
        ApiBindingState::Error => "ERROR",
    }
}
