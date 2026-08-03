//! Durable control-plane job contracts and state transitions.
//!
//! Persistence adapters implement [`JobStore`].  The transition rules live in
//! this crate so SQLite, PostgreSQL, and the in-memory contract tests cannot
//! silently diverge.

use orchestrator_core::OperationStatus;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use thiserror::Error;

mod operation;

pub use operation::{
    DurableOperation, DurableOperationMode, DurableOperationStatus, JobBinding,
    MemoryOperationStore, OPERATION_SCHEMA_VERSION, OperationCoordinator, OperationError,
    OperationRepository, OperationStoreError, PlanOperation, PlannedJob, PlannedJobCondition,
    validate_durable_operation, validate_durable_operation_update,
};

pub const DEFAULT_LEASE_MS: i64 = 30_000;
pub const DEFAULT_HEARTBEAT_MS: i64 = 10_000;
pub const DEFAULT_LONG_POLL_MS: i64 = 25_000;
pub const DEFAULT_MAX_ATTEMPTS: u32 = 3;
pub const RETRY_BACKOFF_MS: [i64; 3] = [1_000, 5_000, 30_000];

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum JobStatus {
    Queued,
    Leased,
    RetryWait,
    CancelRequested,
    Succeeded,
    Failed,
    Cancelled,
    NeedsAttention,
}

impl JobStatus {
    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            Self::Succeeded | Self::Failed | Self::Cancelled | Self::NeedsAttention
        )
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum JobKind {
    Install,
    /// Ordered Store pipeline executed by one Node Agent: typed providers,
    /// durable OCI migrations, runtime install, then Gateway publication.
    ReleasePipeline,
    Upgrade,
    Start,
    Stop,
    Restart,
    Uninstall,
    Rollback,
    Health,
    Inventory,
    TopologyApply,
    /// Control-plane health validation and projection for a non-managed endpoint.
    ExternalHealth,
    /// Internal control-plane job. It is never claimable by a remote Agent.
    NodeDrain,
    /// Internal control-plane job. It is never claimable by a remote Agent.
    NodeRemove,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Job {
    pub job_id: String,
    pub operation_id: String,
    pub node_id: String,
    pub kind: JobKind,
    pub payload: Value,
    pub payload_sha256: String,
    pub idempotency_key: String,
    pub status: JobStatus,
    pub attempt: u32,
    pub max_attempts: u32,
    pub available_at_ms: i64,
    pub lease_owner: Option<String>,
    pub lease_token: Option<String>,
    pub lease_expires_at_ms: Option<i64>,
    pub result: Option<Value>,
    pub error_message: Option<String>,
    pub completion_fingerprint: Option<String>,
    pub created_at_ms: i64,
    pub started_at_ms: Option<i64>,
    pub completed_at_ms: Option<i64>,
    pub updated_at_ms: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct NewJob {
    pub job_id: String,
    pub operation_id: String,
    pub node_id: String,
    pub kind: JobKind,
    pub payload: Value,
    pub idempotency_key: String,
    #[serde(default = "default_max_attempts")]
    pub max_attempts: u32,
}

fn default_max_attempts() -> u32 {
    DEFAULT_MAX_ATTEMPTS
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct JobEvent {
    pub job_id: String,
    pub sequence: u64,
    pub event_type: String,
    pub level: String,
    pub message: String,
    pub data: Value,
    pub created_at_ms: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ClaimRequest {
    pub node_id: String,
    pub instance_id: String,
    pub lease_token: String,
    pub now_ms: i64,
    #[serde(default = "default_lease_ms")]
    pub lease_ms: i64,
}

fn default_lease_ms() -> i64 {
    DEFAULT_LEASE_MS
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct HeartbeatRequest {
    pub job_id: String,
    pub lease_token: String,
    pub now_ms: i64,
    #[serde(default = "default_lease_ms")]
    pub lease_ms: i64,
    #[serde(default)]
    pub events: Vec<NewJobEvent>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct NewJobEvent {
    pub sequence: u64,
    pub event_type: String,
    pub level: String,
    pub message: String,
    pub data: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum CompletionStatus {
    Succeeded,
    RetryableFailure,
    Failed,
    Cancelled,
    NeedsAttention,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CompleteRequest {
    pub job_id: String,
    pub lease_token: String,
    pub status: CompletionStatus,
    #[serde(default)]
    pub result: Value,
    #[serde(default)]
    pub error_message: String,
    pub now_ms: i64,
    #[serde(default)]
    pub events: Vec<NewJobEvent>,
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum JobError {
    #[error("job persistence error: {0}")]
    Persistence(String),
    #[error("job not found: {0}")]
    NotFound(String),
    #[error("idempotency key already belongs to a different payload")]
    IdempotencyConflict,
    #[error("lease is stale or does not belong to this worker")]
    StaleLease,
    #[error("invalid job transition from {from:?} to {to}")]
    InvalidTransition { from: JobStatus, to: &'static str },
    #[error("event sequence {sequence} conflicts with an existing event")]
    EventConflict { sequence: u64 },
    #[error("job id already exists: {0}")]
    JobIdConflict(String),
    #[error("invalid job: {0}")]
    InvalidJob(String),
}

pub trait JobStore {
    fn enqueue(&mut self, job: NewJob, now_ms: i64) -> Result<Job, JobError>;
    fn claim(&mut self, request: ClaimRequest) -> Result<Option<Job>, JobError>;
    fn heartbeat(&mut self, request: HeartbeatRequest) -> Result<Job, JobError>;
    fn complete(&mut self, request: CompleteRequest) -> Result<Job, JobError>;
    fn request_cancel(&mut self, job_id: &str, now_ms: i64) -> Result<Job, JobError>;
    fn recover_expired(&mut self, now_ms: i64) -> Result<Vec<Job>, JobError>;
    fn get(&self, job_id: &str) -> Result<Option<Job>, JobError>;
    fn list(&self) -> Result<Vec<Job>, JobError>;
    fn events(&self, job_id: &str, after_sequence: u64) -> Result<Vec<JobEvent>, JobError>;
}

#[derive(Debug, Default)]
pub struct MemoryJobStore {
    jobs: BTreeMap<String, Job>,
    idempotency: BTreeMap<(String, String), String>,
    events: BTreeMap<String, BTreeMap<u64, JobEvent>>,
}

impl MemoryJobStore {
    fn append_events(
        &mut self,
        job_id: &str,
        now_ms: i64,
        events: Vec<NewJobEvent>,
    ) -> Result<(), JobError> {
        let target = self.events.entry(job_id.to_string()).or_default();
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
            match target.get(&stored.sequence) {
                Some(existing) if existing == &stored => {}
                Some(_) => {
                    return Err(JobError::EventConflict {
                        sequence: stored.sequence,
                    });
                }
                None => {
                    target.insert(stored.sequence, stored);
                }
            }
        }
        Ok(())
    }

    fn job_for_active_lease(
        &self,
        job_id: &str,
        token: &str,
        now_ms: i64,
    ) -> Result<&Job, JobError> {
        let job = self
            .jobs
            .get(job_id)
            .ok_or_else(|| JobError::NotFound(job_id.to_string()))?;
        let active = matches!(job.status, JobStatus::Leased | JobStatus::CancelRequested)
            && job.lease_token.as_deref() == Some(token)
            && job
                .lease_expires_at_ms
                .is_some_and(|deadline| now_ms < deadline);
        if !active {
            return Err(JobError::StaleLease);
        }
        Ok(job)
    }

    fn recover_job_if_expired(&mut self, job_id: &str, now_ms: i64) -> Result<bool, JobError> {
        let job = self
            .jobs
            .get_mut(job_id)
            .ok_or_else(|| JobError::NotFound(job_id.to_string()))?;
        let expired = matches!(job.status, JobStatus::Leased | JobStatus::CancelRequested)
            && job
                .lease_expires_at_ms
                .is_some_and(|deadline| deadline <= now_ms);
        if expired {
            recover_expired_job(job, now_ms);
        }
        Ok(expired)
    }
}

impl JobStore for MemoryJobStore {
    fn enqueue(&mut self, new: NewJob, now_ms: i64) -> Result<Job, JobError> {
        if new.job_id.trim().is_empty()
            || new.operation_id.trim().is_empty()
            || new.node_id.trim().is_empty()
            || new.idempotency_key.trim().is_empty()
        {
            return Err(JobError::InvalidJob(
                "job_id, operation_id, node_id, and idempotency_key are required".to_string(),
            ));
        }
        if new.max_attempts == 0 {
            return Err(JobError::InvalidJob(
                "max_attempts must be greater than zero".to_string(),
            ));
        }
        let payload_sha256 = canonical_payload_sha256(&new.payload);
        let key = (new.node_id.clone(), new.idempotency_key.clone());
        if let Some(job_id) = self.idempotency.get(&key) {
            let existing = self.jobs.get(job_id).expect("idempotency index is valid");
            if existing.payload_sha256 != payload_sha256
                || existing.kind != new.kind
                || existing.operation_id != new.operation_id
            {
                return Err(JobError::IdempotencyConflict);
            }
            return Ok(existing.clone());
        }
        if self.jobs.contains_key(&new.job_id) {
            return Err(JobError::JobIdConflict(new.job_id));
        }
        let job = Job {
            job_id: new.job_id.clone(),
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
        self.idempotency.insert(key, new.job_id.clone());
        self.jobs.insert(new.job_id, job.clone());
        Ok(job)
    }

    fn claim(&mut self, request: ClaimRequest) -> Result<Option<Job>, JobError> {
        if request.lease_ms <= 0 || request.lease_token.trim().is_empty() {
            return Err(JobError::InvalidJob(
                "lease_ms must be positive and lease_token is required".to_string(),
            ));
        }
        let selected = self
            .jobs
            .values()
            .filter(|job| {
                job.node_id == request.node_id
                    && matches!(job.status, JobStatus::Queued | JobStatus::RetryWait)
                    && job.available_at_ms <= request.now_ms
            })
            .min_by_key(|job| (job.available_at_ms, job.created_at_ms, job.job_id.clone()))
            .map(|job| job.job_id.clone());
        let Some(job_id) = selected else {
            return Ok(None);
        };
        let job = self.jobs.get_mut(&job_id).expect("selected job exists");
        job.status = JobStatus::Leased;
        job.attempt += 1;
        job.lease_owner = Some(request.instance_id);
        job.lease_token = Some(request.lease_token);
        job.lease_expires_at_ms = Some(request.now_ms + request.lease_ms);
        job.started_at_ms.get_or_insert(request.now_ms);
        job.updated_at_ms = request.now_ms;
        Ok(Some(job.clone()))
    }

    fn heartbeat(&mut self, request: HeartbeatRequest) -> Result<Job, JobError> {
        if request.lease_ms <= 0 {
            return Err(JobError::InvalidJob(
                "lease_ms must be positive".to_string(),
            ));
        }
        if self.recover_job_if_expired(&request.job_id, request.now_ms)? {
            return Err(JobError::StaleLease);
        }
        self.job_for_active_lease(&request.job_id, &request.lease_token, request.now_ms)?;
        self.append_events(&request.job_id, request.now_ms, request.events)?;
        let job = self
            .jobs
            .get_mut(&request.job_id)
            .expect("validated job exists");
        job.lease_expires_at_ms = Some(request.now_ms + request.lease_ms);
        job.updated_at_ms = request.now_ms;
        Ok(job.clone())
    }

    fn complete(&mut self, request: CompleteRequest) -> Result<Job, JobError> {
        let fingerprint = completion_fingerprint(
            &request.status,
            &request.result,
            request.error_message.as_str(),
        );
        let existing = self
            .jobs
            .get(&request.job_id)
            .ok_or_else(|| JobError::NotFound(request.job_id.clone()))?;
        if existing.status.is_terminal() {
            if existing.lease_token.as_deref() == Some(request.lease_token.as_str())
                && existing.completion_fingerprint.as_deref() == Some(fingerprint.as_str())
            {
                return Ok(existing.clone());
            }
            return Err(JobError::StaleLease);
        }
        if self.recover_job_if_expired(&request.job_id, request.now_ms)? {
            return Err(JobError::StaleLease);
        }
        self.job_for_active_lease(&request.job_id, &request.lease_token, request.now_ms)?;
        self.append_events(&request.job_id, request.now_ms, request.events)?;

        let job = self
            .jobs
            .get_mut(&request.job_id)
            .expect("validated job exists");
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
            job.lease_owner = None;
            job.lease_token = None;
            job.lease_expires_at_ms = None;
        }
        Ok(job.clone())
    }

    fn request_cancel(&mut self, job_id: &str, now_ms: i64) -> Result<Job, JobError> {
        let job = self
            .jobs
            .get_mut(job_id)
            .ok_or_else(|| JobError::NotFound(job_id.to_string()))?;
        match job.status {
            JobStatus::Queued | JobStatus::RetryWait => {
                job.status = JobStatus::Cancelled;
                job.completed_at_ms = Some(now_ms);
            }
            JobStatus::Leased => job.status = JobStatus::CancelRequested,
            JobStatus::CancelRequested => {}
            _ if job.status.is_terminal() => return Ok(job.clone()),
            _ => {
                return Err(JobError::InvalidTransition {
                    from: job.status.clone(),
                    to: "CANCELLED",
                });
            }
        }
        job.updated_at_ms = now_ms;
        Ok(job.clone())
    }

    fn recover_expired(&mut self, now_ms: i64) -> Result<Vec<Job>, JobError> {
        let mut recovered = Vec::new();
        for job in self.jobs.values_mut() {
            let expired = matches!(job.status, JobStatus::Leased | JobStatus::CancelRequested)
                && job
                    .lease_expires_at_ms
                    .is_some_and(|deadline| deadline <= now_ms);
            if !expired {
                continue;
            }
            recover_expired_job(job, now_ms);
            recovered.push(job.clone());
        }
        Ok(recovered)
    }

    fn get(&self, job_id: &str) -> Result<Option<Job>, JobError> {
        Ok(self.jobs.get(job_id).cloned())
    }

    fn list(&self) -> Result<Vec<Job>, JobError> {
        Ok(self.jobs.values().cloned().collect())
    }

    fn events(&self, job_id: &str, after_sequence: u64) -> Result<Vec<JobEvent>, JobError> {
        if !self.jobs.contains_key(job_id) {
            return Err(JobError::NotFound(job_id.to_string()));
        }
        Ok(self
            .events
            .get(job_id)
            .into_iter()
            .flat_map(|events| {
                events
                    .range((after_sequence + 1)..)
                    .map(|(_, event)| event.clone())
            })
            .collect())
    }
}

fn recover_expired_job(job: &mut Job, now_ms: i64) {
    if job.status == JobStatus::CancelRequested {
        job.status = JobStatus::NeedsAttention;
        job.error_message =
            Some("worker lease expired while cancellation outcome was unknown".to_string());
        job.completed_at_ms = Some(now_ms);
    } else if job.attempt < job.max_attempts {
        job.status = JobStatus::RetryWait;
        job.available_at_ms = now_ms + retry_backoff_ms(job.attempt);
        job.error_message =
            Some("worker lease expired; awaiting ledger reconciliation".to_string());
    } else {
        job.status = JobStatus::NeedsAttention;
        job.error_message = Some("worker lease expired and retry budget was exhausted".to_string());
        job.completed_at_ms = Some(now_ms);
    }
    job.lease_owner = None;
    job.lease_token = None;
    job.lease_expires_at_ms = None;
    job.updated_at_ms = now_ms;
}

pub fn canonical_payload_sha256(payload: &Value) -> String {
    let bytes = serde_json::to_vec(payload).expect("JSON values always serialize");
    format!("{:x}", Sha256::digest(bytes))
}

pub fn retry_backoff_ms(attempt: u32) -> i64 {
    RETRY_BACKOFF_MS
        .get(attempt.saturating_sub(1) as usize)
        .copied()
        .unwrap_or(*RETRY_BACKOFF_MS.last().expect("backoff is non-empty"))
}

pub fn aggregate_operation_status<'a>(
    statuses: impl IntoIterator<Item = &'a JobStatus>,
) -> OperationStatus {
    let statuses = statuses.into_iter().collect::<Vec<_>>();
    if statuses.is_empty() {
        return OperationStatus::Planned;
    }
    if statuses
        .iter()
        .any(|status| matches!(status, JobStatus::Failed | JobStatus::NeedsAttention))
    {
        return OperationStatus::Failed;
    }
    if statuses
        .iter()
        .all(|status| **status == JobStatus::Succeeded)
    {
        return OperationStatus::Succeeded;
    }
    if statuses
        .iter()
        .all(|status| **status == JobStatus::Cancelled)
    {
        return OperationStatus::Cancelled;
    }
    OperationStatus::Running
}

fn completion_fingerprint(status: &CompletionStatus, result: &Value, error: &str) -> String {
    let value = serde_json::json!({
        "status": status,
        "result": result,
        "error": error,
    });
    canonical_payload_sha256(&value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn new_job(id: &str) -> NewJob {
        NewJob {
            job_id: id.to_string(),
            operation_id: format!("op-{id}"),
            node_id: "node-a".to_string(),
            kind: JobKind::Install,
            payload: json!({"image": "registry/service@sha256:abc"}),
            idempotency_key: format!("key-{id}"),
            max_attempts: 3,
        }
    }

    fn claim(store: &mut MemoryJobStore, token: &str, now_ms: i64) -> Job {
        store
            .claim(ClaimRequest {
                node_id: "node-a".to_string(),
                instance_id: "worker-1".to_string(),
                lease_token: token.to_string(),
                now_ms,
                lease_ms: 30_000,
            })
            .unwrap()
            .unwrap()
    }

    #[test]
    fn enqueue_is_idempotent_but_rejects_changed_payload() {
        let mut store = MemoryJobStore::default();
        let original = store.enqueue(new_job("1"), 100).unwrap();
        let replay = store.enqueue(new_job("1"), 200).unwrap();
        assert_eq!(original, replay);

        let mut changed = new_job("different-id");
        changed.idempotency_key = "key-1".to_string();
        changed.payload = json!({"image": "different"});
        assert_eq!(
            store.enqueue(changed, 300),
            Err(JobError::IdempotencyConflict)
        );
    }

    #[test]
    fn only_target_node_can_claim_and_stale_lease_cannot_complete() {
        let mut store = MemoryJobStore::default();
        store.enqueue(new_job("1"), 0).unwrap();
        assert!(
            store
                .claim(ClaimRequest {
                    node_id: "node-b".to_string(),
                    instance_id: "other".to_string(),
                    lease_token: "bad".to_string(),
                    now_ms: 0,
                    lease_ms: 30_000,
                })
                .unwrap()
                .is_none()
        );
        let leased = claim(&mut store, "lease-1", 0);
        assert_eq!(leased.attempt, 1);
        let stale = store.complete(CompleteRequest {
            job_id: "1".to_string(),
            lease_token: "wrong".to_string(),
            status: CompletionStatus::Succeeded,
            result: json!({}),
            error_message: String::new(),
            now_ms: 1,
            events: vec![],
        });
        assert_eq!(stale, Err(JobError::StaleLease));
    }

    #[test]
    fn expired_requests_recover_the_lease_before_returning_stale() {
        let mut store = MemoryJobStore::default();
        store.enqueue(new_job("heartbeat"), 0).unwrap();
        claim(&mut store, "current-heartbeat", 0);

        let stale_before_deadline = store.heartbeat(HeartbeatRequest {
            job_id: "heartbeat".to_string(),
            lease_token: "old-token".to_string(),
            now_ms: 29_999,
            lease_ms: 30_000,
            events: vec![],
        });
        assert_eq!(stale_before_deadline, Err(JobError::StaleLease));
        assert_eq!(
            store.get("heartbeat").unwrap().unwrap().status,
            JobStatus::Leased
        );

        let stale_at_deadline = store.heartbeat(HeartbeatRequest {
            job_id: "heartbeat".to_string(),
            lease_token: "old-token".to_string(),
            now_ms: 30_000,
            lease_ms: 30_000,
            events: vec![],
        });
        assert_eq!(stale_at_deadline, Err(JobError::StaleLease));
        assert_eq!(
            store.get("heartbeat").unwrap().unwrap().status,
            JobStatus::RetryWait
        );

        store.enqueue(new_job("complete"), 0).unwrap();
        claim(&mut store, "current-complete", 0);
        let stale_after_deadline = store.complete(CompleteRequest {
            job_id: "complete".to_string(),
            lease_token: "current-complete".to_string(),
            status: CompletionStatus::Succeeded,
            result: json!({}),
            error_message: String::new(),
            now_ms: 30_001,
            events: vec![],
        });
        assert_eq!(stale_after_deadline, Err(JobError::StaleLease));
        assert_eq!(
            store.get("complete").unwrap().unwrap().status,
            JobStatus::RetryWait
        );
    }

    #[test]
    fn retryable_failure_obeys_backoff_and_retry_budget() {
        let mut store = MemoryJobStore::default();
        store.enqueue(new_job("1"), 0).unwrap();
        for (attempt, now) in [(1, 0), (2, 1_000), (3, 6_000)] {
            let token = format!("lease-{attempt}");
            let leased = claim(&mut store, &token, now);
            assert_eq!(leased.attempt, attempt);
            let completed = store
                .complete(CompleteRequest {
                    job_id: "1".to_string(),
                    lease_token: token,
                    status: CompletionStatus::RetryableFailure,
                    result: json!({}),
                    error_message: "temporary".to_string(),
                    now_ms: now,
                    events: vec![],
                })
                .unwrap();
            if attempt < 3 {
                assert_eq!(completed.status, JobStatus::RetryWait);
            } else {
                assert_eq!(completed.status, JobStatus::Failed);
            }
        }
    }

    #[test]
    fn terminal_completion_can_be_replayed_exactly_once() {
        let mut store = MemoryJobStore::default();
        store.enqueue(new_job("1"), 0).unwrap();
        claim(&mut store, "lease-1", 0);
        let request = CompleteRequest {
            job_id: "1".to_string(),
            lease_token: "lease-1".to_string(),
            status: CompletionStatus::Succeeded,
            result: json!({"container_id": "abc"}),
            error_message: String::new(),
            now_ms: 10,
            events: vec![],
        };
        let first = store.complete(request.clone()).unwrap();
        let replay = store.complete(request).unwrap();
        assert_eq!(first, replay);
    }

    #[test]
    fn expired_lease_requeues_then_requires_attention() {
        let mut store = MemoryJobStore::default();
        let mut job = new_job("1");
        job.max_attempts = 2;
        store.enqueue(job, 0).unwrap();
        claim(&mut store, "lease-1", 0);
        let first = store.recover_expired(30_000).unwrap();
        assert_eq!(first[0].status, JobStatus::RetryWait);
        claim(&mut store, "lease-2", 31_000);
        let second = store.recover_expired(61_000).unwrap();
        assert_eq!(second[0].status, JobStatus::NeedsAttention);
    }

    #[test]
    fn cancellation_is_immediate_before_claim_and_cooperative_after_claim() {
        let mut store = MemoryJobStore::default();
        store.enqueue(new_job("queued"), 0).unwrap();
        assert_eq!(
            store.request_cancel("queued", 1).unwrap().status,
            JobStatus::Cancelled
        );

        store.enqueue(new_job("leased"), 0).unwrap();
        claim(&mut store, "lease", 0);
        assert_eq!(
            store.request_cancel("leased", 1).unwrap().status,
            JobStatus::CancelRequested
        );
    }

    #[test]
    fn operation_status_is_derived_without_expanding_public_enum() {
        assert_eq!(
            aggregate_operation_status([&JobStatus::Queued]),
            OperationStatus::Running
        );
        assert_eq!(
            aggregate_operation_status([&JobStatus::Succeeded, &JobStatus::Succeeded]),
            OperationStatus::Succeeded
        );
        assert_eq!(
            aggregate_operation_status([&JobStatus::Succeeded, &JobStatus::NeedsAttention]),
            OperationStatus::Failed
        );
    }
}
