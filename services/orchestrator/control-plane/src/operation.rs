use crate::{Job, JobError, JobKind, JobStatus, JobStore, NewJob, canonical_payload_sha256};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use thiserror::Error;

pub const OPERATION_SCHEMA_VERSION: u16 = 1;
const PROJECT_MAX_CAS_ATTEMPTS: usize = 4;
const ENQUEUE_MAX_CAS_ATTEMPTS: usize = 4;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum DurableOperationMode {
    Apply,
    Rollback,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum DurableOperationStatus {
    Planned,
    Confirmed,
    Enqueuing,
    Running,
    Cancelling,
    Succeeded,
    Failed,
    Cancelled,
    NeedsAttention,
    RolledBack,
}

impl DurableOperationStatus {
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Succeeded
                | Self::Failed
                | Self::Cancelled
                | Self::NeedsAttention
                | Self::RolledBack
        )
    }

    fn is_recoverable(self) -> bool {
        matches!(self, Self::Enqueuing | Self::Running | Self::Cancelling)
            || self == Self::Confirmed
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PlannedJob {
    pub step_id: String,
    pub node_id: String,
    pub kind: JobKind,
    /// Step ids that must reach the condition-specific terminal state before
    /// this durable child job can be materialized.
    #[serde(default)]
    pub depends_on: Vec<String>,
    #[serde(default)]
    pub condition: PlannedJobCondition,
    #[serde(default)]
    pub payload: Value,
    #[serde(default = "default_max_attempts")]
    pub max_attempts: u32,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum PlannedJobCondition {
    #[default]
    OnSuccess,
    /// Materialize after dependencies are terminal and at least one ended in
    /// a known FAILED/CANCELLED state. NEEDS_ATTENTION never auto-compensates.
    OnFailure,
}

fn default_max_attempts() -> u32 {
    crate::DEFAULT_MAX_ATTEMPTS
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PlanOperation {
    pub operation_id: String,
    pub action: String,
    pub target_type: String,
    pub target_id: String,
    #[serde(default)]
    pub request: Value,
    pub jobs: Vec<PlannedJob>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct JobBinding {
    pub step_id: String,
    pub generation: u32,
    pub job_id: String,
    pub idempotency_key: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DurableOperation {
    pub schema_version: u16,
    pub operation_id: String,
    pub mode: DurableOperationMode,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rollback_of_operation_id: Option<String>,
    pub action: String,
    pub target_type: String,
    pub target_id: String,
    pub status: DurableOperationStatus,
    #[serde(default)]
    pub request: Value,
    pub plan_sha256: String,
    pub planned_jobs: Vec<PlannedJob>,
    #[serde(default)]
    pub job_bindings: Vec<JobBinding>,
    #[serde(default)]
    pub pending_step_ids: Vec<String>,
    #[serde(default)]
    pub attention_job_ids: Vec<String>,
    pub generation: u32,
    pub revision: u64,
    #[serde(default)]
    pub result: Value,
    #[serde(default)]
    pub error_message: String,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confirmed_at_ms: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at_ms: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finished_at_ms: Option<i64>,
}

impl DurableOperation {
    pub fn active_binding(&self, step_id: &str) -> Option<&JobBinding> {
        self.job_bindings
            .iter()
            .filter(|binding| binding.step_id == step_id)
            .max_by_key(|binding| binding.generation)
    }
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum OperationStoreError {
    #[error("operation persistence error: {0}")]
    Persistence(String),
    #[error("operation not found: {0}")]
    NotFound(String),
    #[error("operation already exists: {0}")]
    AlreadyExists(String),
    #[error("operation revision conflict: expected {expected}, actual {actual}")]
    RevisionConflict { expected: u64, actual: u64 },
    #[error("operation invariant failed: {0}")]
    Invariant(String),
}

pub trait OperationRepository {
    fn create(
        &mut self,
        operation: DurableOperation,
    ) -> Result<DurableOperation, OperationStoreError>;

    fn get(&self, operation_id: &str) -> Result<Option<DurableOperation>, OperationStoreError>;

    fn compare_and_swap(
        &mut self,
        expected_revision: u64,
        operation: DurableOperation,
    ) -> Result<DurableOperation, OperationStoreError>;

    fn recoverable(&self) -> Result<Vec<DurableOperation>, OperationStoreError>;

    fn list(&self) -> Result<Vec<DurableOperation>, OperationStoreError>;
}

#[derive(Debug, Clone, Default)]
pub struct MemoryOperationStore {
    operations: BTreeMap<String, DurableOperation>,
}

impl OperationRepository for MemoryOperationStore {
    fn create(
        &mut self,
        operation: DurableOperation,
    ) -> Result<DurableOperation, OperationStoreError> {
        validate_durable_operation(&operation)?;
        if self.operations.contains_key(&operation.operation_id) {
            return Err(OperationStoreError::AlreadyExists(operation.operation_id));
        }
        self.operations
            .insert(operation.operation_id.clone(), operation.clone());
        Ok(operation)
    }

    fn get(&self, operation_id: &str) -> Result<Option<DurableOperation>, OperationStoreError> {
        Ok(self.operations.get(operation_id).cloned())
    }

    fn compare_and_swap(
        &mut self,
        expected_revision: u64,
        operation: DurableOperation,
    ) -> Result<DurableOperation, OperationStoreError> {
        validate_durable_operation(&operation)?;
        let current = self
            .operations
            .get(&operation.operation_id)
            .ok_or_else(|| OperationStoreError::NotFound(operation.operation_id.clone()))?;
        if current.revision != expected_revision {
            return Err(OperationStoreError::RevisionConflict {
                expected: expected_revision,
                actual: current.revision,
            });
        }
        if operation.revision != expected_revision + 1 {
            return Err(OperationStoreError::Invariant(
                "a compare-and-swap must increment revision exactly once".to_string(),
            ));
        }
        ensure_immutable_fields(current, &operation)?;
        if !valid_status_transition(current.status, operation.status) {
            return Err(OperationStoreError::Invariant(format!(
                "invalid durable status transition from {:?} to {:?}",
                current.status, operation.status
            )));
        }
        self.operations
            .insert(operation.operation_id.clone(), operation.clone());
        Ok(operation)
    }

    fn recoverable(&self) -> Result<Vec<DurableOperation>, OperationStoreError> {
        Ok(self
            .operations
            .values()
            .filter(|operation| operation.status.is_recoverable())
            .cloned()
            .collect())
    }

    fn list(&self) -> Result<Vec<DurableOperation>, OperationStoreError> {
        Ok(self.operations.values().cloned().collect())
    }
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum OperationError {
    #[error(transparent)]
    Store(#[from] OperationStoreError),
    #[error(transparent)]
    Job(#[from] JobError),
    #[error("operation not found: {0}")]
    NotFound(String),
    #[error("operation idempotency key belongs to a different plan")]
    IdempotencyConflict,
    #[error("invalid operation plan: {0}")]
    InvalidPlan(String),
    #[error("invalid operation transition from {from:?} via {action}")]
    InvalidTransition {
        from: DurableOperationStatus,
        action: &'static str,
    },
}

pub struct OperationCoordinator<'a, O: OperationRepository, J: JobStore> {
    operations: &'a mut O,
    jobs: &'a mut J,
}

impl<'a, O: OperationRepository, J: JobStore> OperationCoordinator<'a, O, J> {
    pub fn new(operations: &'a mut O, jobs: &'a mut J) -> Self {
        Self { operations, jobs }
    }

    pub fn plan(
        &mut self,
        plan: PlanOperation,
        now_ms: i64,
    ) -> Result<DurableOperation, OperationError> {
        validate_plan(&plan)?;
        let plan_sha256 = plan_sha256(&plan);
        if let Some(existing) = self.operations.get(&plan.operation_id)? {
            if existing.plan_sha256 == plan_sha256
                && existing.mode == DurableOperationMode::Apply
                && existing.rollback_of_operation_id.is_none()
            {
                return Ok(existing);
            }
            return Err(OperationError::IdempotencyConflict);
        }
        let operation =
            operation_from_plan(plan, DurableOperationMode::Apply, None, plan_sha256, now_ms);
        Ok(self.operations.create(operation)?)
    }

    pub fn confirm(
        &mut self,
        operation_id: &str,
        now_ms: i64,
    ) -> Result<DurableOperation, OperationError> {
        let mut operation = self.required(operation_id)?;
        match operation.status {
            DurableOperationStatus::Planned => {
                operation.status = DurableOperationStatus::Confirmed;
                operation.confirmed_at_ms = Some(now_ms);
                self.save(operation, now_ms)
            }
            DurableOperationStatus::Confirmed => Ok(operation),
            from => Err(OperationError::InvalidTransition {
                from,
                action: "confirm",
            }),
        }
    }

    pub fn enqueue(
        &mut self,
        operation_id: &str,
        now_ms: i64,
    ) -> Result<DurableOperation, OperationError> {
        for attempt in 0..ENQUEUE_MAX_CAS_ATTEMPTS {
            match self.enqueue_once(operation_id, now_ms) {
                Err(OperationError::Store(OperationStoreError::RevisionConflict { .. }))
                    if attempt + 1 < ENQUEUE_MAX_CAS_ATTEMPTS =>
                {
                    // Materialization enqueues deterministic Job IDs before
                    // persisting their bindings. A control-plane worker may
                    // complete one of those Jobs and project the Operation in
                    // the same window. Reloading and replaying is safe because
                    // JobStore::enqueue is idempotent for the same identity.
                    continue;
                }
                result => return result,
            }
        }
        unreachable!("the final bounded enqueue attempt always returns")
    }

    fn enqueue_once(
        &mut self,
        operation_id: &str,
        now_ms: i64,
    ) -> Result<DurableOperation, OperationError> {
        let mut operation = self.required(operation_id)?;
        match operation.status {
            DurableOperationStatus::Confirmed => {
                operation.status = DurableOperationStatus::Enqueuing;
                operation = self.save(operation, now_ms)?;
            }
            DurableOperationStatus::Enqueuing => {}
            DurableOperationStatus::Running => return self.project(operation_id, now_ms),
            from => {
                return Err(OperationError::InvalidTransition {
                    from,
                    action: "enqueue",
                });
            }
        }
        self.materialize_pending(operation, now_ms)
    }

    pub fn cancel(
        &mut self,
        operation_id: &str,
        now_ms: i64,
    ) -> Result<DurableOperation, OperationError> {
        let mut operation = self.required(operation_id)?;
        match operation.status {
            DurableOperationStatus::Planned | DurableOperationStatus::Confirmed => {
                operation.status = DurableOperationStatus::Cancelled;
                operation.finished_at_ms = Some(now_ms);
                self.save(operation, now_ms)
            }
            DurableOperationStatus::Enqueuing | DurableOperationStatus::Running => {
                operation.status = DurableOperationStatus::Cancelling;
                operation = self.save(operation, now_ms)?;
                self.cancel_jobs(&operation, now_ms)?;
                self.project(operation_id, now_ms)
            }
            DurableOperationStatus::Cancelling => {
                self.cancel_jobs(&operation, now_ms)?;
                self.project(operation_id, now_ms)
            }
            DurableOperationStatus::Cancelled => Ok(operation),
            from => Err(OperationError::InvalidTransition {
                from,
                action: "cancel",
            }),
        }
    }

    pub fn retry(
        &mut self,
        operation_id: &str,
        now_ms: i64,
    ) -> Result<DurableOperation, OperationError> {
        let mut operation = self.required(operation_id)?;
        if operation.status == DurableOperationStatus::NeedsAttention {
            return Err(OperationError::InvalidTransition {
                from: operation.status,
                action: "retry_without_reconciliation",
            });
        }
        if operation.status != DurableOperationStatus::Failed {
            return Err(OperationError::InvalidTransition {
                from: operation.status,
                action: "retry",
            });
        }
        let mut pending = Vec::new();
        for planned in &operation.planned_jobs {
            let Some(binding) = operation.active_binding(&planned.step_id) else {
                // Dependency-aware materialization deliberately leaves
                // impossible descendants without a durable Job. A retry of a
                // definitively failed ancestor can make those descendants
                // runnable in the next generation, so preserve them as
                // pending instead of rejecting the durable plan.
                pending.push(planned.step_id.clone());
                continue;
            };
            let job = self
                .jobs
                .get(&binding.job_id)?
                .ok_or_else(|| JobError::NotFound(binding.job_id.clone()))?;
            match job.status {
                JobStatus::Failed | JobStatus::Cancelled => {
                    pending.push(planned.step_id.clone());
                }
                JobStatus::Succeeded => {}
                JobStatus::NeedsAttention => {
                    return Err(OperationError::InvalidTransition {
                        from: DurableOperationStatus::NeedsAttention,
                        action: "retry_without_reconciliation",
                    });
                }
                _ => {
                    return Err(OperationError::InvalidPlan(format!(
                        "failed operation contains non-terminal job {}",
                        job.job_id
                    )));
                }
            }
        }
        if pending.is_empty() {
            return Err(OperationError::InvalidPlan(
                "failed operation has no failed jobs to retry".to_string(),
            ));
        }
        operation.generation += 1;
        operation.pending_step_ids = pending;
        operation.status = DurableOperationStatus::Enqueuing;
        operation.error_message.clear();
        operation.finished_at_ms = None;
        operation = self.save(operation, now_ms)?;
        self.materialize_pending(operation, now_ms)
    }

    pub fn rollback(
        &mut self,
        source_operation_id: &str,
        rollback_plan: PlanOperation,
        now_ms: i64,
    ) -> Result<DurableOperation, OperationError> {
        let source = self.required(source_operation_id)?;
        if !matches!(
            source.status,
            DurableOperationStatus::Succeeded | DurableOperationStatus::Failed
        ) {
            return Err(OperationError::InvalidTransition {
                from: source.status,
                action: "rollback",
            });
        }
        if rollback_plan.operation_id == source_operation_id {
            return Err(OperationError::InvalidPlan(
                "rollback must have its own operation_id".to_string(),
            ));
        }
        validate_plan(&rollback_plan)?;
        let plan_sha256 = plan_sha256(&rollback_plan);
        if let Some(existing) = self.operations.get(&rollback_plan.operation_id)? {
            if existing.plan_sha256 == plan_sha256
                && existing.mode == DurableOperationMode::Rollback
                && existing.rollback_of_operation_id.as_deref() == Some(source_operation_id)
            {
                return Ok(existing);
            }
            return Err(OperationError::IdempotencyConflict);
        }
        let operation = operation_from_plan(
            rollback_plan,
            DurableOperationMode::Rollback,
            Some(source_operation_id.to_string()),
            plan_sha256,
            now_ms,
        );
        Ok(self.operations.create(operation)?)
    }

    pub fn project(
        &mut self,
        operation_id: &str,
        now_ms: i64,
    ) -> Result<DurableOperation, OperationError> {
        for attempt in 0..PROJECT_MAX_CAS_ATTEMPTS {
            match self.project_once(operation_id, now_ms) {
                Err(OperationError::Store(OperationStoreError::RevisionConflict { .. }))
                    if attempt + 1 < PROJECT_MAX_CAS_ATTEMPTS =>
                {
                    // Job completion and lease recovery can project the same
                    // Operation concurrently. Reload the newest durable
                    // revision and derive the projection again. Any child Job
                    // materialized before the conflict is deterministic and
                    // idempotent, so replaying this bounded section cannot
                    // create a second Job.
                    continue;
                }
                result => return result,
            }
        }
        unreachable!("the final bounded projection attempt always returns")
    }

    fn project_once(
        &mut self,
        operation_id: &str,
        now_ms: i64,
    ) -> Result<DurableOperation, OperationError> {
        let mut operation = self.required(operation_id)?;
        if !matches!(
            operation.status,
            DurableOperationStatus::Running | DurableOperationStatus::Cancelling
        ) {
            return Ok(operation);
        }
        if matches!(
            operation.status,
            DurableOperationStatus::Running | DurableOperationStatus::Cancelling
        ) && !operation.pending_step_ids.is_empty()
            // A missing Job behind an already-persisted binding is an
            // unprovable durable side-effect outcome.  Do not start more
            // compensation in that state; the cancelling projection below
            // will move the Operation to NEEDS_ATTENTION.
            && !self.has_missing_active_job(&operation)?
        {
            operation = self.materialize_pending(operation, now_ms)?;
        }
        let (mut status, mut result, mut error_message) = self.derive_projection(&operation)?;
        if operation.status == DurableOperationStatus::Cancelling {
            // A worker can finish a compensation Job while the projection is
            // being derived.  Never combine a stale BLOCKED projection with a
            // second, newer "no active jobs" observation: that used to move a
            // healthy cancellation to NEEDS_ATTENTION.  Once no active Job is
            // observed, re-run materialization and derive from the resulting
            // durable graph.  Terminal Job states are monotonic, so reaching
            // the same fixed point proves that the graph is genuinely stuck.
            loop {
                if status != DurableOperationStatus::Cancelling
                    || !has_blocked_pending_compensation(&operation, &result)
                    || self.cancellation_has_active_jobs(&operation)?
                {
                    break;
                }

                let previous = operation.clone();
                operation = self.materialize_pending(operation, now_ms)?;
                let next = self.derive_projection(&operation)?;
                if operation == previous && next == (status, result.clone(), error_message.clone())
                {
                    status = DurableOperationStatus::NeedsAttention;
                    error_message =
                        "applicable compensation cannot be materialized from the durable dependency graph"
                            .to_string();
                    break;
                }
                (status, result, error_message) = next;
            }
        }
        if operation.status == status
            && operation.result == result
            && operation.error_message == error_message
        {
            return Ok(operation);
        }
        operation.status = status;
        operation.result = result;
        operation.error_message = error_message;
        if status.is_terminal() {
            operation.finished_at_ms.get_or_insert(now_ms);
        }
        self.save(operation, now_ms)
    }

    pub fn recover(&mut self, now_ms: i64) -> Result<Vec<DurableOperation>, OperationError> {
        let expired = self.jobs.recover_expired(now_ms)?;
        for job in expired {
            if job.status == JobStatus::RetryWait && !job_kind_is_observation(&job.kind) {
                let cancelled = self.jobs.request_cancel(&job.job_id, now_ms)?;
                self.mark_needs_attention(
                    &job.operation_id,
                    &cancelled.job_id,
                    "worker lease expired with an unproven side-effect outcome",
                    now_ms,
                )?;
            } else if job.status == JobStatus::NeedsAttention {
                self.mark_needs_attention(
                    &job.operation_id,
                    &job.job_id,
                    job.error_message
                        .as_deref()
                        .unwrap_or("job outcome requires reconciliation"),
                    now_ms,
                )?;
            }
        }

        let recoverable = self.operations.recoverable()?;
        let mut recovered = Vec::new();
        for operation in recoverable {
            let current = self.required(&operation.operation_id)?;
            let next = match current.status {
                DurableOperationStatus::Confirmed
                    if current.request.get("auto_enqueue").and_then(Value::as_bool)
                        == Some(true) =>
                {
                    self.enqueue(&current.operation_id, now_ms)?
                }
                DurableOperationStatus::Enqueuing => self.materialize_pending(current, now_ms)?,
                DurableOperationStatus::Cancelling => {
                    self.cancel_jobs(&current, now_ms)?;
                    self.project(&current.operation_id, now_ms)?
                }
                DurableOperationStatus::Running => self.project(&current.operation_id, now_ms)?,
                _ => current,
            };
            recovered.push(next);
        }
        Ok(recovered)
    }

    fn required(&self, operation_id: &str) -> Result<DurableOperation, OperationError> {
        self.operations
            .get(operation_id)?
            .ok_or_else(|| OperationError::NotFound(operation_id.to_string()))
    }

    fn save(
        &mut self,
        mut operation: DurableOperation,
        now_ms: i64,
    ) -> Result<DurableOperation, OperationError> {
        let expected_revision = operation.revision;
        operation.revision += 1;
        operation.updated_at_ms = now_ms;
        Ok(self
            .operations
            .compare_and_swap(expected_revision, operation)?)
    }

    fn materialize_pending(
        &mut self,
        mut operation: DurableOperation,
        now_ms: i64,
    ) -> Result<DurableOperation, OperationError> {
        let pending = operation.pending_step_ids.clone();
        let compensation_steps = compensation_step_ids(&operation);
        let mut ready = Vec::new();
        for step_id in &pending {
            // Cancellation freezes the forward graph.  ON_FAILURE steps and
            // their downstream ON_SUCCESS cleanup are the only jobs allowed
            // to become durable after cancellation intent is persisted.
            if operation.status == DurableOperationStatus::Cancelling
                && !compensation_steps.contains(step_id)
            {
                continue;
            }
            let planned = operation
                .planned_jobs
                .iter()
                .find(|planned| planned.step_id == *step_id)
                .ok_or_else(|| {
                    OperationError::InvalidPlan(format!("unknown pending step {step_id}"))
                })?;
            if self.planned_job_ready(&operation, planned)? {
                ready.push(step_id.clone());
            }
        }
        for step_id in &ready {
            let planned = operation
                .planned_jobs
                .iter()
                .find(|planned| planned.step_id == *step_id)
                .ok_or_else(|| {
                    OperationError::InvalidPlan(format!("unknown pending step {step_id}"))
                })?;
            let binding = binding_for(&operation, planned);
            let job = self.jobs.enqueue(
                NewJob {
                    job_id: binding.job_id.clone(),
                    operation_id: operation.operation_id.clone(),
                    node_id: planned.node_id.clone(),
                    kind: planned.kind.clone(),
                    payload: planned.payload.clone(),
                    idempotency_key: binding.idempotency_key.clone(),
                    max_attempts: planned.max_attempts,
                },
                now_ms,
            )?;
            debug_assert_eq!(job.job_id, binding.job_id);
            if !operation.job_bindings.iter().any(|existing| {
                existing.step_id == binding.step_id && existing.generation == binding.generation
            }) {
                operation.job_bindings.push(binding);
            }
        }
        operation
            .pending_step_ids
            .retain(|step_id| !ready.contains(step_id));
        let was_enqueuing = operation.status == DurableOperationStatus::Enqueuing;
        if was_enqueuing {
            operation.status = DurableOperationStatus::Running;
            operation.started_at_ms.get_or_insert(now_ms);
        }
        if ready.is_empty() && !was_enqueuing {
            Ok(operation)
        } else {
            self.save(operation, now_ms)
        }
    }

    fn planned_job_ready(
        &self,
        operation: &DurableOperation,
        planned: &PlannedJob,
    ) -> Result<bool, OperationError> {
        if operation.status == DurableOperationStatus::Cancelling
            && planned.condition == PlannedJobCondition::OnFailure
        {
            return self.cancellation_compensator_ready(operation, planned);
        }
        if planned.depends_on.is_empty() {
            return Ok(planned.condition == PlannedJobCondition::OnSuccess);
        }
        let mut statuses = Vec::with_capacity(planned.depends_on.len());
        for dependency in &planned.depends_on {
            let Some(binding) = operation.active_binding(dependency) else {
                // An unbound success step is blocked by an earlier failure and
                // is treated as skipped for ON_FAILURE compensation.
                if planned.condition == PlannedJobCondition::OnSuccess {
                    return Ok(false);
                }
                continue;
            };
            let status = self
                .jobs
                .get(&binding.job_id)?
                .ok_or_else(|| JobError::NotFound(binding.job_id.clone()))?
                .status;
            statuses.push(status);
        }
        Ok(match planned.condition {
            PlannedJobCondition::OnSuccess => {
                statuses.len() == planned.depends_on.len()
                    && statuses
                        .iter()
                        .all(|status| *status == JobStatus::Succeeded)
            }
            PlannedJobCondition::OnFailure => {
                !statuses.contains(&JobStatus::NeedsAttention)
                    && statuses.iter().all(JobStatus::is_terminal)
                    && statuses
                        .iter()
                        .any(|status| matches!(status, JobStatus::Failed | JobStatus::Cancelled))
            }
        })
    }

    fn cancel_jobs(
        &mut self,
        operation: &DurableOperation,
        now_ms: i64,
    ) -> Result<(), OperationError> {
        let compensation_steps = compensation_step_ids(operation);
        let mut job_ids = BTreeSet::new();
        for planned in &operation.planned_jobs {
            if compensation_steps.contains(&planned.step_id) {
                continue;
            }
            if let Some(binding) = operation.active_binding(&planned.step_id) {
                job_ids.insert(binding.job_id.clone());
            } else if operation.pending_step_ids.contains(&planned.step_id) {
                // Enqueue is intentionally a two-write protocol.  The
                // deterministic Job may exist even if its binding CAS did not
                // complete, so cancellation must still find it.
                job_ids.insert(binding_for(operation, planned).job_id);
            }
        }
        for job_id in job_ids {
            if self
                .jobs
                .get(&job_id)?
                .is_some_and(|job| !job.status.is_terminal())
            {
                self.jobs.request_cancel(&job_id, now_ms)?;
            }
        }
        Ok(())
    }

    fn derive_projection(
        &self,
        operation: &DurableOperation,
    ) -> Result<(DurableOperationStatus, Value, String), OperationError> {
        if !operation.attention_job_ids.is_empty() {
            return Ok((
                DurableOperationStatus::NeedsAttention,
                operation.result.clone(),
                operation.error_message.clone(),
            ));
        }
        if operation.status == DurableOperationStatus::Cancelling {
            return self.derive_cancelling_projection(operation);
        }
        let mut jobs = Vec::with_capacity(operation.planned_jobs.len());
        let mut statuses = Vec::with_capacity(operation.planned_jobs.len());
        let mut pending_success = false;
        let mut result = Map::new();
        for planned in &operation.planned_jobs {
            let binding = operation
                .active_binding(&planned.step_id)
                .cloned()
                .or_else(|| {
                    (operation.status == DurableOperationStatus::Cancelling
                        && operation.pending_step_ids.contains(&planned.step_id))
                    .then(|| binding_for(operation, planned))
                });
            let Some(binding) = binding else {
                if operation.pending_step_ids.contains(&planned.step_id) {
                    let pending_status = if planned.condition == PlannedJobCondition::OnFailure {
                        "DORMANT"
                    } else if self.planned_step_is_impossible(
                        operation,
                        planned,
                        &mut BTreeSet::new(),
                    )? {
                        "SKIPPED"
                    } else {
                        pending_success = true;
                        "BLOCKED"
                    };
                    result.insert(
                        planned.step_id.clone(),
                        json!({
                            "job_id": Value::Null,
                            "status": pending_status,
                            "attempt": 0,
                            "result": Value::Null,
                            "error_message": Value::Null,
                        }),
                    );
                    continue;
                }
                return Err(OperationError::InvalidPlan(format!(
                    "step {} has no active job binding",
                    planned.step_id
                )));
            };
            let Some(job) = self.jobs.get(&binding.job_id)? else {
                if operation.status == DurableOperationStatus::Cancelling
                    && operation.pending_step_ids.contains(&planned.step_id)
                {
                    statuses.push(JobStatus::Cancelled);
                    result.insert(
                        planned.step_id.clone(),
                        json!({
                            "job_id": Value::Null,
                            "status": JobStatus::Cancelled,
                            "attempt": 0,
                            "result": Value::Null,
                            "error_message": Value::Null,
                        }),
                    );
                    continue;
                }
                return Err(JobError::NotFound(binding.job_id).into());
            };
            result.insert(planned.step_id.clone(), job_projection(&job));
            statuses.push(job.status.clone());
            jobs.push(job);
        }
        let status = if statuses.contains(&JobStatus::NeedsAttention) {
            DurableOperationStatus::NeedsAttention
        } else if operation.status == DurableOperationStatus::Cancelling {
            if statuses.iter().all(|status| status.is_terminal()) {
                if statuses.contains(&JobStatus::Failed) {
                    DurableOperationStatus::Failed
                } else {
                    DurableOperationStatus::Cancelled
                }
            } else {
                DurableOperationStatus::Cancelling
            }
        } else if !pending_success
            && statuses
                .iter()
                .all(|status| *status == JobStatus::Succeeded)
        {
            match operation.mode {
                DurableOperationMode::Apply => DurableOperationStatus::Succeeded,
                DurableOperationMode::Rollback => DurableOperationStatus::RolledBack,
            }
        } else if statuses.contains(&JobStatus::Failed)
            && statuses.iter().all(|status| status.is_terminal())
        {
            DurableOperationStatus::Failed
        } else if statuses.iter().all(|status| status.is_terminal()) && !pending_success {
            if statuses.contains(&JobStatus::Failed) {
                DurableOperationStatus::Failed
            } else {
                DurableOperationStatus::Cancelled
            }
        } else {
            DurableOperationStatus::Running
        };
        let error_message = jobs
            .iter()
            .filter_map(|job| {
                job.error_message
                    .as_ref()
                    .map(|message| format!("{}: {message}", job.job_id))
            })
            .collect::<Vec<_>>()
            .join("; ");
        Ok((status, Value::Object(result), error_message))
    }

    /// Project a cancellation independently from the normal success/failure
    /// aggregate.  Forward failures are expected cancellation triggers; only
    /// failed, cancelled, or unprovable compensation requires attention.
    fn derive_cancelling_projection(
        &self,
        operation: &DurableOperation,
    ) -> Result<(DurableOperationStatus, Value, String), OperationError> {
        let compensation_steps = compensation_step_ids(operation);
        let mut result = Map::new();
        let mut jobs = Vec::new();
        let mut forward_pending = false;
        let mut compensation_pending = false;
        let mut needs_attention = false;
        let mut attention_reasons = Vec::new();

        for planned in &operation.planned_jobs {
            let is_compensation = compensation_steps.contains(&planned.step_id);
            let active_binding = operation.active_binding(&planned.step_id).cloned();
            let candidate_binding = active_binding.clone().or_else(|| {
                operation
                    .pending_step_ids
                    .contains(&planned.step_id)
                    .then(|| binding_for(operation, planned))
            });
            let job = match candidate_binding.as_ref() {
                Some(binding) => self.jobs.get(&binding.job_id)?,
                None => None,
            };

            if let Some(job) = job {
                result.insert(planned.step_id.clone(), job_projection(&job));
                if is_compensation {
                    match &job.status {
                        JobStatus::Failed | JobStatus::Cancelled | JobStatus::NeedsAttention => {
                            needs_attention = true;
                            attention_reasons.push(format!(
                                "compensation step {} ended in {:?}",
                                planned.step_id, job.status
                            ));
                        }
                        status if !status.is_terminal() => compensation_pending = true,
                        JobStatus::Succeeded => {}
                        _ => unreachable!("all terminal compensation states are handled"),
                    }
                } else if job.status == JobStatus::NeedsAttention {
                    needs_attention = true;
                    attention_reasons.push(format!(
                        "forward step {} has an unproven cancellation outcome",
                        planned.step_id
                    ));
                } else if !job.status.is_terminal() {
                    forward_pending = true;
                }
                jobs.push(job);
                continue;
            }

            if let Some(binding) = active_binding {
                // A binding proves that a durable Job should exist.  Its
                // absence is not equivalent to a successfully skipped step.
                needs_attention = true;
                attention_reasons.push(format!(
                    "step {} references missing durable job {}",
                    planned.step_id, binding.job_id
                ));
                result.insert(
                    planned.step_id.clone(),
                    json!({
                        "job_id": binding.job_id,
                        "status": "UNKNOWN",
                        "attempt": 0,
                        "result": Value::Null,
                        "error_message": "durable job is missing",
                    }),
                );
                continue;
            }

            if !operation.pending_step_ids.contains(&planned.step_id) {
                return Err(OperationError::InvalidPlan(format!(
                    "step {} has no active job binding",
                    planned.step_id
                )));
            }

            if !is_compensation {
                // This forward step was frozen before it became durable.
                result.insert(
                    planned.step_id.clone(),
                    json!({
                        "job_id": Value::Null,
                        "status": JobStatus::Cancelled,
                        "attempt": 0,
                        "result": Value::Null,
                        "error_message": Value::Null,
                    }),
                );
                continue;
            }

            let (pending_status, waits_for_compensation) = if planned.condition
                == PlannedJobCondition::OnFailure
            {
                if self.cancellation_compensation_applicable(operation, planned)? {
                    ("BLOCKED", true)
                } else {
                    ("DORMANT", false)
                }
            } else if self.planned_step_is_impossible(operation, planned, &mut BTreeSet::new())? {
                ("SKIPPED", false)
            } else {
                ("BLOCKED", true)
            };
            compensation_pending |= waits_for_compensation;
            result.insert(
                planned.step_id.clone(),
                json!({
                    "job_id": Value::Null,
                    "status": pending_status,
                    "attempt": 0,
                    "result": Value::Null,
                    "error_message": Value::Null,
                }),
            );
        }

        let status = if needs_attention {
            DurableOperationStatus::NeedsAttention
        } else if forward_pending || compensation_pending {
            DurableOperationStatus::Cancelling
        } else {
            DurableOperationStatus::Cancelled
        };
        let mut error_messages = jobs
            .iter()
            .filter_map(|job| {
                job.error_message
                    .as_ref()
                    .map(|message| format!("{}: {message}", job.job_id))
            })
            .collect::<Vec<_>>();
        error_messages.extend(attention_reasons);
        Ok((status, Value::Object(result), error_messages.join("; ")))
    }

    fn has_missing_active_job(&self, operation: &DurableOperation) -> Result<bool, OperationError> {
        for planned in &operation.planned_jobs {
            if let Some(binding) = operation.active_binding(&planned.step_id)
                && self.jobs.get(&binding.job_id)?.is_none()
            {
                return Ok(true);
            }
        }
        Ok(false)
    }

    fn cancellation_has_active_jobs(
        &self,
        operation: &DurableOperation,
    ) -> Result<bool, OperationError> {
        for planned in &operation.planned_jobs {
            if let Some(job) = self.materialized_job(operation, planned)?
                && !job.status.is_terminal()
            {
                return Ok(true);
            }
        }
        Ok(false)
    }

    fn cancellation_compensator_ready(
        &self,
        operation: &DurableOperation,
        planned: &PlannedJob,
    ) -> Result<bool, OperationError> {
        if !self.cancellation_compensation_applicable(operation, planned)? {
            return Ok(false);
        }
        let compensation_steps = compensation_step_ids(operation);

        // Never race compensation against a forward Job whose cancellation
        // outcome is still unknown.
        for forward in operation
            .planned_jobs
            .iter()
            .filter(|job| !compensation_steps.contains(&job.step_id))
        {
            if let Some(job) = self.materialized_job(operation, forward)?
                && !job.status.is_terminal()
            {
                return Ok(false);
            }
        }

        for dependency_id in &planned.depends_on {
            let dependency = operation
                .planned_jobs
                .iter()
                .find(|candidate| candidate.step_id == *dependency_id)
                .ok_or_else(|| {
                    OperationError::InvalidPlan(format!(
                        "step {} depends on unknown step {dependency_id}",
                        planned.step_id
                    ))
                })?;
            match self.materialized_job(operation, dependency)? {
                Some(job) if job.status == JobStatus::NeedsAttention => return Ok(false),
                Some(job) if !job.status.is_terminal() => return Ok(false),
                Some(_) => {}
                None if compensation_steps.contains(dependency_id)
                    && self.cancellation_compensation_applicable(operation, dependency)? =>
                {
                    return Ok(false);
                }
                None => {}
            }
        }
        Ok(true)
    }

    fn cancellation_compensation_applicable(
        &self,
        operation: &DurableOperation,
        planned: &PlannedJob,
    ) -> Result<bool, OperationError> {
        let mut visiting = BTreeSet::new();
        for dependency in &planned.depends_on {
            if self.step_has_materialized_evidence(operation, dependency, &mut visiting)? {
                return Ok(true);
            }
        }
        Ok(false)
    }

    fn step_has_materialized_evidence(
        &self,
        operation: &DurableOperation,
        step_id: &str,
        visiting: &mut BTreeSet<String>,
    ) -> Result<bool, OperationError> {
        if !visiting.insert(step_id.to_string()) {
            return Ok(false);
        }
        let planned = operation
            .planned_jobs
            .iter()
            .find(|candidate| candidate.step_id == step_id)
            .ok_or_else(|| {
                OperationError::InvalidPlan(format!("unknown planned step {step_id}"))
            })?;
        if operation.active_binding(step_id).is_some()
            || (operation.pending_step_ids.contains(&planned.step_id)
                && self
                    .jobs
                    .get(&binding_for(operation, planned).job_id)?
                    .is_some())
        {
            visiting.remove(step_id);
            return Ok(true);
        }
        for dependency in &planned.depends_on {
            if self.step_has_materialized_evidence(operation, dependency, visiting)? {
                visiting.remove(step_id);
                return Ok(true);
            }
        }
        visiting.remove(step_id);
        Ok(false)
    }

    fn materialized_job(
        &self,
        operation: &DurableOperation,
        planned: &PlannedJob,
    ) -> Result<Option<Job>, OperationError> {
        if let Some(binding) = operation.active_binding(&planned.step_id) {
            return Ok(self.jobs.get(&binding.job_id)?);
        }
        if operation.pending_step_ids.contains(&planned.step_id) {
            return Ok(self.jobs.get(&binding_for(operation, planned).job_id)?);
        }
        Ok(None)
    }

    fn planned_step_is_impossible(
        &self,
        operation: &DurableOperation,
        planned: &PlannedJob,
        visiting: &mut BTreeSet<String>,
    ) -> Result<bool, OperationError> {
        if !visiting.insert(planned.step_id.clone()) {
            return Err(OperationError::InvalidPlan(format!(
                "dependency cycle reaches step {}",
                planned.step_id
            )));
        }
        let mut observed_failure = false;
        for dependency_id in &planned.depends_on {
            match operation.active_binding(dependency_id) {
                Some(binding) => {
                    let status = self
                        .jobs
                        .get(&binding.job_id)?
                        .ok_or_else(|| JobError::NotFound(binding.job_id.clone()))?
                        .status;
                    match planned.condition {
                        PlannedJobCondition::OnSuccess => {
                            if status.is_terminal() && status != JobStatus::Succeeded {
                                visiting.remove(&planned.step_id);
                                return Ok(true);
                            }
                        }
                        PlannedJobCondition::OnFailure => {
                            if !status.is_terminal() {
                                visiting.remove(&planned.step_id);
                                return Ok(false);
                            }
                            if status == JobStatus::NeedsAttention {
                                visiting.remove(&planned.step_id);
                                return Ok(true);
                            }
                            observed_failure |=
                                matches!(status, JobStatus::Failed | JobStatus::Cancelled);
                        }
                    }
                }
                None => {
                    let dependency = operation
                        .planned_jobs
                        .iter()
                        .find(|candidate| candidate.step_id == *dependency_id)
                        .ok_or_else(|| {
                            OperationError::InvalidPlan(format!(
                                "step {} depends on unknown step {dependency_id}",
                                planned.step_id
                            ))
                        })?;
                    if self.planned_step_is_impossible(operation, dependency, visiting)? {
                        if planned.condition == PlannedJobCondition::OnSuccess {
                            visiting.remove(&planned.step_id);
                            return Ok(true);
                        }
                        // An impossible OnFailure dependency is a skipped
                        // compensation branch. It cannot satisfy a downstream
                        // OnSuccess step, but it is terminal for deciding
                        // whether another OnFailure branch can still run.
                        continue;
                    }
                    if planned.condition == PlannedJobCondition::OnFailure {
                        visiting.remove(&planned.step_id);
                        return Ok(false);
                    }
                }
            }
        }
        visiting.remove(&planned.step_id);
        Ok(match planned.condition {
            PlannedJobCondition::OnSuccess => false,
            PlannedJobCondition::OnFailure => !observed_failure,
        })
    }

    fn mark_needs_attention(
        &mut self,
        operation_id: &str,
        job_id: &str,
        reason: &str,
        now_ms: i64,
    ) -> Result<(), OperationError> {
        let mut operation = self.required(operation_id)?;
        if !operation
            .attention_job_ids
            .iter()
            .any(|existing| existing == job_id)
        {
            operation.attention_job_ids.push(job_id.to_string());
        }
        operation.status = DurableOperationStatus::NeedsAttention;
        operation.error_message = reason.to_string();
        operation.finished_at_ms.get_or_insert(now_ms);
        self.save(operation, now_ms)?;
        Ok(())
    }
}

/// Cancellation compensation is rooted at every ON_FAILURE step and includes
/// the success-only cleanup steps that depend on those compensators.
fn compensation_step_ids(operation: &DurableOperation) -> BTreeSet<String> {
    let mut compensation = operation
        .planned_jobs
        .iter()
        .filter(|planned| planned.condition == PlannedJobCondition::OnFailure)
        .map(|planned| planned.step_id.clone())
        .collect::<BTreeSet<_>>();
    loop {
        let before = compensation.len();
        for planned in &operation.planned_jobs {
            if planned
                .depends_on
                .iter()
                .any(|dependency| compensation.contains(dependency))
            {
                compensation.insert(planned.step_id.clone());
            }
        }
        if compensation.len() == before {
            return compensation;
        }
    }
}

fn has_blocked_pending_compensation(operation: &DurableOperation, result: &Value) -> bool {
    let compensation_steps = compensation_step_ids(operation);
    operation.pending_step_ids.iter().any(|step_id| {
        compensation_steps.contains(step_id) && result[step_id]["status"] == "BLOCKED"
    })
}

fn operation_from_plan(
    plan: PlanOperation,
    mode: DurableOperationMode,
    rollback_of_operation_id: Option<String>,
    plan_sha256: String,
    now_ms: i64,
) -> DurableOperation {
    let pending_step_ids = plan.jobs.iter().map(|job| job.step_id.clone()).collect();
    DurableOperation {
        schema_version: OPERATION_SCHEMA_VERSION,
        operation_id: plan.operation_id,
        mode,
        rollback_of_operation_id,
        action: plan.action,
        target_type: plan.target_type,
        target_id: plan.target_id,
        status: DurableOperationStatus::Planned,
        request: plan.request,
        plan_sha256,
        planned_jobs: plan.jobs,
        job_bindings: Vec::new(),
        pending_step_ids,
        attention_job_ids: Vec::new(),
        generation: 0,
        revision: 1,
        result: Value::Object(Map::new()),
        error_message: String::new(),
        created_at_ms: now_ms,
        updated_at_ms: now_ms,
        confirmed_at_ms: None,
        started_at_ms: None,
        finished_at_ms: None,
    }
}

fn validate_plan(plan: &PlanOperation) -> Result<(), OperationError> {
    if plan.operation_id.trim().is_empty()
        || plan.action.trim().is_empty()
        || plan.target_type.trim().is_empty()
        || plan.target_id.trim().is_empty()
    {
        return Err(OperationError::InvalidPlan(
            "operation_id, action, target_type, and target_id are required".to_string(),
        ));
    }
    if plan.jobs.is_empty() {
        return Err(OperationError::InvalidPlan(
            "an executable operation requires at least one job".to_string(),
        ));
    }
    let mut step_ids = BTreeSet::new();
    for job in &plan.jobs {
        if job.step_id.trim().is_empty() || job.node_id.trim().is_empty() {
            return Err(OperationError::InvalidPlan(
                "every job requires step_id and node_id".to_string(),
            ));
        }
        if job.max_attempts == 0 {
            return Err(OperationError::InvalidPlan(format!(
                "step {} has zero max_attempts",
                job.step_id
            )));
        }
        if !step_ids.insert(job.step_id.clone()) {
            return Err(OperationError::InvalidPlan(format!(
                "duplicate step_id {}",
                job.step_id
            )));
        }
    }
    for job in &plan.jobs {
        let mut dependencies = BTreeSet::new();
        for dependency in &job.depends_on {
            if dependency == &job.step_id || !step_ids.contains(dependency) {
                return Err(OperationError::InvalidPlan(format!(
                    "step {} has unknown or self dependency {dependency}",
                    job.step_id
                )));
            }
            if !dependencies.insert(dependency) {
                return Err(OperationError::InvalidPlan(format!(
                    "step {} repeats dependency {dependency}",
                    job.step_id
                )));
            }
        }
        if job.condition == PlannedJobCondition::OnFailure && job.depends_on.is_empty() {
            return Err(OperationError::InvalidPlan(format!(
                "ON_FAILURE step {} requires at least one dependency",
                job.step_id
            )));
        }
    }
    let mut resolved = BTreeSet::new();
    loop {
        let before = resolved.len();
        for job in &plan.jobs {
            if !resolved.contains(&job.step_id)
                && job
                    .depends_on
                    .iter()
                    .all(|dependency| resolved.contains(dependency))
            {
                resolved.insert(job.step_id.clone());
            }
        }
        if resolved.len() == plan.jobs.len() {
            break;
        }
        if resolved.len() == before {
            return Err(OperationError::InvalidPlan(
                "planned job dependency graph contains a cycle".to_string(),
            ));
        }
    }
    Ok(())
}

pub fn validate_durable_operation(operation: &DurableOperation) -> Result<(), OperationStoreError> {
    if operation.schema_version != OPERATION_SCHEMA_VERSION {
        return Err(OperationStoreError::Invariant(format!(
            "unsupported operation schema version {}",
            operation.schema_version
        )));
    }
    if operation.revision == 0 || operation.generation > operation.revision as u32 {
        return Err(OperationStoreError::Invariant(
            "operation revision/generation is invalid".to_string(),
        ));
    }
    let planned_steps = operation
        .planned_jobs
        .iter()
        .map(|job| job.step_id.as_str())
        .collect::<BTreeSet<_>>();
    if planned_steps.len() != operation.planned_jobs.len()
        || operation
            .pending_step_ids
            .iter()
            .any(|step| !planned_steps.contains(step.as_str()))
        || operation
            .job_bindings
            .iter()
            .any(|binding| !planned_steps.contains(binding.step_id.as_str()))
    {
        return Err(OperationStoreError::Invariant(
            "operation contains duplicate or unknown job steps".to_string(),
        ));
    }
    Ok(())
}

pub fn validate_durable_operation_update(
    current: &DurableOperation,
    expected_revision: u64,
    next: &DurableOperation,
) -> Result<(), OperationStoreError> {
    validate_durable_operation(next)?;
    if current.revision != expected_revision {
        return Err(OperationStoreError::RevisionConflict {
            expected: expected_revision,
            actual: current.revision,
        });
    }
    if next.revision != expected_revision + 1 {
        return Err(OperationStoreError::Invariant(
            "a compare-and-swap must increment revision exactly once".to_string(),
        ));
    }
    ensure_immutable_fields(current, next)?;
    if !valid_status_transition(current.status, next.status) {
        return Err(OperationStoreError::Invariant(format!(
            "invalid durable status transition from {:?} to {:?}",
            current.status, next.status
        )));
    }
    Ok(())
}

fn ensure_immutable_fields(
    current: &DurableOperation,
    next: &DurableOperation,
) -> Result<(), OperationStoreError> {
    if current.schema_version != next.schema_version
        || current.operation_id != next.operation_id
        || current.mode != next.mode
        || current.rollback_of_operation_id != next.rollback_of_operation_id
        || current.action != next.action
        || current.target_type != next.target_type
        || current.target_id != next.target_id
        || current.request != next.request
        || current.plan_sha256 != next.plan_sha256
        || current.planned_jobs != next.planned_jobs
        || current.created_at_ms != next.created_at_ms
    {
        return Err(OperationStoreError::Invariant(
            "immutable operation plan fields changed".to_string(),
        ));
    }
    if next.generation < current.generation || next.generation > current.generation + 1 {
        return Err(OperationStoreError::Invariant(
            "operation generation must be monotonic".to_string(),
        ));
    }
    Ok(())
}

fn valid_status_transition(from: DurableOperationStatus, to: DurableOperationStatus) -> bool {
    from == to
        || matches!(
            (from, to),
            (
                DurableOperationStatus::Planned,
                DurableOperationStatus::Confirmed
            ) | (
                DurableOperationStatus::Planned,
                DurableOperationStatus::Cancelled
            ) | (
                DurableOperationStatus::Confirmed,
                DurableOperationStatus::Enqueuing
            ) | (
                DurableOperationStatus::Confirmed,
                DurableOperationStatus::Cancelled
            ) | (
                DurableOperationStatus::Enqueuing,
                DurableOperationStatus::Running
            ) | (
                DurableOperationStatus::Enqueuing,
                DurableOperationStatus::Cancelling
            ) | (
                DurableOperationStatus::Enqueuing,
                DurableOperationStatus::NeedsAttention
            ) | (
                DurableOperationStatus::Running,
                DurableOperationStatus::Cancelling
            ) | (
                DurableOperationStatus::Running,
                DurableOperationStatus::Succeeded
            ) | (
                DurableOperationStatus::Running,
                DurableOperationStatus::Failed
            ) | (
                DurableOperationStatus::Running,
                DurableOperationStatus::Cancelled
            ) | (
                DurableOperationStatus::Running,
                DurableOperationStatus::NeedsAttention
            ) | (
                DurableOperationStatus::Running,
                DurableOperationStatus::RolledBack
            ) | (
                DurableOperationStatus::Cancelling,
                DurableOperationStatus::Cancelled
            ) | (
                DurableOperationStatus::Cancelling,
                DurableOperationStatus::Failed
            ) | (
                DurableOperationStatus::Cancelling,
                DurableOperationStatus::NeedsAttention
            ) | (
                DurableOperationStatus::Failed,
                DurableOperationStatus::Enqueuing
            )
        )
}

fn plan_sha256(plan: &PlanOperation) -> String {
    canonical_payload_sha256(&serde_json::to_value(plan).expect("plan always serializes"))
}

fn binding_for(operation: &DurableOperation, planned: &PlannedJob) -> JobBinding {
    let source = format!(
        "{}\0{}\0{}\0{}",
        operation.operation_id, operation.generation, planned.step_id, planned.node_id
    );
    let digest = format!("{:x}", Sha256::digest(source.as_bytes()));
    JobBinding {
        step_id: planned.step_id.clone(),
        generation: operation.generation,
        job_id: format!("job-{}", &digest[..32]),
        idempotency_key: format!(
            "operation/{}/generation/{}/step/{}",
            operation.operation_id, operation.generation, planned.step_id
        ),
    }
}

fn job_projection(job: &Job) -> Value {
    json!({
        "job_id": job.job_id,
        "status": job.status,
        "attempt": job.attempt,
        "result": job.result,
        "error_message": job.error_message,
    })
}

fn job_kind_is_observation(kind: &JobKind) -> bool {
    matches!(
        kind,
        JobKind::Health | JobKind::Inventory | JobKind::ExternalHealth
    )
}
