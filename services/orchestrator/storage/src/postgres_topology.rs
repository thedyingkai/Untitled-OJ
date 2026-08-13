use crate::{
    PostgresError, PostgresOrchestratorStore, PostgresResult, TopologyApplyGroupMember,
    TopologyApplyOutcome, TopologyHeads, postgres_api_bindings::lock_api_binding_mutations,
};
use orchestrator_control_plane::{Job, JobError, JobStatus, ResolveExpiredSuccessRequest};
use orchestrator_legacy::{
    ApiBindingState, TopologyReconciliationState, TopologyRevision, TopologySpec, TopologyStatus,
};
use r2d2_postgres::postgres::{GenericClient, Transaction};
use std::collections::BTreeSet;

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
        let validated = current
            .next(spec, created_at, created_by, message)
            .map_err(domain_error)?;
        let revision_number = next_persisted_revision_number(&mut transaction, topology_id)?;
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
        let validated = current
            .rollback_to(&target, created_at, created_by, message)
            .map_err(domain_error)?;
        let revision_number = next_persisted_revision_number(&mut transaction, topology_id)?;
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
        let completed = finish_topology_apply_transaction(
            &mut transaction,
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
    /// still current. PostgreSQL locks the Job row before the topology head.
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
    ) -> PostgresResult<TopologyHeads> {
        let mut connection = self.pool().connection()?;
        let mut transaction = connection.transaction()?;
        ensure_active_topology_job_lease(
            &mut transaction,
            job_id,
            operation_id,
            lease_token,
            now_ms,
        )?;
        let completed = finish_topology_apply_transaction(
            &mut transaction,
            topology_id,
            revision_id,
            operation_id,
            outcome,
            updated_at,
        )?;
        transaction.commit()?;
        Ok(completed)
    }

    pub fn finish_topology_apply_group(
        &self,
        members: &[TopologyApplyGroupMember],
        operation_id: &str,
        updated_at: &str,
    ) -> PostgresResult<Vec<TopologyHeads>> {
        validate_apply_group(members, operation_id)?;
        let mut connection = self.pool().connection()?;
        let mut transaction = connection.transaction()?;
        let completed = finish_topology_apply_group_transaction(
            &mut transaction,
            members,
            operation_id,
            updated_at,
        )?;
        transaction.commit()?;
        Ok(completed)
    }

    /// Atomically validates the finalizer Job lease and publishes every member
    /// of a deployment-wide Binding generation. Job-first and sorted head locks
    /// give every fenced caller the same PostgreSQL lock order.
    pub fn finish_topology_apply_group_fenced(
        &self,
        members: &[TopologyApplyGroupMember],
        operation_id: &str,
        updated_at: &str,
        job_id: &str,
        lease_token: &str,
        now_ms: i64,
    ) -> PostgresResult<Vec<TopologyHeads>> {
        validate_apply_group(members, operation_id)?;
        let mut connection = self.pool().connection()?;
        let mut transaction = connection.transaction()?;
        ensure_active_topology_job_lease(
            &mut transaction,
            job_id,
            operation_id,
            lease_token,
            now_ms,
        )?;
        let completed = finish_topology_apply_group_transaction(
            &mut transaction,
            members,
            operation_id,
            updated_at,
        )?;
        transaction.commit()?;
        Ok(completed)
    }

    /// Resolves an expired topology-finalizer Job only when every member's
    /// durable head proves that the whole group committed. PostgreSQL locks the
    /// Job first and then every topology head in stable topology-id order.
    pub fn resolve_expired_topology_apply_group_success(
        &self,
        members: &[TopologyApplyGroupMember],
        operation_id: &str,
        job_id: &str,
        now_ms: i64,
        result: serde_json::Value,
    ) -> PostgresResult<Option<Job>> {
        validate_apply_group(members, operation_id)?;
        let mut connection = self.pool().connection()?;
        let mut transaction = connection.transaction()?;
        let job = crate::postgres_jobs::resolve_expired_success_in_transaction(
            &mut transaction,
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
            let Some(heads) = load_heads(&mut transaction, &member.topology_id, true)? else {
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

    pub fn compensate_completed_topology_apply(
        &self,
        topology_id: &str,
        revision_id: &str,
        previous_revision_id: &str,
        operation_id: &str,
        updated_at: &str,
    ) -> PostgresResult<TopologyHeads> {
        let mut connection = self.pool().connection()?;
        let mut transaction = connection.transaction()?;
        ensure_compensation_revisions(
            &mut transaction,
            topology_id,
            revision_id,
            previous_revision_id,
            operation_id,
        )?;
        let changed = transaction.execute(
            "UPDATE orchestrator_topology_heads SET draft_revision_id = $3, applied_revision_id = $3, applying_revision_id = NULL, applying_operation_id = NULL, last_operation_id = $4, updated_at = clock_timestamp() WHERE topology_id = $1 AND draft_revision_id = $2 AND applied_revision_id = $2 AND applying_revision_id IS NULL AND applying_operation_id IS NULL AND last_operation_id = $4",
            &[&topology_id, &revision_id, &previous_revision_id, &operation_id],
        )?;
        if changed != 1 {
            return Err(PostgresError::Conflict(format!(
                "topology {topology_id} completed apply no longer belongs to operation {operation_id}"
            )));
        }
        upsert_compensated_status(
            &mut transaction,
            topology_id,
            previous_revision_id,
            operation_id,
            updated_at,
        )?;
        let completed = load_heads(&mut transaction, topology_id, false)?.ok_or_else(|| {
            PostgresError::Invariant(format!("topology {topology_id} head disappeared"))
        })?;
        transaction.commit()?;
        Ok(completed)
    }

    /// Atomically publishes a successful provider/binding ABORT while the
    /// candidate still owns the apply lease. PostgreSQL locks the head and
    /// refuses stale Operations or a concurrently advanced draft.
    pub fn complete_compensated_topology_abort(
        &self,
        topology_id: &str,
        candidate_revision_id: &str,
        previous_revision_id: &str,
        operation_id: &str,
        updated_at: &str,
    ) -> PostgresResult<TopologyHeads> {
        let mut connection = self.pool().connection()?;
        let mut transaction = connection.transaction()?;
        ensure_compensation_revisions(
            &mut transaction,
            topology_id,
            candidate_revision_id,
            previous_revision_id,
            operation_id,
        )?;
        let changed = transaction.execute(
            "UPDATE orchestrator_topology_heads SET draft_revision_id = $3, applied_revision_id = $3, applying_revision_id = NULL, applying_operation_id = NULL, last_operation_id = $4, updated_at = clock_timestamp() WHERE topology_id = $1 AND draft_revision_id = $2 AND applying_revision_id = $2 AND applying_operation_id = $4 AND applied_revision_id = $3",
            &[
                &topology_id,
                &candidate_revision_id,
                &previous_revision_id,
                &operation_id,
            ],
        )?;
        if changed != 1 {
            return Err(PostgresError::Conflict(format!(
                "topology {topology_id} compensated abort no longer owns candidate {candidate_revision_id} for operation {operation_id}"
            )));
        }
        upsert_compensated_status(
            &mut transaction,
            topology_id,
            previous_revision_id,
            operation_id,
            updated_at,
        )?;
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

fn ensure_active_topology_job_lease(
    transaction: &mut Transaction<'_>,
    job_id: &str,
    operation_id: &str,
    lease_token: &str,
    now_ms: i64,
) -> PostgresResult<()> {
    if job_id.trim().is_empty() || lease_token.trim().is_empty() {
        return Err(PostgresError::Invariant(
            "job_id and lease_token must not be empty".to_string(),
        ));
    }
    let row = transaction.query_opt(
        "SELECT status, lease_expires_at_ms, payload::text FROM orchestrator_jobs WHERE job_id = $1 FOR UPDATE",
        &[&job_id],
    )?;
    let Some(row) = row else {
        return Err(stale_topology_job_lease(job_id));
    };
    let stored_status = row.get::<_, String>(0);
    let stored_expiry = row.get::<_, Option<i64>>(1);
    let job: Job = serde_json::from_str(&row.get::<_, String>(2))?;
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

fn stale_topology_job_lease(job_id: &str) -> PostgresError {
    PostgresError::Conflict(format!(
        "topology apply job {job_id} lease is stale or does not match"
    ))
}

fn topology_job_resolution_error(error: JobError) -> PostgresError {
    match error {
        JobError::Persistence(message) => PostgresError::Invariant(message),
        other => PostgresError::Conflict(other.to_string()),
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
    transaction: &mut Transaction<'_>,
    topology_id: &str,
    revision_id: &str,
    operation_id: &str,
    outcome: TopologyApplyOutcome,
    updated_at: &str,
) -> PostgresResult<TopologyHeads> {
    let heads = load_heads(transaction, topology_id, true)?.ok_or_else(|| {
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
    upsert_status(transaction, &status)?;
    load_heads(transaction, topology_id, false)?
        .ok_or_else(|| PostgresError::Invariant(format!("topology {topology_id} head disappeared")))
}

fn finish_topology_apply_group_transaction(
    transaction: &mut Transaction<'_>,
    members: &[TopologyApplyGroupMember],
    operation_id: &str,
    updated_at: &str,
) -> PostgresResult<Vec<TopologyHeads>> {
    // This transaction replaces the same projection that Agent completion may
    // update by deployment. Acquire the shared lock before any row locks so all
    // binding writers use one global ordering and cannot interleave replacement
    // DELETE/INSERT sequences.
    lock_api_binding_mutations(transaction)?;
    let mut ordered_members = members.iter().collect::<Vec<_>>();
    ordered_members.sort_by(|left, right| left.topology_id.cmp(&right.topology_id));
    for member in &ordered_members {
        let heads = load_heads(transaction, &member.topology_id, true)?.ok_or_else(|| {
            PostgresError::Invariant(format!("topology {} does not exist", member.topology_id))
        })?;
        if heads.applying_revision_id.as_deref() != Some(member.revision_id.as_str())
            || heads.applying_operation_id.as_deref() != Some(operation_id)
        {
            return Err(PostgresError::Conflict(format!(
                "topology {} group apply lease does not match operation {operation_id}",
                member.topology_id
            )));
        }
    }
    for member in &ordered_members {
        transaction.execute(
            "DELETE FROM orchestrator_api_bindings WHERE topology_id = $1",
            &[&member.topology_id],
        )?;
    }
    for member in &ordered_members {
        for binding in &member.active_bindings {
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
                    &binding_state_label(binding.derived_state()),
                    &payload,
                ],
            )?;
        }
    }
    for member in &ordered_members {
        let changed = transaction.execute(
            "UPDATE orchestrator_topology_heads SET applied_revision_id = $2, applying_revision_id = NULL, applying_operation_id = NULL, last_operation_id = $3, updated_at = clock_timestamp() WHERE topology_id = $1 AND applying_revision_id = $2 AND applying_operation_id = $3",
            &[&member.topology_id, &member.revision_id, &operation_id],
        )?;
        if changed != 1 {
            return Err(PostgresError::Conflict(format!(
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
    }
    members
        .iter()
        .map(|member| {
            load_heads(transaction, &member.topology_id, false)?.ok_or_else(|| {
                PostgresError::Invariant(format!(
                    "topology {} head disappeared",
                    member.topology_id
                ))
            })
        })
        .collect()
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

fn next_persisted_revision_number(
    client: &mut impl GenericClient,
    topology_id: &str,
) -> PostgresResult<u64> {
    let maximum = client
        .query_one(
            "SELECT MAX(revision_number) FROM orchestrator_topology_revisions WHERE topology_id = $1",
            &[&topology_id],
        )?
        .get::<_, Option<i64>>(0)
        .ok_or_else(|| {
            PostgresError::Invariant(format!("topology {topology_id} has no revision history"))
        })?;
    u64::try_from(maximum)
        .map_err(|_| PostgresError::Invariant("negative topology revision number".to_string()))?
        .checked_add(1)
        .ok_or_else(|| PostgresError::Invariant("topology revision number overflow".to_string()))
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

fn ensure_compensation_revisions(
    client: &mut impl GenericClient,
    topology_id: &str,
    candidate_revision_id: &str,
    previous_revision_id: &str,
    operation_id: &str,
) -> PostgresResult<()> {
    if topology_id.trim().is_empty()
        || candidate_revision_id.trim().is_empty()
        || previous_revision_id.trim().is_empty()
        || operation_id.trim().is_empty()
        || candidate_revision_id == previous_revision_id
    {
        return Err(PostgresError::Invariant(
            "compensated topology abort requires distinct non-empty candidate/previous revisions and a non-empty operation_id"
                .to_string(),
        ));
    }
    if load_revision(client, topology_id, candidate_revision_id)?.is_none() {
        return Err(PostgresError::Invariant(format!(
            "candidate topology revision {candidate_revision_id} is missing"
        )));
    }
    if load_revision(client, topology_id, previous_revision_id)?.is_none() {
        return Err(PostgresError::Invariant(format!(
            "previous topology revision {previous_revision_id} is missing; an initial topology apply cannot be automatically rewound"
        )));
    }
    Ok(())
}

fn upsert_compensated_status(
    client: &mut impl GenericClient,
    topology_id: &str,
    previous_revision_id: &str,
    operation_id: &str,
    updated_at: &str,
) -> PostgresResult<()> {
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
    upsert_status(client, &status)
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

fn validate_apply_group(
    members: &[TopologyApplyGroupMember],
    operation_id: &str,
) -> PostgresResult<()> {
    if members.is_empty() || operation_id.trim().is_empty() {
        return Err(PostgresError::Invariant(
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
            return Err(PostgresError::Invariant(
                "topology apply group members must have unique non-empty identities".to_string(),
            ));
        }
        for binding in &member.active_bindings {
            binding
                .validate()
                .map_err(|error| PostgresError::Invariant(error.to_string()))?;
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
                return Err(PostgresError::Invariant(
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
