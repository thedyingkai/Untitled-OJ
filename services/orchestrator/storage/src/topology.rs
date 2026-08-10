use crate::{SqliteOrchestratorStore, StorageError, StorageResult};
use orchestrator_control_plane::{Job, JobError, JobStatus, ResolveExpiredSuccessRequest};
use orchestrator_legacy::{
    ApiBinding, ApiBindingState, TopologyReconciliationState, TopologyRevision, TopologySpec,
    TopologyStatus,
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TopologyApplyGroupMember {
    pub topology_id: String,
    pub revision_id: String,
    pub active_bindings: Vec<ApiBinding>,
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
        if load_revision(&transaction, topology_id, previous_revision_id)?.is_none() {
            return Err(StorageError::Invariant(format!(
                "previous topology revision {previous_revision_id} is missing"
            )));
        }
        let changed = transaction.execute(
            "UPDATE orchestrator_topology_heads SET applied_revision_id = ?3, last_operation_id = ?4, updated_at = unixepoch() WHERE topology_id = ?1 AND applied_revision_id = ?2 AND applying_revision_id IS NULL AND last_operation_id = ?4",
            params![topology_id, revision_id, previous_revision_id, operation_id],
        )?;
        if changed != 1 {
            return Err(StorageError::Conflict(format!(
                "topology {topology_id} completed apply no longer belongs to operation {operation_id}"
            )));
        }
        let status = TopologyStatus {
            topology_id: topology_id.to_string(),
            desired_revision_id: Some(revision_id.to_string()),
            observed_revision_id: Some(previous_revision_id.to_string()),
            state: TopologyReconciliationState::Failed,
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
                    binding_state_label(binding.state),
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::SqliteJobStore;
    use orchestrator_control_plane::{
        ClaimRequest, CompleteRequest, CompletionStatus, JobKind, JobStore, NewJob,
    };
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
                api_bindings: Vec::new(),
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

    fn finalized_binding(
        binding_id: &str,
        requirement_name: &str,
        consumer_deployment_id: &str,
        topology_id: &str,
        revision_id: &str,
        operation_id: &str,
    ) -> ApiBinding {
        ApiBinding {
            binding_id: binding_id.to_string(),
            requirement_name: requirement_name.to_string(),
            api_id: format!("fixture.{requirement_name}"),
            api_version: "1.0.0".to_string(),
            consumer_deployment_id: consumer_deployment_id.to_string(),
            consumer_service_id: "fixture-consumer".to_string(),
            consumer_node_id: "node-consumer".to_string(),
            consumer_endpoint: "10.0.0.2:9000:consumer".to_string(),
            provider_deployment_id: format!("provider-{binding_id}"),
            provider_service_id: "fixture-provider".to_string(),
            provider_node_id: "node-provider".to_string(),
            provider_endpoint: "10.0.0.1:8080:provider".to_string(),
            provider_path: format!("/{requirement_name}"),
            virtual_endpoint: format!("/internal/apis/fixture.{requirement_name}"),
            protocol: "http".to_string(),
            methods: vec!["GET".to_string()],
            auth_mode: "workload".to_string(),
            provider_auth_mode: "workload".to_string(),
            permission: format!("fixture.{requirement_name}"),
            timeout_ms: Some(5_000),
            topology_id: topology_id.to_string(),
            topology_revision_id: revision_id.to_string(),
            link_source_endpoint: "10.0.0.2:9000:consumer".to_string(),
            link_target_endpoint: "10.0.0.1:8080:provider".to_string(),
            credential_ref: String::new(),
            credential_generation: 2,
            context_generation: 2,
            desired_state: "ACTIVE".to_string(),
            observed_state: "ACTIVE".to_string(),
            health: "HEALTHY".to_string(),
            drift: Vec::new(),
            last_operation_id: operation_id.to_string(),
            state: ApiBindingState::Active,
            optional: false,
            reason: String::new(),
            created_at: "unix-ms:1".to_string(),
            updated_at: "unix-ms:2".to_string(),
        }
    }

    fn lease_job(
        store: &SqliteOrchestratorStore,
        job_id: &str,
        operation_id: &str,
        lease_token: &str,
        now_ms: i64,
        lease_ms: i64,
    ) {
        let mut jobs = SqliteJobStore::new(store.clone());
        jobs.enqueue(
            NewJob {
                job_id: job_id.to_string(),
                operation_id: operation_id.to_string(),
                node_id: format!("node-{job_id}"),
                kind: JobKind::TopologyApply,
                payload: json!({"topology_finalizer": true}),
                idempotency_key: format!("idempotency-{job_id}"),
                max_attempts: 3,
            },
            now_ms,
        )
        .unwrap();
        jobs.claim(ClaimRequest {
            node_id: format!("node-{job_id}"),
            instance_id: "worker-1".to_string(),
            lease_token: lease_token.to_string(),
            now_ms,
            lease_ms,
        })
        .unwrap()
        .unwrap();
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
    fn fenced_apply_rejects_wrong_expired_and_stale_job_leases() {
        let (_directory, store) = store();
        let revision = store
            .create_initial_topology_revision(spec("primary", "first"), "t1", "admin", "initial")
            .unwrap();
        store
            .begin_topology_apply("primary", revision.revision_id(), "op-1", "t2")
            .unwrap();
        lease_job(&store, "job-1", "op-1", "lease-1", 100, 100);

        assert!(matches!(
            store.finish_topology_apply_fenced(
                "primary",
                revision.revision_id(),
                "op-1",
                TopologyApplyOutcome::Succeeded,
                "t3",
                "job-1",
                "wrong-token",
                150,
            ),
            Err(StorageError::Conflict(_))
        ));
        let completed = store
            .finish_topology_apply_fenced(
                "primary",
                revision.revision_id(),
                "op-1",
                TopologyApplyOutcome::Succeeded,
                "t4",
                "job-1",
                "lease-1",
                150,
            )
            .unwrap();
        assert_eq!(
            completed.applied_revision_id.as_deref(),
            Some(revision.revision_id())
        );

        let expired = store
            .create_initial_topology_revision(spec("expired", "expired"), "t5", "admin", "initial")
            .unwrap();
        store
            .begin_topology_apply("expired", expired.revision_id(), "op-expired", "t6")
            .unwrap();
        lease_job(
            &store,
            "job-expired",
            "op-expired",
            "expired-lease",
            100,
            100,
        );
        assert!(matches!(
            store.finish_topology_apply_fenced(
                "expired",
                expired.revision_id(),
                "op-expired",
                TopologyApplyOutcome::Succeeded,
                "t7",
                "job-expired",
                "expired-lease",
                200,
            ),
            Err(StorageError::Conflict(_))
        ));
        let heads = store.topology_heads("expired").unwrap().unwrap();
        assert!(heads.applied_revision_id.is_none());
        assert_eq!(heads.applying_operation_id.as_deref(), Some("op-expired"));

        let terminal = store
            .create_initial_topology_revision(
                spec("terminal", "terminal"),
                "t8",
                "admin",
                "initial",
            )
            .unwrap();
        store
            .begin_topology_apply("terminal", terminal.revision_id(), "op-terminal", "t9")
            .unwrap();
        lease_job(
            &store,
            "job-terminal",
            "op-terminal",
            "terminal-lease",
            300,
            100,
        );
        SqliteJobStore::new(store.clone())
            .complete(CompleteRequest {
                job_id: "job-terminal".to_string(),
                lease_token: "terminal-lease".to_string(),
                status: CompletionStatus::Succeeded,
                result: json!({"completed_without_head": true}),
                error_message: String::new(),
                now_ms: 350,
                events: Vec::new(),
            })
            .unwrap();
        assert!(matches!(
            store.finish_topology_apply_fenced(
                "terminal",
                terminal.revision_id(),
                "op-terminal",
                TopologyApplyOutcome::Succeeded,
                "t10",
                "job-terminal",
                "terminal-lease",
                351,
            ),
            Err(StorageError::Conflict(_))
        ));
        let heads = store.topology_heads("terminal").unwrap().unwrap();
        assert!(heads.applied_revision_id.is_none());
        assert_eq!(heads.applying_operation_id.as_deref(), Some("op-terminal"));
    }

    #[test]
    fn fenced_group_rejects_bad_lease_without_advancing_any_head() {
        let (_directory, store) = store();
        let first = store
            .create_initial_topology_revision(spec("first", "first"), "t1", "admin", "initial")
            .unwrap();
        let second = store
            .create_initial_topology_revision(spec("second", "second"), "t1", "admin", "initial")
            .unwrap();
        for (topology_id, revision) in [("first", &first), ("second", &second)] {
            store
                .begin_topology_apply(topology_id, revision.revision_id(), "op-group", "t2")
                .unwrap();
        }
        lease_job(&store, "job-group", "op-group", "group-lease", 500, 100);
        let members = vec![
            TopologyApplyGroupMember {
                topology_id: "second".to_string(),
                revision_id: second.revision_id().to_string(),
                active_bindings: vec![],
            },
            TopologyApplyGroupMember {
                topology_id: "first".to_string(),
                revision_id: first.revision_id().to_string(),
                active_bindings: vec![],
            },
        ];

        assert!(matches!(
            store.finish_topology_apply_group_fenced(
                &members,
                "op-group",
                "t3",
                "job-group",
                "bad-lease",
                550,
            ),
            Err(StorageError::Conflict(_))
        ));
        for topology_id in ["first", "second"] {
            let heads = store.topology_heads(topology_id).unwrap().unwrap();
            assert!(heads.applied_revision_id.is_none());
            assert_eq!(heads.applying_operation_id.as_deref(), Some("op-group"));
        }

        store
            .finish_topology_apply_group_fenced(
                &members,
                "op-group",
                "t4",
                "job-group",
                "group-lease",
                550,
            )
            .unwrap();
        for topology_id in ["first", "second"] {
            let heads = store.topology_heads(topology_id).unwrap().unwrap();
            assert!(heads.applied_revision_id.is_some());
            assert!(heads.applying_operation_id.is_none());
        }
    }

    #[test]
    fn expired_group_resolution_commits_job_only_after_every_head_matches() {
        let (_directory, store) = store();
        let first = store
            .create_initial_topology_revision(spec("first", "first"), "t1", "admin", "initial")
            .unwrap();
        let second = store
            .create_initial_topology_revision(spec("second", "second"), "t1", "admin", "initial")
            .unwrap();
        for (topology_id, revision) in [("first", &first), ("second", &second)] {
            store
                .begin_topology_apply(topology_id, revision.revision_id(), "op-group", "t2")
                .unwrap();
        }
        lease_job(&store, "job-group", "op-group", "group-lease", 500, 100);
        let members = vec![
            TopologyApplyGroupMember {
                topology_id: "second".to_string(),
                revision_id: second.revision_id().to_string(),
                active_bindings: vec![],
            },
            TopologyApplyGroupMember {
                topology_id: "first".to_string(),
                revision_id: first.revision_id().to_string(),
                active_bindings: vec![],
            },
        ];
        let result = json!({"durable_evidence": "all-group-heads-applied"});

        store
            .finish_topology_apply(
                "first",
                first.revision_id(),
                "op-group",
                TopologyApplyOutcome::Succeeded,
                "t3",
            )
            .unwrap();
        assert!(
            store
                .resolve_expired_topology_apply_group_success(
                    &members,
                    "op-group",
                    "job-group",
                    600,
                    result.clone(),
                )
                .unwrap()
                .is_none()
        );
        assert_eq!(
            SqliteJobStore::new(store.clone())
                .get("job-group")
                .unwrap()
                .unwrap()
                .status,
            JobStatus::Leased
        );

        store
            .finish_topology_apply(
                "second",
                second.revision_id(),
                "op-group",
                TopologyApplyOutcome::Succeeded,
                "t4",
            )
            .unwrap();
        let resolved = store
            .resolve_expired_topology_apply_group_success(
                &members,
                "op-group",
                "job-group",
                600,
                result.clone(),
            )
            .unwrap()
            .unwrap();
        assert_eq!(resolved.status, JobStatus::Succeeded);
        assert_eq!(resolved.result, Some(result));
    }

    #[test]
    fn grouped_apply_advances_all_heads_or_none() {
        let (_directory, store) = store();
        let first = store
            .create_initial_topology_revision(spec("first", "first"), "t1", "admin", "initial")
            .unwrap();
        let second = store
            .create_initial_topology_revision(spec("second", "second"), "t1", "admin", "initial")
            .unwrap();
        store
            .begin_topology_apply("first", first.revision_id(), "op-group", "t2")
            .unwrap();
        store
            .begin_topology_apply("second", second.revision_id(), "op-group", "t2")
            .unwrap();
        let mut members = vec![
            TopologyApplyGroupMember {
                topology_id: "first".to_string(),
                revision_id: first.revision_id().to_string(),
                active_bindings: vec![],
            },
            TopologyApplyGroupMember {
                topology_id: "second".to_string(),
                revision_id: "stale-revision".to_string(),
                active_bindings: vec![],
            },
        ];
        assert!(
            store
                .finish_topology_apply_group(&members, "op-group", "t3")
                .is_err()
        );
        for topology_id in ["first", "second"] {
            let heads = store.topology_heads(topology_id).unwrap().unwrap();
            assert!(heads.applied_revision_id.is_none());
            assert_eq!(heads.applying_operation_id.as_deref(), Some("op-group"));
        }
        members[1].revision_id = second.revision_id().to_string();
        let completed = store
            .finish_topology_apply_group(&members, "op-group", "t4")
            .unwrap();
        assert_eq!(completed.len(), 2);
        assert_eq!(
            store
                .topology_heads("first")
                .unwrap()
                .unwrap()
                .applied_revision_id
                .as_deref(),
            Some(first.revision_id())
        );
        assert_eq!(
            store
                .topology_heads("second")
                .unwrap()
                .unwrap()
                .applied_revision_id
                .as_deref(),
            Some(second.revision_id())
        );
    }

    #[test]
    fn grouped_apply_accepts_only_current_terminal_unique_bindings() {
        let (_directory, store) = store();
        let first = store
            .create_initial_topology_revision(
                spec("first-active", "first"),
                "t1",
                "admin",
                "initial",
            )
            .unwrap();
        let second = store
            .create_initial_topology_revision(
                spec("second-active", "second"),
                "t1",
                "admin",
                "initial",
            )
            .unwrap();
        let operation_id = "op-active-group";
        for (topology_id, revision) in [("first-active", &first), ("second-active", &second)] {
            store
                .begin_topology_apply(topology_id, revision.revision_id(), operation_id, "t2")
                .unwrap();
        }
        let mut revoked_echo = finalized_binding(
            "binding-first-revoked",
            "removed_echo",
            "consumer-first",
            "first-active",
            first.revision_id(),
            operation_id,
        );
        revoked_echo.desired_state = "REVOKED".to_string();
        revoked_echo.observed_state = "REVOKED".to_string();
        revoked_echo.health = "UNKNOWN".to_string();
        revoked_echo.state = ApiBindingState::Revoked;
        revoked_echo.optional = true;
        let members = vec![
            TopologyApplyGroupMember {
                topology_id: "first-active".to_string(),
                revision_id: first.revision_id().to_string(),
                active_bindings: vec![
                    finalized_binding(
                        "binding-first-active",
                        "echo",
                        "consumer-first",
                        "first-active",
                        first.revision_id(),
                        operation_id,
                    ),
                    revoked_echo,
                ],
            },
            TopologyApplyGroupMember {
                topology_id: "second-active".to_string(),
                revision_id: second.revision_id().to_string(),
                active_bindings: vec![finalized_binding(
                    "binding-second-active",
                    "permission_check",
                    "consumer-second",
                    "second-active",
                    second.revision_id(),
                    operation_id,
                )],
            },
        ];

        let mut unstaged = members.clone();
        unstaged[0].active_bindings[0].last_operation_id = "op-older".to_string();
        assert!(matches!(
            store.finish_topology_apply_group(&unstaged, operation_id, "t3"),
            Err(StorageError::Invariant(_))
        ));

        let mut pending = members.clone();
        let pending_binding = &mut pending[0].active_bindings[0];
        pending_binding.observed_state = "PENDING".to_string();
        pending_binding.health = "UNKNOWN".to_string();
        pending_binding.state = ApiBindingState::Pending;
        assert!(matches!(
            store.finish_topology_apply_group(&pending, operation_id, "t3"),
            Err(StorageError::Invariant(_))
        ));

        let mut duplicate = members.clone();
        duplicate[1].active_bindings[0].binding_id =
            duplicate[0].active_bindings[0].binding_id.clone();
        assert!(matches!(
            store.finish_topology_apply_group(&duplicate, operation_id, "t3"),
            Err(StorageError::Invariant(_))
        ));

        for topology_id in ["first-active", "second-active"] {
            let heads = store.topology_heads(topology_id).unwrap().unwrap();
            assert!(heads.applied_revision_id.is_none());
            assert_eq!(heads.applying_operation_id.as_deref(), Some(operation_id));
            assert!(
                store
                    .api_bindings_for_topology(topology_id)
                    .unwrap()
                    .is_empty()
            );
        }

        store
            .finish_topology_apply_group(&members, operation_id, "t4")
            .unwrap();
        for member in &members {
            let heads = store.topology_heads(&member.topology_id).unwrap().unwrap();
            assert_eq!(
                heads.applied_revision_id.as_deref(),
                Some(member.revision_id.as_str())
            );
            assert_eq!(
                store
                    .api_bindings_for_topology(&member.topology_id)
                    .unwrap(),
                member.active_bindings
            );
        }
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
