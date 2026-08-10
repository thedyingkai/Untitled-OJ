use crate::SqliteOrchestratorStore;
use crate::operations::{EXPIRED_LEASE_COUNTER, increment_counter};
use orchestrator_control_plane::{
    ClaimRequest, CompleteRequest, CompletionStatus, HeartbeatRequest, Job, JobError, JobEvent,
    JobStatus, JobStore, NewJob, NewJobEvent, ResolveExpiredSuccessRequest,
    canonical_payload_sha256, retry_backoff_ms,
};
use rusqlite::{OptionalExtension, Transaction, TransactionBehavior, params};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct JobMetricsSnapshot {
    pub queued: u64,
    pub leased: u64,
    pub retry_wait: u64,
    pub cancel_requested: u64,
    pub succeeded: u64,
    pub failed: u64,
    pub cancelled: u64,
    pub needs_attention: u64,
    pub expired_leases: u64,
    pub oldest_leased_heartbeat_age_seconds: u64,
}

#[derive(Debug, Clone)]
pub struct SqliteJobStore {
    storage: SqliteOrchestratorStore,
}

impl SqliteJobStore {
    pub fn new(storage: SqliteOrchestratorStore) -> Self {
        Self { storage }
    }

    pub fn storage(&self) -> &SqliteOrchestratorStore {
        &self.storage
    }

    pub fn active_job_count(&self, node_id: &str) -> Result<u64, JobError> {
        let connection = self.storage.connection().map_err(job_storage_error)?;
        let count = connection
            .query_row(
                "SELECT COUNT(*) FROM orchestrator_jobs WHERE node_id = ?1 AND status IN ('QUEUED', 'LEASED', 'RETRY_WAIT', 'CANCEL_REQUESTED')",
                [node_id],
                |row| row.get::<_, i64>(0),
            )
            .map_err(job_sqlite_error)?;
        u64::try_from(count)
            .map_err(|_| JobError::Persistence("active job count is negative".to_string()))
    }

    pub fn metrics_snapshot(&self, now_ms: i64) -> Result<JobMetricsSnapshot, JobError> {
        let mut connection = self.storage.connection().map_err(job_storage_error)?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Deferred)
            .map_err(job_sqlite_error)?;
        let mut snapshot = JobMetricsSnapshot::default();
        let mut seen_statuses = BTreeSet::new();
        let mut statement = transaction
            .prepare("SELECT status, job_count FROM orchestrator_job_status_counts")
            .map_err(job_sqlite_error)?;
        let rows = statement
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
            })
            .map_err(job_sqlite_error)?;
        for row in rows {
            let (status, count) = row.map_err(job_sqlite_error)?;
            assign_status_count(&mut snapshot, &status, count)?;
            seen_statuses.insert(status);
        }
        if seen_statuses.len() != 8 {
            return Err(JobError::Persistence(format!(
                "job status counter table has {} of 8 required rows",
                seen_statuses.len()
            )));
        }
        let expired: i64 = transaction
            .query_row(
                "SELECT COUNT(*) FROM orchestrator_jobs WHERE status IN ('LEASED', 'CANCEL_REQUESTED') AND lease_expires_at_ms IS NOT NULL AND lease_expires_at_ms <= ?1",
                [now_ms],
                |row| row.get(0),
            )
            .map_err(job_sqlite_error)?;
        snapshot.expired_leases = nonnegative_u64(expired, "expired lease count")?;
        let oldest_updated_at: Option<i64> = transaction
            .query_row(
                "SELECT MIN(updated_at_ms) FROM orchestrator_jobs WHERE status = 'LEASED' AND lease_expires_at_ms IS NOT NULL",
                [],
                |row| row.get(0),
            )
            .map_err(job_sqlite_error)?;
        snapshot.oldest_leased_heartbeat_age_seconds = oldest_updated_at
            .map(|updated| nonnegative_u64(now_ms.saturating_sub(updated).max(0), "leased Job age"))
            .transpose()?
            .unwrap_or_default()
            / 1_000;
        drop(statement);
        transaction.commit().map_err(job_sqlite_error)?;
        Ok(snapshot)
    }
}

impl JobStore for SqliteJobStore {
    fn enqueue(&mut self, new: NewJob, now_ms: i64) -> Result<Job, JobError> {
        validate_new_job(&new)?;
        let payload_sha256 = canonical_payload_sha256(&new.payload);
        let mut connection = self.storage.connection().map_err(job_storage_error)?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(job_sqlite_error)?;
        let idempotent = transaction
            .query_row(
                "SELECT payload FROM orchestrator_jobs WHERE node_id = ?1 AND idempotency_key = ?2",
                params![&new.node_id, &new.idempotency_key],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(job_sqlite_error)?;
        if let Some(payload) = idempotent {
            let existing: Job = serde_json::from_str(&payload).map_err(job_json_error)?;
            if existing.payload_sha256 != payload_sha256
                || existing.kind != new.kind
                || existing.operation_id != new.operation_id
            {
                return Err(JobError::IdempotencyConflict);
            }
            return Ok(existing);
        }
        let job_id_exists: bool = transaction
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM orchestrator_jobs WHERE job_id = ?1)",
                [&new.job_id],
                |row| row.get(0),
            )
            .map_err(job_sqlite_error)?;
        if job_id_exists {
            return Err(JobError::JobIdConflict(new.job_id));
        }
        let job = Job {
            job_id: new.job_id,
            operation_id: new.operation_id,
            node_id: new.node_id,
            kind: new.kind,
            payload: new.payload,
            payload_sha256,
            idempotency_key: new.idempotency_key,
            status: JobStatus::Queued,
            attempt: 0,
            max_attempts: new.max_attempts,
            available_at_ms: now_ms,
            lease_owner: None,
            lease_token: None,
            lease_expires_at_ms: None,
            result: None,
            error_message: None,
            completion_fingerprint: None,
            created_at_ms: now_ms,
            started_at_ms: None,
            completed_at_ms: None,
            updated_at_ms: now_ms,
        };
        insert_job(&transaction, &job)?;
        transaction.commit().map_err(job_sqlite_error)?;
        Ok(job)
    }

    fn claim(&mut self, request: ClaimRequest) -> Result<Option<Job>, JobError> {
        if request.lease_ms <= 0 || request.lease_token.trim().is_empty() {
            return Err(JobError::InvalidJob(
                "lease_ms must be positive and lease_token is required".to_string(),
            ));
        }
        let mut connection = self.storage.connection().map_err(job_storage_error)?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(job_sqlite_error)?;
        let payload = transaction.query_row(
            "SELECT payload FROM orchestrator_jobs WHERE node_id = ?1 AND status IN ('QUEUED', 'RETRY_WAIT') AND available_at_ms <= ?2 ORDER BY available_at_ms, created_at_ms, job_id LIMIT 1",
            params![&request.node_id, request.now_ms],
            |row| row.get::<_, String>(0),
        ).optional().map_err(job_sqlite_error)?;
        let Some(payload) = payload else {
            transaction.commit().map_err(job_sqlite_error)?;
            return Ok(None);
        };
        let mut job: Job = serde_json::from_str(&payload).map_err(job_json_error)?;
        job.status = JobStatus::Leased;
        job.attempt += 1;
        job.lease_owner = Some(request.instance_id);
        job.lease_token = Some(request.lease_token);
        job.lease_expires_at_ms = Some(request.now_ms + request.lease_ms);
        job.started_at_ms.get_or_insert(request.now_ms);
        job.updated_at_ms = request.now_ms;
        update_job(&transaction, &job)?;
        transaction.commit().map_err(job_sqlite_error)?;
        Ok(Some(job))
    }

    fn heartbeat(&mut self, request: HeartbeatRequest) -> Result<Job, JobError> {
        if request.lease_ms <= 0 {
            return Err(JobError::InvalidJob(
                "lease_ms must be positive".to_string(),
            ));
        }
        let mut connection = self.storage.connection().map_err(job_storage_error)?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(job_sqlite_error)?;
        let mut job = required_job(&transaction, &request.job_id)?;
        if lease_is_expired(&job, request.now_ms) {
            let newly_observed = recover_expired_job(&transaction, &mut job, request.now_ms)?;
            increment_counter(
                &transaction,
                EXPIRED_LEASE_COUNTER,
                u64::from(newly_observed),
            )
            .map_err(|error| JobError::Persistence(error.to_string()))?;
            transaction.commit().map_err(job_sqlite_error)?;
            return Err(JobError::StaleLease);
        }
        ensure_active_lease(&job, &request.lease_token, request.now_ms)?;
        append_events(
            &transaction,
            &request.job_id,
            request.now_ms,
            request.events,
        )?;
        job.lease_expires_at_ms = Some(request.now_ms + request.lease_ms);
        job.updated_at_ms = request.now_ms;
        update_job(&transaction, &job)?;
        transaction.commit().map_err(job_sqlite_error)?;
        Ok(job)
    }

    fn complete(&mut self, request: CompleteRequest) -> Result<Job, JobError> {
        let fingerprint = completion_fingerprint(&request);
        let mut connection = self.storage.connection().map_err(job_storage_error)?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(job_sqlite_error)?;
        let mut job = required_job(&transaction, &request.job_id)?;
        if job.status.is_terminal() {
            if job.lease_token.as_deref() == Some(request.lease_token.as_str())
                && job.completion_fingerprint.as_deref() == Some(fingerprint.as_str())
            {
                return Ok(job);
            }
            return Err(JobError::StaleLease);
        }
        if lease_is_expired(&job, request.now_ms) {
            let newly_observed = recover_expired_job(&transaction, &mut job, request.now_ms)?;
            increment_counter(
                &transaction,
                EXPIRED_LEASE_COUNTER,
                u64::from(newly_observed),
            )
            .map_err(|error| JobError::Persistence(error.to_string()))?;
            transaction.commit().map_err(job_sqlite_error)?;
            return Err(JobError::StaleLease);
        }
        ensure_active_lease(&job, &request.lease_token, request.now_ms)?;
        append_events(
            &transaction,
            &request.job_id,
            request.now_ms,
            request.events,
        )?;
        match request.status {
            CompletionStatus::Succeeded => job.status = JobStatus::Succeeded,
            CompletionStatus::Failed => job.status = JobStatus::Failed,
            CompletionStatus::Cancelled => job.status = JobStatus::Cancelled,
            CompletionStatus::NeedsAttention => job.status = JobStatus::NeedsAttention,
            CompletionStatus::RetryableFailure if job.attempt < job.max_attempts => {
                job.status = JobStatus::RetryWait;
                job.available_at_ms = request.now_ms + retry_backoff_ms(job.attempt);
            }
            CompletionStatus::RetryableFailure => job.status = JobStatus::Failed,
        }
        job.result = Some(request.result);
        job.error_message = (!request.error_message.is_empty()).then_some(request.error_message);
        job.updated_at_ms = request.now_ms;
        if job.status.is_terminal() {
            job.completed_at_ms = Some(request.now_ms);
            job.completion_fingerprint = Some(fingerprint);
        } else {
            clear_lease(&mut job);
        }
        update_job(&transaction, &job)?;
        transaction.commit().map_err(job_sqlite_error)?;
        Ok(job)
    }

    fn resolve_expired_success(
        &mut self,
        request: ResolveExpiredSuccessRequest,
    ) -> Result<Job, JobError> {
        let mut connection = self.storage.connection().map_err(job_storage_error)?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(job_sqlite_error)?;
        let job = resolve_expired_success_in_transaction(&transaction, &request)?;
        transaction.commit().map_err(job_sqlite_error)?;
        Ok(job)
    }

    fn request_cancel(&mut self, job_id: &str, now_ms: i64) -> Result<Job, JobError> {
        let mut connection = self.storage.connection().map_err(job_storage_error)?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(job_sqlite_error)?;
        let mut job = required_job(&transaction, job_id)?;
        match job.status {
            JobStatus::Queued | JobStatus::RetryWait => {
                job.status = JobStatus::Cancelled;
                job.completed_at_ms = Some(now_ms);
            }
            JobStatus::Leased => job.status = JobStatus::CancelRequested,
            JobStatus::CancelRequested => {}
            _ if job.status.is_terminal() => return Ok(job),
            _ => {
                return Err(JobError::InvalidTransition {
                    from: job.status,
                    to: "CANCELLED",
                });
            }
        }
        job.updated_at_ms = now_ms;
        update_job(&transaction, &job)?;
        transaction.commit().map_err(job_sqlite_error)?;
        Ok(job)
    }

    fn expired_leases(&self, now_ms: i64) -> Result<Vec<Job>, JobError> {
        let connection = self.storage.connection().map_err(job_storage_error)?;
        let mut statement = connection
            .prepare(
                "SELECT payload FROM orchestrator_jobs WHERE status IN ('LEASED', 'CANCEL_REQUESTED') AND lease_expires_at_ms <= ?1 ORDER BY status, lease_expires_at_ms, job_id",
            )
            .map_err(job_sqlite_error)?;
        statement
            .query_map([now_ms], |row| row.get::<_, String>(0))
            .map_err(job_sqlite_error)?
            .map(|payload| {
                payload
                    .map_err(job_sqlite_error)
                    .and_then(|payload| serde_json::from_str(&payload).map_err(job_json_error))
            })
            .collect()
    }

    fn recover_expired(&mut self, now_ms: i64) -> Result<Vec<Job>, JobError> {
        let mut connection = self.storage.connection().map_err(job_storage_error)?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(job_sqlite_error)?;
        let payloads = {
            let mut statement = transaction.prepare(
                "SELECT payload FROM orchestrator_jobs WHERE status IN ('LEASED', 'CANCEL_REQUESTED') AND lease_expires_at_ms <= ?1 ORDER BY status, lease_expires_at_ms, job_id",
            ).map_err(job_sqlite_error)?;
            statement
                .query_map([now_ms], |row| row.get::<_, String>(0))
                .map_err(job_sqlite_error)?
                .collect::<std::result::Result<Vec<_>, _>>()
                .map_err(job_sqlite_error)?
        };
        let mut recovered = Vec::new();
        let mut newly_observed = 0_u64;
        for payload in payloads {
            let mut job: Job = serde_json::from_str(&payload).map_err(job_json_error)?;
            newly_observed = newly_observed.saturating_add(u64::from(recover_expired_job(
                &transaction,
                &mut job,
                now_ms,
            )?));
            recovered.push(job);
        }
        increment_counter(&transaction, EXPIRED_LEASE_COUNTER, newly_observed)
            .map_err(|error| JobError::Persistence(error.to_string()))?;
        transaction.commit().map_err(job_sqlite_error)?;
        Ok(recovered)
    }

    fn get(&self, job_id: &str) -> Result<Option<Job>, JobError> {
        let connection = self.storage.connection().map_err(job_storage_error)?;
        let payload = connection
            .query_row(
                "SELECT payload FROM orchestrator_jobs WHERE job_id = ?1",
                [job_id],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(job_sqlite_error)?;
        payload
            .map(|value| serde_json::from_str(&value).map_err(job_json_error))
            .transpose()
    }

    fn list(&self) -> Result<Vec<Job>, JobError> {
        let connection = self.storage.connection().map_err(job_storage_error)?;
        let mut statement = connection
            .prepare("SELECT payload FROM orchestrator_jobs ORDER BY created_at_ms, job_id")
            .map_err(job_sqlite_error)?;
        statement
            .query_map([], |row| row.get::<_, String>(0))
            .map_err(job_sqlite_error)?
            .map(|payload| {
                payload
                    .map_err(job_sqlite_error)
                    .and_then(|payload| serde_json::from_str(&payload).map_err(job_json_error))
            })
            .collect()
    }

    fn events(&self, job_id: &str, after_sequence: u64) -> Result<Vec<JobEvent>, JobError> {
        if self.get(job_id)?.is_none() {
            return Err(JobError::NotFound(job_id.to_string()));
        }
        let connection = self.storage.connection().map_err(job_storage_error)?;
        let mut statement = connection.prepare(
            "SELECT payload FROM orchestrator_job_events WHERE job_id = ?1 AND sequence > ?2 ORDER BY sequence",
        ).map_err(job_sqlite_error)?;
        let payloads = statement
            .query_map(params![job_id, after_sequence], |row| {
                row.get::<_, String>(0)
            })
            .map_err(job_sqlite_error)?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(job_sqlite_error)?;
        payloads
            .into_iter()
            .map(|value| serde_json::from_str(&value).map_err(job_json_error))
            .collect()
    }
}

fn validate_new_job(job: &NewJob) -> Result<(), JobError> {
    if job.job_id.trim().is_empty()
        || job.operation_id.trim().is_empty()
        || job.node_id.trim().is_empty()
        || job.idempotency_key.trim().is_empty()
    {
        return Err(JobError::InvalidJob(
            "job_id, operation_id, node_id, and idempotency_key are required".to_string(),
        ));
    }
    if job.max_attempts == 0 {
        return Err(JobError::InvalidJob(
            "max_attempts must be greater than zero".to_string(),
        ));
    }
    Ok(())
}

pub(crate) fn resolve_expired_success_in_transaction(
    transaction: &Transaction<'_>,
    request: &ResolveExpiredSuccessRequest,
) -> Result<Job, JobError> {
    let mut job = required_job(transaction, &request.job_id)?;
    validate_expired_success_result(&request.result)?;
    let fingerprint = successful_completion_fingerprint(&request.result);
    if job.status == JobStatus::Succeeded {
        if job.result.as_ref() == Some(&request.result)
            && job.completion_fingerprint.as_deref() == Some(fingerprint.as_str())
        {
            return Ok(job);
        }
        return Err(JobError::InvalidTransition {
            from: job.status,
            to: "SUCCEEDED_FROM_EXPIRED_EVIDENCE",
        });
    }
    if !matches!(job.status, JobStatus::Leased | JobStatus::CancelRequested) {
        return Err(JobError::InvalidTransition {
            from: job.status,
            to: "SUCCEEDED_FROM_EXPIRED_EVIDENCE",
        });
    }
    if job
        .lease_expires_at_ms
        .is_none_or(|deadline| deadline > request.now_ms)
    {
        return Err(JobError::StaleLease);
    }

    let lease_identity = lease_episode_identity(&job);
    let was_already_observed = match lease_identity.as_deref() {
        Some(identity) => transaction
            .execute(
                "DELETE FROM orchestrator_active_expired_lease_anomalies WHERE job_id = ?1 AND lease_identity = ?2",
                params![&job.job_id, identity],
            )
            .map_err(job_sqlite_error)?
            == 1,
        None => false,
    };
    transaction
        .execute(
            "DELETE FROM orchestrator_active_expired_lease_anomalies WHERE job_id = ?1",
            [&job.job_id],
        )
        .map_err(job_sqlite_error)?;

    job.status = JobStatus::Succeeded;
    job.result = Some(request.result.clone());
    job.error_message = None;
    job.completion_fingerprint = Some(fingerprint);
    job.completed_at_ms = Some(request.now_ms);
    job.updated_at_ms = request.now_ms;
    update_job(transaction, &job)?;
    increment_counter(
        transaction,
        EXPIRED_LEASE_COUNTER,
        u64::from(!was_already_observed),
    )
    .map_err(|error| JobError::Persistence(error.to_string()))?;
    Ok(job)
}

fn insert_job(transaction: &Transaction<'_>, job: &Job) -> Result<(), JobError> {
    transaction.execute(
        "INSERT INTO orchestrator_jobs(job_id, operation_id, node_id, idempotency_key, payload_sha256, status, available_at_ms, created_at_ms, lease_expires_at_ms, updated_at_ms, payload) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
        params![&job.job_id, &job.operation_id, &job.node_id, &job.idempotency_key,
            &job.payload_sha256, status_text(&job.status), job.available_at_ms,
            job.created_at_ms, job.lease_expires_at_ms, job.updated_at_ms,
            serde_json::to_string(job).map_err(job_json_error)?],
    ).map_err(job_sqlite_error)?;
    adjust_status_count(transaction, status_text(&job.status), 1)?;
    Ok(())
}

fn update_job(transaction: &Transaction<'_>, job: &Job) -> Result<(), JobError> {
    let (previous_status, previous_payload) = transaction
        .query_row(
            "SELECT status, payload FROM orchestrator_jobs WHERE job_id = ?1",
            [&job.job_id],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        )
        .map_err(job_sqlite_error)?;
    let previous: Job = serde_json::from_str(&previous_payload).map_err(job_json_error)?;
    let updated = transaction.execute(
        "UPDATE orchestrator_jobs SET operation_id = ?2, node_id = ?3, idempotency_key = ?4, payload_sha256 = ?5, status = ?6, available_at_ms = ?7, lease_expires_at_ms = ?8, updated_at_ms = ?9, payload = ?10 WHERE job_id = ?1",
        params![&job.job_id, &job.operation_id, &job.node_id, &job.idempotency_key,
            &job.payload_sha256, status_text(&job.status), job.available_at_ms,
            job.lease_expires_at_ms, job.updated_at_ms,
            serde_json::to_string(job).map_err(job_json_error)?],
    ).map_err(job_sqlite_error)?;
    if updated != 1 {
        return Err(JobError::NotFound(job.job_id.clone()));
    }
    let next_status = status_text(&job.status);
    if previous_status != next_status {
        adjust_status_count(transaction, &previous_status, -1)?;
        adjust_status_count(transaction, next_status, 1)?;
    }
    if lease_episode_identity(&previous) != lease_episode_identity(job) {
        transaction
            .execute(
                "DELETE FROM orchestrator_active_expired_lease_anomalies WHERE job_id = ?1",
                [&job.job_id],
            )
            .map_err(job_sqlite_error)?;
    }
    Ok(())
}

pub(crate) fn lease_episode_identity(job: &Job) -> Option<String> {
    if !matches!(job.status, JobStatus::Leased | JobStatus::CancelRequested) {
        return None;
    }
    let deadline = job.lease_expires_at_ms?;
    let token_digest = Sha256::digest(job.lease_token.as_deref().unwrap_or("").as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    Some(format!("{}:{deadline}:{token_digest}", job.attempt))
}

fn adjust_status_count(
    transaction: &Transaction<'_>,
    status: &str,
    delta: i64,
) -> Result<(), JobError> {
    let changed = transaction
        .execute(
            "UPDATE orchestrator_job_status_counts SET job_count = job_count + ?2 WHERE status = ?1 AND job_count + ?2 >= 0",
            params![status, delta],
        )
        .map_err(job_sqlite_error)?;
    if changed != 1 {
        return Err(JobError::Persistence(format!(
            "job status count for {status} is missing or exhausted"
        )));
    }
    Ok(())
}

fn nonnegative_u64(value: i64, label: &str) -> Result<u64, JobError> {
    u64::try_from(value).map_err(|_| JobError::Persistence(format!("{label} is negative")))
}

fn assign_status_count(
    snapshot: &mut JobMetricsSnapshot,
    status: &str,
    count: i64,
) -> Result<(), JobError> {
    let count = nonnegative_u64(count, "job status count")?;
    match status {
        "QUEUED" => snapshot.queued = count,
        "LEASED" => snapshot.leased = count,
        "RETRY_WAIT" => snapshot.retry_wait = count,
        "CANCEL_REQUESTED" => snapshot.cancel_requested = count,
        "SUCCEEDED" => snapshot.succeeded = count,
        "FAILED" => snapshot.failed = count,
        "CANCELLED" => snapshot.cancelled = count,
        "NEEDS_ATTENTION" => snapshot.needs_attention = count,
        _ => {
            return Err(JobError::Persistence(format!(
                "unknown Job status count {status}"
            )));
        }
    }
    Ok(())
}

fn required_job(transaction: &Transaction<'_>, job_id: &str) -> Result<Job, JobError> {
    let payload = transaction
        .query_row(
            "SELECT payload FROM orchestrator_jobs WHERE job_id = ?1",
            [job_id],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .map_err(job_sqlite_error)?
        .ok_or_else(|| JobError::NotFound(job_id.to_string()))?;
    serde_json::from_str(&payload).map_err(job_json_error)
}

fn lease_is_expired(job: &Job, now_ms: i64) -> bool {
    matches!(job.status, JobStatus::Leased | JobStatus::CancelRequested)
        && job
            .lease_expires_at_ms
            .is_some_and(|deadline| deadline <= now_ms)
}

fn ensure_active_lease(job: &Job, token: &str, now_ms: i64) -> Result<(), JobError> {
    if matches!(job.status, JobStatus::Leased | JobStatus::CancelRequested)
        && job.lease_token.as_deref() == Some(token)
        && job
            .lease_expires_at_ms
            .is_some_and(|deadline| now_ms < deadline)
    {
        Ok(())
    } else {
        Err(JobError::StaleLease)
    }
}

fn recover_expired_job(
    transaction: &Transaction<'_>,
    job: &mut Job,
    now_ms: i64,
) -> Result<bool, JobError> {
    let lease_identity = lease_episode_identity(job);
    let was_already_observed = match lease_identity.as_deref() {
        Some(identity) => transaction
            .execute(
                "DELETE FROM orchestrator_active_expired_lease_anomalies WHERE job_id = ?1 AND lease_identity = ?2",
                params![&job.job_id, identity],
            )
            .map_err(job_sqlite_error)?
            == 1,
        None => false,
    };
    // A stale legacy marker for a prior lease of the same Job must not
    // survive recovery of the current lease.
    transaction
        .execute(
            "DELETE FROM orchestrator_active_expired_lease_anomalies WHERE job_id = ?1",
            [&job.job_id],
        )
        .map_err(job_sqlite_error)?;
    if job.status == JobStatus::CancelRequested {
        job.status = JobStatus::NeedsAttention;
        job.error_message =
            Some("worker lease expired while cancellation outcome was unknown".to_string());
        job.completed_at_ms = Some(now_ms);
    } else if job.kind.is_retry_safe_after_lease_expiry() && job.attempt < job.max_attempts {
        job.status = JobStatus::RetryWait;
        job.available_at_ms = now_ms + retry_backoff_ms(job.attempt);
        job.error_message =
            Some("worker lease expired; awaiting ledger reconciliation".to_string());
    } else {
        job.status = JobStatus::NeedsAttention;
        job.error_message = Some(if job.kind.is_retry_safe_after_lease_expiry() {
            "worker lease expired and retry budget was exhausted".to_string()
        } else {
            "worker lease expired with an unknown side-effect outcome; automatic retry is forbidden"
                .to_string()
        });
        job.completed_at_ms = Some(now_ms);
    }
    clear_lease(job);
    job.updated_at_ms = now_ms;
    update_job(transaction, job)?;
    Ok(!was_already_observed)
}

fn append_events(
    transaction: &Transaction<'_>,
    job_id: &str,
    now_ms: i64,
    events: Vec<NewJobEvent>,
) -> Result<(), JobError> {
    for event in events {
        let stored = JobEvent {
            job_id: job_id.to_string(),
            sequence: event.sequence,
            event_type: event.event_type,
            level: event.level,
            message: event.message,
            data: event.data,
            created_at_ms: now_ms,
        };
        let existing = transaction
            .query_row(
                "SELECT payload FROM orchestrator_job_events WHERE job_id = ?1 AND sequence = ?2",
                params![job_id, stored.sequence],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(job_sqlite_error)?;
        match existing {
            Some(payload) => {
                let existing: JobEvent = serde_json::from_str(&payload).map_err(job_json_error)?;
                if existing != stored {
                    return Err(JobError::EventConflict {
                        sequence: stored.sequence,
                    });
                }
            }
            None => {
                transaction.execute(
                    "INSERT INTO orchestrator_job_events(job_id, sequence, payload, created_at_ms) VALUES (?1, ?2, ?3, ?4)",
                    params![job_id, stored.sequence, serde_json::to_string(&stored).map_err(job_json_error)?, now_ms],
                ).map_err(job_sqlite_error)?;
            }
        }
    }
    Ok(())
}

fn clear_lease(job: &mut Job) {
    job.lease_owner = None;
    job.lease_token = None;
    job.lease_expires_at_ms = None;
}

fn completion_fingerprint(request: &CompleteRequest) -> String {
    canonical_payload_sha256(&serde_json::json!({
        "status": request.status,
        "result": request.result,
        "error": request.error_message,
    }))
}

fn successful_completion_fingerprint(result: &serde_json::Value) -> String {
    canonical_payload_sha256(&serde_json::json!({
        "status": CompletionStatus::Succeeded,
        "result": result,
        "error": "",
    }))
}

fn validate_expired_success_result(result: &serde_json::Value) -> Result<(), JobError> {
    if result.is_null() {
        return Err(JobError::InvalidJob(
            "expired success resolution requires durable result evidence".to_string(),
        ));
    }
    Ok(())
}

fn status_text(status: &JobStatus) -> &'static str {
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

fn job_storage_error(error: crate::StorageError) -> JobError {
    JobError::Persistence(error.to_string())
}

fn job_sqlite_error(error: rusqlite::Error) -> JobError {
    JobError::Persistence(error.to_string())
}

fn job_json_error(error: serde_json::Error) -> JobError {
    JobError::Persistence(error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use orchestrator_control_plane::JobKind;
    use serde_json::json;
    use tempfile::tempdir;

    fn new_job(index: usize) -> NewJob {
        NewJob {
            job_id: format!("job-{index}"),
            operation_id: format!("operation-{index}"),
            node_id: "node-1".to_string(),
            kind: JobKind::Health,
            payload: json!({}),
            idempotency_key: format!("idempotency-{index}"),
            max_attempts: 3,
        }
    }

    #[test]
    fn metrics_projection_is_exact_and_fails_closed_when_a_status_row_is_missing() {
        let directory = tempdir().unwrap();
        let storage = SqliteOrchestratorStore::open(directory.path().join("db.sqlite")).unwrap();
        let mut jobs = SqliteJobStore::new(storage.clone());
        for index in 0..128 {
            jobs.enqueue(new_job(index), 1).unwrap();
            jobs.request_cancel(&format!("job-{index}"), 2).unwrap();
        }
        let snapshot = jobs.metrics_snapshot(3).unwrap();
        assert_eq!(snapshot.cancelled, 128);
        assert_eq!(snapshot.queued, 0);

        storage
            .connection()
            .unwrap()
            .execute(
                "DELETE FROM orchestrator_job_status_counts WHERE status = 'FAILED'",
                [],
            )
            .unwrap();
        let error = jobs.metrics_snapshot(3).unwrap_err();
        assert!(error.to_string().contains("7 of 8 required rows"));
    }

    #[test]
    fn lease_recovery_and_metrics_queries_use_the_v9_partial_index() {
        let directory = tempdir().unwrap();
        let storage = SqliteOrchestratorStore::open(directory.path().join("db.sqlite")).unwrap();
        let connection = storage.connection().unwrap();
        for sql in [
            "EXPLAIN QUERY PLAN SELECT payload FROM orchestrator_jobs WHERE status IN ('LEASED', 'CANCEL_REQUESTED') AND lease_expires_at_ms <= 1 ORDER BY status, lease_expires_at_ms, job_id",
            "EXPLAIN QUERY PLAN SELECT COUNT(*) FROM orchestrator_jobs WHERE status IN ('LEASED', 'CANCEL_REQUESTED') AND lease_expires_at_ms IS NOT NULL AND lease_expires_at_ms <= 1",
        ] {
            let mut statement = connection.prepare(sql).unwrap();
            let details = statement
                .query_map([], |row| row.get::<_, String>(3))
                .unwrap()
                .collect::<std::result::Result<Vec<_>, _>>()
                .unwrap();
            assert!(
                details
                    .iter()
                    .any(|detail| detail.contains("idx_orchestrator_jobs_lease_recovery")),
                "query did not use v9 lease index: {details:?}"
            );
        }
    }
}
