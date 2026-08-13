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
            DurableOperationStatus::Running
                | DurableOperationStatus::Cancelling
                | DurableOperationStatus::NeedsAttention
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
        let (status, result, error_message) = self.derive_projection(&operation)?;
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
            let (status, result, error_message) = self.derive_cancelling_projection(operation)?;
            if status == DurableOperationStatus::Cancelling
                && operation.pending_step_ids.iter().any(|step_id| {
                    compensation_step_ids(operation).contains(step_id)
                        && result[step_id]["status"] == "BLOCKED"
                })
                && !self.cancellation_has_active_jobs(operation)?
            {
                return Ok((
                    DurableOperationStatus::NeedsAttention,
                    result,
                    "applicable compensation cannot be materialized from the durable dependency graph"
                        .to_string(),
                ));
            }
            return Ok((status, result, error_message));
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ClaimRequest, CompleteRequest, CompletionStatus, MemoryJobStore};
    use std::sync::{Arc, Mutex};
    use std::thread;

    fn plan(operation_id: &str, count: usize) -> PlanOperation {
        PlanOperation {
            operation_id: operation_id.to_string(),
            action: "deployment.install".to_string(),
            target_type: "deployment".to_string(),
            target_id: "deployment-a".to_string(),
            request: json!({"version": "1.0.0"}),
            jobs: (0..count)
                .map(|index| PlannedJob {
                    step_id: format!("step-{index}"),
                    node_id: format!("node-{index}"),
                    kind: JobKind::Install,
                    depends_on: vec![],
                    condition: Default::default(),
                    payload: json!({"index": index}),
                    max_attempts: 3,
                })
                .collect(),
        }
    }

    fn prepare_and_abort_plan(operation_id: &str) -> PlanOperation {
        PlanOperation {
            operation_id: operation_id.to_string(),
            action: "topology.apply".to_string(),
            target_type: "Topology".to_string(),
            target_id: "primary".to_string(),
            request: json!({"auto_enqueue": true}),
            jobs: vec![
                PlannedJob {
                    step_id: "prepare".to_string(),
                    node_id: "control-plane".to_string(),
                    kind: JobKind::TopologyApply,
                    depends_on: vec![],
                    condition: PlannedJobCondition::OnSuccess,
                    payload: json!({"phase": "PREPARE"}),
                    max_attempts: 1,
                },
                PlannedJob {
                    step_id: "apply".to_string(),
                    node_id: "node-0".to_string(),
                    kind: JobKind::Install,
                    depends_on: vec!["prepare".to_string()],
                    condition: PlannedJobCondition::OnSuccess,
                    payload: json!({"deployment_id": "deployment-a"}),
                    max_attempts: 1,
                },
                PlannedJob {
                    step_id: "finalize".to_string(),
                    node_id: "control-plane".to_string(),
                    kind: JobKind::TopologyApply,
                    depends_on: vec!["apply".to_string()],
                    condition: PlannedJobCondition::OnSuccess,
                    payload: json!({"phase": "FINALIZE"}),
                    max_attempts: 1,
                },
                PlannedJob {
                    step_id: "abort".to_string(),
                    node_id: "control-plane".to_string(),
                    kind: JobKind::TopologyApply,
                    depends_on: vec!["apply".to_string(), "finalize".to_string()],
                    condition: PlannedJobCondition::OnFailure,
                    payload: json!({"phase": "ABORT", "previous_bindings": ["old"]}),
                    max_attempts: 1,
                },
                PlannedJob {
                    step_id: "cleanup".to_string(),
                    node_id: "node-0".to_string(),
                    kind: JobKind::Uninstall,
                    depends_on: vec!["apply".to_string(), "abort".to_string()],
                    condition: PlannedJobCondition::OnSuccess,
                    payload: json!({"deployment_id": "deployment-a"}),
                    max_attempts: 1,
                },
            ],
        }
    }

    fn run_to_running(
        operations: &mut MemoryOperationStore,
        jobs: &mut MemoryJobStore,
        operation_id: &str,
        count: usize,
    ) -> DurableOperation {
        let mut coordinator = OperationCoordinator::new(operations, jobs);
        coordinator.plan(plan(operation_id, count), 0).unwrap();
        coordinator.confirm(operation_id, 1).unwrap();
        coordinator.enqueue(operation_id, 2).unwrap()
    }

    fn claim_and_complete(
        jobs: &mut MemoryJobStore,
        node_id: &str,
        token: &str,
        status: CompletionStatus,
        now_ms: i64,
    ) -> Job {
        let job = jobs
            .claim(ClaimRequest {
                node_id: node_id.to_string(),
                instance_id: format!("worker-{node_id}"),
                lease_token: token.to_string(),
                now_ms,
                lease_ms: 30_000,
            })
            .unwrap()
            .unwrap();
        jobs.complete(CompleteRequest {
            job_id: job.job_id,
            lease_token: token.to_string(),
            status,
            result: json!({"node": node_id}),
            error_message: String::new(),
            now_ms: now_ms + 1,
            events: vec![],
        })
        .unwrap()
    }

    #[derive(Debug)]
    struct RevisionConflictOperationStore {
        inner: MemoryOperationStore,
        successful_compares_before_conflict: usize,
        remaining_conflicts: usize,
        compare_attempts: usize,
    }

    impl RevisionConflictOperationStore {
        fn new(inner: MemoryOperationStore, remaining_conflicts: usize) -> Self {
            Self {
                inner,
                successful_compares_before_conflict: 0,
                remaining_conflicts,
                compare_attempts: 0,
            }
        }

        fn after_successful_compares(
            inner: MemoryOperationStore,
            successful_compares_before_conflict: usize,
            remaining_conflicts: usize,
        ) -> Self {
            Self {
                inner,
                successful_compares_before_conflict,
                remaining_conflicts,
                compare_attempts: 0,
            }
        }
    }

    impl OperationRepository for RevisionConflictOperationStore {
        fn create(
            &mut self,
            operation: DurableOperation,
        ) -> Result<DurableOperation, OperationStoreError> {
            self.inner.create(operation)
        }

        fn get(&self, operation_id: &str) -> Result<Option<DurableOperation>, OperationStoreError> {
            self.inner.get(operation_id)
        }

        fn compare_and_swap(
            &mut self,
            expected_revision: u64,
            operation: DurableOperation,
        ) -> Result<DurableOperation, OperationStoreError> {
            self.compare_attempts += 1;
            if self.successful_compares_before_conflict != 0 {
                self.successful_compares_before_conflict -= 1;
                return self.inner.compare_and_swap(expected_revision, operation);
            }
            if self.remaining_conflicts != 0 {
                self.remaining_conflicts -= 1;
                let mut concurrent = self
                    .inner
                    .get(&operation.operation_id)?
                    .ok_or_else(|| OperationStoreError::NotFound(operation.operation_id.clone()))?;
                let concurrent_expected = concurrent.revision;
                concurrent.revision += 1;
                concurrent.updated_at_ms = concurrent.updated_at_ms.saturating_add(1);
                let concurrent = self
                    .inner
                    .compare_and_swap(concurrent_expected, concurrent)?;
                return Err(OperationStoreError::RevisionConflict {
                    expected: expected_revision,
                    actual: concurrent.revision,
                });
            }
            self.inner.compare_and_swap(expected_revision, operation)
        }

        fn recoverable(&self) -> Result<Vec<DurableOperation>, OperationStoreError> {
            self.inner.recoverable()
        }

        fn list(&self) -> Result<Vec<DurableOperation>, OperationStoreError> {
            self.inner.list()
        }
    }

    #[test]
    fn plan_confirm_enqueue_is_durable_and_idempotent() {
        let mut operations = MemoryOperationStore::default();
        let mut jobs = MemoryJobStore::default();
        let mut coordinator = OperationCoordinator::new(&mut operations, &mut jobs);
        let planned = coordinator.plan(plan("op-1", 2), 0).unwrap();
        assert_eq!(planned.status, DurableOperationStatus::Planned);
        assert_eq!(coordinator.plan(plan("op-1", 2), 10).unwrap(), planned);
        coordinator.confirm("op-1", 1).unwrap();
        let running = coordinator.enqueue("op-1", 2).unwrap();
        assert_eq!(running.status, DurableOperationStatus::Running);
        assert_eq!(running.job_bindings.len(), 2);
        assert!(running.pending_step_ids.is_empty());
    }

    #[test]
    fn operation_projects_terminal_jobs_in_any_order() {
        let mut operations = MemoryOperationStore::default();
        let mut jobs = MemoryJobStore::default();
        run_to_running(&mut operations, &mut jobs, "op-order", 3);

        claim_and_complete(&mut jobs, "node-2", "t2", CompletionStatus::Succeeded, 10);
        {
            let mut coordinator = OperationCoordinator::new(&mut operations, &mut jobs);
            assert_eq!(
                coordinator.project("op-order", 12).unwrap().status,
                DurableOperationStatus::Running
            );
        }
        claim_and_complete(&mut jobs, "node-0", "t0", CompletionStatus::Succeeded, 13);
        claim_and_complete(&mut jobs, "node-1", "t1", CompletionStatus::Succeeded, 14);
        let mut coordinator = OperationCoordinator::new(&mut operations, &mut jobs);
        assert_eq!(
            coordinator.project("op-order", 20).unwrap().status,
            DurableOperationStatus::Succeeded
        );
    }

    #[test]
    fn projection_reloads_and_retries_after_a_concurrent_revision_write() {
        let mut operations = MemoryOperationStore::default();
        let mut jobs = MemoryJobStore::default();
        let running = run_to_running(&mut operations, &mut jobs, "op-project-conflict", 1);
        claim_and_complete(
            &mut jobs,
            "node-0",
            "terminal",
            CompletionStatus::Succeeded,
            10,
        );
        let initial_revision = running.revision;
        let mut operations = RevisionConflictOperationStore::new(operations, 1);

        let projected = OperationCoordinator::new(&mut operations, &mut jobs)
            .project("op-project-conflict", 20)
            .unwrap();

        assert_eq!(projected.status, DurableOperationStatus::Succeeded);
        assert_eq!(projected.revision, initial_revision + 2);
        assert_eq!(operations.compare_attempts, 2);
        assert_eq!(
            operations.get("op-project-conflict").unwrap().unwrap(),
            projected
        );
    }

    #[test]
    fn enqueue_reloads_after_a_concurrent_operation_projection() {
        let mut operations = MemoryOperationStore::default();
        let mut jobs = MemoryJobStore::default();
        {
            let mut coordinator = OperationCoordinator::new(&mut operations, &mut jobs);
            coordinator.plan(plan("op-enqueue-conflict", 1), 0).unwrap();
            coordinator.confirm("op-enqueue-conflict", 1).unwrap();
        }
        let mut operations = RevisionConflictOperationStore::new(operations, 1);

        let running = OperationCoordinator::new(&mut operations, &mut jobs)
            .enqueue("op-enqueue-conflict", 2)
            .unwrap();

        assert_eq!(running.status, DurableOperationStatus::Running);
        assert_eq!(running.job_bindings.len(), 1);
        assert!(running.pending_step_ids.is_empty());
        assert_eq!(
            jobs.list()
                .unwrap()
                .into_iter()
                .filter(|job| job.operation_id == "op-enqueue-conflict")
                .count(),
            1
        );
    }

    #[test]
    fn enqueue_replays_the_same_job_after_the_binding_save_conflicts() {
        let mut operations = MemoryOperationStore::default();
        let mut jobs = MemoryJobStore::default();
        {
            let mut coordinator = OperationCoordinator::new(&mut operations, &mut jobs);
            coordinator
                .plan(plan("op-binding-save-conflict", 1), 0)
                .unwrap();
            coordinator.confirm("op-binding-save-conflict", 1).unwrap();
        }
        // The first compare persists CONFIRMED -> ENQUEUING. The injected
        // conflict is therefore the second compare, after JobStore::enqueue
        // has already made the deterministic Job durable but before its
        // Operation binding is saved.
        let mut operations =
            RevisionConflictOperationStore::after_successful_compares(operations, 1, 1);

        let running = OperationCoordinator::new(&mut operations, &mut jobs)
            .enqueue("op-binding-save-conflict", 2)
            .unwrap();

        assert_eq!(running.status, DurableOperationStatus::Running);
        assert_eq!(running.job_bindings.len(), 1);
        assert_eq!(operations.compare_attempts, 3);
        let durable_jobs = jobs
            .list()
            .unwrap()
            .into_iter()
            .filter(|job| job.operation_id == "op-binding-save-conflict")
            .collect::<Vec<_>>();
        assert_eq!(durable_jobs.len(), 1);
        assert_eq!(durable_jobs[0].job_id, running.job_bindings[0].job_id);
    }

    #[test]
    fn projection_revision_retry_budget_is_bounded() {
        let mut operations = MemoryOperationStore::default();
        let mut jobs = MemoryJobStore::default();
        run_to_running(&mut operations, &mut jobs, "op-project-conflict-budget", 1);
        claim_and_complete(
            &mut jobs,
            "node-0",
            "terminal",
            CompletionStatus::Succeeded,
            10,
        );
        let mut operations =
            RevisionConflictOperationStore::new(operations, PROJECT_MAX_CAS_ATTEMPTS);

        assert!(matches!(
            OperationCoordinator::new(&mut operations, &mut jobs)
                .project("op-project-conflict-budget", 20),
            Err(OperationError::Store(
                OperationStoreError::RevisionConflict { .. }
            ))
        ));
        assert_eq!(operations.compare_attempts, PROJECT_MAX_CAS_ATTEMPTS);
        assert_eq!(
            operations
                .get("op-project-conflict-budget")
                .unwrap()
                .unwrap()
                .status,
            DurableOperationStatus::Running
        );
    }

    #[test]
    fn dependent_jobs_are_durable_and_failure_compensation_runs_after_known_failure() {
        let mut operations = MemoryOperationStore::default();
        let mut jobs = MemoryJobStore::default();
        let graph = PlanOperation {
            operation_id: "op-graph".to_string(),
            action: "release.install".to_string(),
            target_type: "Release".to_string(),
            target_id: "root@1.0.0".to_string(),
            request: json!({"auto_enqueue": true}),
            jobs: vec![
                PlannedJob {
                    step_id: "dependency".to_string(),
                    node_id: "node-0".to_string(),
                    kind: JobKind::Install,
                    depends_on: vec![],
                    condition: PlannedJobCondition::OnSuccess,
                    payload: json!({"name": "dependency"}),
                    max_attempts: 1,
                },
                PlannedJob {
                    step_id: "root".to_string(),
                    node_id: "node-0".to_string(),
                    kind: JobKind::Install,
                    depends_on: vec!["dependency".to_string()],
                    condition: PlannedJobCondition::OnSuccess,
                    payload: json!({"name": "root"}),
                    max_attempts: 1,
                },
                PlannedJob {
                    step_id: "compensate".to_string(),
                    node_id: "node-0".to_string(),
                    kind: JobKind::Uninstall,
                    depends_on: vec!["dependency".to_string(), "root".to_string()],
                    condition: PlannedJobCondition::OnFailure,
                    payload: json!({"container_id": "ojos-dependency"}),
                    max_attempts: 1,
                },
            ],
        };
        {
            let mut coordinator = OperationCoordinator::new(&mut operations, &mut jobs);
            coordinator.plan(graph, 0).unwrap();
            coordinator.confirm("op-graph", 1).unwrap();
            let running = coordinator.enqueue("op-graph", 2).unwrap();
            assert_eq!(running.job_bindings.len(), 1);
            assert_eq!(running.job_bindings[0].step_id, "dependency");
            assert_eq!(running.pending_step_ids, ["root", "compensate"]);
        }
        claim_and_complete(
            &mut jobs,
            "node-0",
            "dependency-token",
            CompletionStatus::Succeeded,
            10,
        );
        {
            let mut coordinator = OperationCoordinator::new(&mut operations, &mut jobs);
            let running = coordinator.project("op-graph", 12).unwrap();
            assert_eq!(running.job_bindings.len(), 2);
            assert_eq!(running.job_bindings[1].step_id, "root");
        }
        claim_and_complete(
            &mut jobs,
            "node-0",
            "root-token",
            CompletionStatus::Failed,
            20,
        );
        {
            let mut coordinator = OperationCoordinator::new(&mut operations, &mut jobs);
            let compensating = coordinator.project("op-graph", 22).unwrap();
            assert_eq!(compensating.status, DurableOperationStatus::Running);
            assert_eq!(compensating.job_bindings.len(), 3);
            assert_eq!(compensating.job_bindings[2].step_id, "compensate");
        }
        claim_and_complete(
            &mut jobs,
            "node-0",
            "compensate-token",
            CompletionStatus::Succeeded,
            30,
        );
        let mut coordinator = OperationCoordinator::new(&mut operations, &mut jobs);
        let failed = coordinator.project("op-graph", 32).unwrap();
        assert_eq!(failed.status, DurableOperationStatus::Failed);
        assert_eq!(failed.result["root"]["status"], json!(JobStatus::Failed));
        assert_eq!(
            failed.result["compensate"]["status"],
            json!(JobStatus::Succeeded)
        );
    }

    #[test]
    fn successful_branch_skips_nested_compensation_and_terminates() {
        let mut operations = MemoryOperationStore::default();
        let mut jobs = MemoryJobStore::default();
        let graph = PlanOperation {
            operation_id: "op-nested-compensation-success".to_string(),
            action: "release.install".to_string(),
            target_type: "Release".to_string(),
            target_id: "root@1.0.0".to_string(),
            request: json!({"auto_enqueue": true}),
            jobs: vec![
                PlannedJob {
                    step_id: "root".to_string(),
                    node_id: "node-0".to_string(),
                    kind: JobKind::Install,
                    depends_on: vec![],
                    condition: PlannedJobCondition::OnSuccess,
                    payload: json!({}),
                    max_attempts: 1,
                },
                PlannedJob {
                    step_id: "finalize".to_string(),
                    node_id: "control-plane".to_string(),
                    kind: JobKind::TopologyApply,
                    depends_on: vec!["root".to_string()],
                    condition: PlannedJobCondition::OnSuccess,
                    payload: json!({}),
                    max_attempts: 1,
                },
                PlannedJob {
                    step_id: "abort".to_string(),
                    node_id: "control-plane".to_string(),
                    kind: JobKind::TopologyApply,
                    depends_on: vec!["root".to_string(), "finalize".to_string()],
                    condition: PlannedJobCondition::OnFailure,
                    payload: json!({}),
                    max_attempts: 1,
                },
                PlannedJob {
                    step_id: "cleanup".to_string(),
                    node_id: "node-0".to_string(),
                    kind: JobKind::Uninstall,
                    depends_on: vec!["root".to_string(), "abort".to_string()],
                    condition: PlannedJobCondition::OnSuccess,
                    payload: json!({}),
                    max_attempts: 1,
                },
            ],
        };
        {
            let mut coordinator = OperationCoordinator::new(&mut operations, &mut jobs);
            coordinator.plan(graph, 0).unwrap();
            coordinator
                .confirm("op-nested-compensation-success", 1)
                .unwrap();
            coordinator
                .enqueue("op-nested-compensation-success", 2)
                .unwrap();
        }
        claim_and_complete(
            &mut jobs,
            "node-0",
            "root-success",
            CompletionStatus::Succeeded,
            10,
        );
        OperationCoordinator::new(&mut operations, &mut jobs)
            .project("op-nested-compensation-success", 12)
            .unwrap();
        claim_and_complete(
            &mut jobs,
            "control-plane",
            "finalize-success",
            CompletionStatus::Succeeded,
            20,
        );
        let operation = OperationCoordinator::new(&mut operations, &mut jobs)
            .project("op-nested-compensation-success", 22)
            .unwrap();
        assert_eq!(operation.status, DurableOperationStatus::Succeeded);
        assert!(operation.active_binding("abort").is_none());
        assert!(operation.active_binding("cleanup").is_none());
        assert_eq!(operation.result["abort"]["status"], "DORMANT");
        assert_eq!(operation.result["cleanup"]["status"], "SKIPPED");
    }

    #[test]
    fn failed_dependency_skips_unstarted_descendants_and_terminates_operation() {
        let mut operations = MemoryOperationStore::default();
        let mut jobs = MemoryJobStore::default();
        let graph = PlanOperation {
            operation_id: "op-dependency-failure".to_string(),
            action: "release.install".to_string(),
            target_type: "Release".to_string(),
            target_id: "root@1.0.0".to_string(),
            request: json!({"auto_enqueue": true}),
            jobs: vec![
                PlannedJob {
                    step_id: "dependency".to_string(),
                    node_id: "node-0".to_string(),
                    kind: JobKind::Install,
                    depends_on: vec![],
                    condition: PlannedJobCondition::OnSuccess,
                    payload: json!({"name": "dependency"}),
                    max_attempts: 1,
                },
                PlannedJob {
                    step_id: "middle".to_string(),
                    node_id: "node-0".to_string(),
                    kind: JobKind::Install,
                    depends_on: vec!["dependency".to_string()],
                    condition: PlannedJobCondition::OnSuccess,
                    payload: json!({"name": "middle"}),
                    max_attempts: 1,
                },
                PlannedJob {
                    step_id: "root".to_string(),
                    node_id: "node-0".to_string(),
                    kind: JobKind::Install,
                    depends_on: vec!["middle".to_string()],
                    condition: PlannedJobCondition::OnSuccess,
                    payload: json!({"name": "root"}),
                    max_attempts: 1,
                },
            ],
        };
        {
            let mut coordinator = OperationCoordinator::new(&mut operations, &mut jobs);
            coordinator.plan(graph, 0).unwrap();
            coordinator.confirm("op-dependency-failure", 1).unwrap();
            let running = coordinator.enqueue("op-dependency-failure", 2).unwrap();
            assert_eq!(running.job_bindings.len(), 1);
        }
        claim_and_complete(
            &mut jobs,
            "node-0",
            "dependency-token",
            CompletionStatus::Failed,
            10,
        );
        let mut coordinator = OperationCoordinator::new(&mut operations, &mut jobs);
        let failed = coordinator.project("op-dependency-failure", 12).unwrap();
        assert_eq!(failed.status, DurableOperationStatus::Failed);
        assert_eq!(failed.job_bindings.len(), 1);
        assert_eq!(failed.result["middle"]["status"], json!("SKIPPED"));
        assert_eq!(failed.result["root"]["status"], json!("SKIPPED"));
    }

    #[test]
    fn duplicate_terminal_callback_and_projection_do_not_regress() {
        let mut operations = MemoryOperationStore::default();
        let mut jobs = MemoryJobStore::default();
        let running = run_to_running(&mut operations, &mut jobs, "op-replay", 1);
        let binding = running.job_bindings[0].clone();
        let leased = jobs
            .claim(ClaimRequest {
                node_id: "node-0".to_string(),
                instance_id: "worker".to_string(),
                lease_token: "lease".to_string(),
                now_ms: 10,
                lease_ms: 30_000,
            })
            .unwrap()
            .unwrap();
        assert_eq!(leased.job_id, binding.job_id);
        let completion = CompleteRequest {
            job_id: binding.job_id,
            lease_token: "lease".to_string(),
            status: CompletionStatus::Succeeded,
            result: json!({"ok": true}),
            error_message: String::new(),
            now_ms: 11,
            events: vec![],
        };
        assert_eq!(
            jobs.complete(completion.clone()).unwrap(),
            jobs.complete(completion).unwrap()
        );
        let mut coordinator = OperationCoordinator::new(&mut operations, &mut jobs);
        let first = coordinator.project("op-replay", 12).unwrap();
        let replay = coordinator.project("op-replay", 13).unwrap();
        assert_eq!(first, replay);
        assert_eq!(replay.status, DurableOperationStatus::Succeeded);
    }

    #[test]
    fn restart_resumes_partially_enqueued_operation_without_second_job() {
        let mut operations = MemoryOperationStore::default();
        let mut jobs = MemoryJobStore::default();
        let binding = {
            let mut coordinator = OperationCoordinator::new(&mut operations, &mut jobs);
            coordinator.plan(plan("op-recover", 1), 0).unwrap();
            coordinator.confirm("op-recover", 1).unwrap();

            let mut operation = coordinator.required("op-recover").unwrap();
            operation.status = DurableOperationStatus::Enqueuing;
            operation = coordinator.save(operation, 2).unwrap();
            let planned = &operation.planned_jobs[0];
            let binding = binding_for(&operation, planned);
            coordinator
                .jobs
                .enqueue(
                    NewJob {
                        job_id: binding.job_id.clone(),
                        operation_id: operation.operation_id.clone(),
                        node_id: planned.node_id.clone(),
                        kind: planned.kind.clone(),
                        payload: planned.payload.clone(),
                        idempotency_key: binding.idempotency_key.clone(),
                        max_attempts: planned.max_attempts,
                    },
                    2,
                )
                .unwrap();
            binding
        };

        let mut after_restart = OperationCoordinator::new(&mut operations, &mut jobs);
        let recovered = after_restart.recover(3).unwrap();
        assert_eq!(recovered.len(), 1);
        assert_eq!(recovered[0].status, DurableOperationStatus::Running);
        assert_eq!(recovered[0].job_bindings.len(), 1);
        assert_eq!(
            after_restart
                .jobs
                .get(&binding.job_id)
                .unwrap()
                .unwrap()
                .attempt,
            0
        );
    }

    #[test]
    fn restart_enqueues_only_confirmed_automatic_workflows() {
        let mut operations = MemoryOperationStore::default();
        let mut jobs = MemoryJobStore::default();
        {
            let mut coordinator = OperationCoordinator::new(&mut operations, &mut jobs);
            let mut automatic = plan("op-auto-confirmed", 1);
            automatic.request["auto_enqueue"] = Value::Bool(true);
            coordinator.plan(automatic, 0).unwrap();
            coordinator.confirm("op-auto-confirmed", 1).unwrap();
            coordinator.plan(plan("op-manual-confirmed", 1), 0).unwrap();
            coordinator.confirm("op-manual-confirmed", 1).unwrap();
        }

        let mut after_restart = OperationCoordinator::new(&mut operations, &mut jobs);
        after_restart.recover(2).unwrap();
        assert_eq!(
            after_restart.required("op-auto-confirmed").unwrap().status,
            DurableOperationStatus::Running
        );
        assert_eq!(
            after_restart
                .required("op-manual-confirmed")
                .unwrap()
                .status,
            DurableOperationStatus::Confirmed
        );
        assert!(
            after_restart
                .required("op-manual-confirmed")
                .unwrap()
                .job_bindings
                .is_empty()
        );
    }

    #[test]
    fn cancellation_recovers_a_partially_enqueued_operation() {
        let mut operations = MemoryOperationStore::default();
        let mut jobs = MemoryJobStore::default();
        {
            let mut coordinator = OperationCoordinator::new(&mut operations, &mut jobs);
            coordinator.plan(plan("op-partial-cancel", 2), 0).unwrap();
            coordinator.confirm("op-partial-cancel", 1).unwrap();
            let mut operation = coordinator.required("op-partial-cancel").unwrap();
            operation.status = DurableOperationStatus::Enqueuing;
            operation = coordinator.save(operation, 2).unwrap();
            let planned = &operation.planned_jobs[0];
            let binding = binding_for(&operation, planned);
            coordinator
                .jobs
                .enqueue(
                    NewJob {
                        job_id: binding.job_id,
                        operation_id: operation.operation_id.clone(),
                        node_id: planned.node_id.clone(),
                        kind: planned.kind.clone(),
                        payload: planned.payload.clone(),
                        idempotency_key: binding.idempotency_key,
                        max_attempts: planned.max_attempts,
                    },
                    2,
                )
                .unwrap();
        }

        let mut after_restart = OperationCoordinator::new(&mut operations, &mut jobs);
        let cancelled = after_restart.cancel("op-partial-cancel", 3).unwrap();
        assert_eq!(cancelled.status, DurableOperationStatus::Cancelled);
        assert!(cancelled.finished_at_ms.is_some());
    }

    #[test]
    fn cancellation_after_prepare_materializes_and_preserves_abort() {
        let mut operations = MemoryOperationStore::default();
        let mut jobs = MemoryJobStore::default();
        {
            let mut coordinator = OperationCoordinator::new(&mut operations, &mut jobs);
            coordinator
                .plan(prepare_and_abort_plan("op-cancel-after-prepare"), 0)
                .unwrap();
            coordinator.confirm("op-cancel-after-prepare", 1).unwrap();
            coordinator.enqueue("op-cancel-after-prepare", 2).unwrap();
        }
        claim_and_complete(
            &mut jobs,
            "control-plane",
            "prepare-token",
            CompletionStatus::Succeeded,
            10,
        );
        {
            let mut coordinator = OperationCoordinator::new(&mut operations, &mut jobs);
            let running = coordinator.project("op-cancel-after-prepare", 12).unwrap();
            assert_eq!(running.status, DurableOperationStatus::Running);
            assert!(running.active_binding("apply").is_some());

            let cancelling = coordinator.cancel("op-cancel-after-prepare", 13).unwrap();
            assert_eq!(cancelling.status, DurableOperationStatus::Cancelling);
            assert_eq!(cancelling.result["apply"]["status"], "CANCELLED");
            assert_eq!(cancelling.result["finalize"]["status"], "CANCELLED");
            assert_eq!(cancelling.result["abort"]["status"], "QUEUED");
            assert!(cancelling.active_binding("abort").is_some());

            // Repeating cancellation must never cancel the compensator.
            let replay = coordinator.cancel("op-cancel-after-prepare", 14).unwrap();
            let abort = replay.active_binding("abort").unwrap();
            assert_eq!(
                coordinator.jobs.get(&abort.job_id).unwrap().unwrap().status,
                JobStatus::Queued
            );
        }
        claim_and_complete(
            &mut jobs,
            "control-plane",
            "abort-token",
            CompletionStatus::Succeeded,
            20,
        );
        let cancelled = OperationCoordinator::new(&mut operations, &mut jobs)
            .project("op-cancel-after-prepare", 22)
            .unwrap();
        assert_eq!(cancelled.status, DurableOperationStatus::Cancelled);
        assert_eq!(cancelled.result["abort"]["status"], "SUCCEEDED");
        assert_eq!(cancelled.result["cleanup"]["status"], "SKIPPED");
    }

    #[test]
    fn successful_abort_materializes_applicable_cleanup_before_cancel_finishes() {
        let mut operations = MemoryOperationStore::default();
        let mut jobs = MemoryJobStore::default();
        {
            let mut coordinator = OperationCoordinator::new(&mut operations, &mut jobs);
            coordinator
                .plan(prepare_and_abort_plan("op-cancel-cleanup"), 0)
                .unwrap();
            coordinator.confirm("op-cancel-cleanup", 1).unwrap();
            coordinator.enqueue("op-cancel-cleanup", 2).unwrap();
        }
        claim_and_complete(
            &mut jobs,
            "control-plane",
            "prepare-token",
            CompletionStatus::Succeeded,
            10,
        );
        OperationCoordinator::new(&mut operations, &mut jobs)
            .project("op-cancel-cleanup", 12)
            .unwrap();
        let apply = jobs
            .claim(ClaimRequest {
                node_id: "node-0".to_string(),
                instance_id: "worker-node-0".to_string(),
                lease_token: "apply-token".to_string(),
                now_ms: 13,
                lease_ms: 30_000,
            })
            .unwrap()
            .unwrap();
        OperationCoordinator::new(&mut operations, &mut jobs)
            .cancel("op-cancel-cleanup", 14)
            .unwrap();
        jobs.complete(CompleteRequest {
            job_id: apply.job_id,
            lease_token: "apply-token".to_string(),
            status: CompletionStatus::Succeeded,
            result: json!({"installed": true}),
            error_message: String::new(),
            now_ms: 15,
            events: vec![],
        })
        .unwrap();
        {
            let recovered = OperationCoordinator::new(&mut operations, &mut jobs)
                .recover(16)
                .unwrap();
            assert_eq!(recovered[0].result["abort"]["status"], "QUEUED");
        }
        claim_and_complete(
            &mut jobs,
            "control-plane",
            "abort-token",
            CompletionStatus::Succeeded,
            20,
        );
        {
            let mut coordinator = OperationCoordinator::new(&mut operations, &mut jobs);
            let cleaning = coordinator.project("op-cancel-cleanup", 22).unwrap();
            assert_eq!(cleaning.status, DurableOperationStatus::Cancelling);
            assert_eq!(cleaning.result["cleanup"]["status"], "QUEUED");
        }
        claim_and_complete(
            &mut jobs,
            "node-0",
            "cleanup-token",
            CompletionStatus::Succeeded,
            30,
        );
        let cancelled = OperationCoordinator::new(&mut operations, &mut jobs)
            .project("op-cancel-cleanup", 32)
            .unwrap();
        assert_eq!(cancelled.status, DurableOperationStatus::Cancelled);
        assert_eq!(cancelled.result["cleanup"]["status"], "SUCCEEDED");
    }

    #[test]
    fn cancellation_does_not_materialize_unrelated_forward_jobs() {
        let mut operations = MemoryOperationStore::default();
        let mut jobs = MemoryJobStore::default();
        let mut graph = prepare_and_abort_plan("op-cancel-freezes-forward");
        graph.jobs.push(PlannedJob {
            step_id: "unrelated-forward".to_string(),
            node_id: "node-1".to_string(),
            kind: JobKind::Install,
            depends_on: vec!["prepare".to_string()],
            condition: PlannedJobCondition::OnSuccess,
            payload: json!({"deployment_id": "must-not-start"}),
            max_attempts: 1,
        });
        {
            let mut coordinator = OperationCoordinator::new(&mut operations, &mut jobs);
            coordinator.plan(graph, 0).unwrap();
            coordinator.confirm("op-cancel-freezes-forward", 1).unwrap();
            coordinator.enqueue("op-cancel-freezes-forward", 2).unwrap();
            let cancelling = coordinator.cancel("op-cancel-freezes-forward", 3).unwrap();
            assert_eq!(cancelling.status, DurableOperationStatus::Cancelling);
            assert!(cancelling.active_binding("unrelated-forward").is_none());
            assert_eq!(
                cancelling.result["unrelated-forward"]["status"],
                "CANCELLED"
            );
            assert_eq!(cancelling.result["abort"]["status"], "QUEUED");
        }
        assert!(
            jobs.list()
                .unwrap()
                .iter()
                .all(|job| job.node_id != "node-1")
        );
    }

    #[test]
    fn restart_resumes_cancellation_and_materializes_abort_after_forward_settles() {
        let mut operations = MemoryOperationStore::default();
        let mut jobs = MemoryJobStore::default();
        {
            let mut coordinator = OperationCoordinator::new(&mut operations, &mut jobs);
            coordinator
                .plan(prepare_and_abort_plan("op-cancel-restart"), 0)
                .unwrap();
            coordinator.confirm("op-cancel-restart", 1).unwrap();
            coordinator.enqueue("op-cancel-restart", 2).unwrap();
        }
        claim_and_complete(
            &mut jobs,
            "control-plane",
            "prepare-token",
            CompletionStatus::Succeeded,
            10,
        );
        OperationCoordinator::new(&mut operations, &mut jobs)
            .project("op-cancel-restart", 12)
            .unwrap();
        let apply = jobs
            .claim(ClaimRequest {
                node_id: "node-0".to_string(),
                instance_id: "worker-node-0".to_string(),
                lease_token: "apply-token".to_string(),
                now_ms: 13,
                lease_ms: 30_000,
            })
            .unwrap()
            .unwrap();
        {
            let mut coordinator = OperationCoordinator::new(&mut operations, &mut jobs);
            let cancelling = coordinator.cancel("op-cancel-restart", 14).unwrap();
            assert_eq!(cancelling.status, DurableOperationStatus::Cancelling);
            assert!(cancelling.active_binding("abort").is_none());
        }
        // The worker may cross the cancellation race and prove that the
        // forward side effect completed.  Recovery must compensate it.
        jobs.complete(CompleteRequest {
            job_id: apply.job_id,
            lease_token: "apply-token".to_string(),
            status: CompletionStatus::Succeeded,
            result: json!({"installed": true}),
            error_message: String::new(),
            now_ms: 15,
            events: vec![],
        })
        .unwrap();

        let mut after_restart = OperationCoordinator::new(&mut operations, &mut jobs);
        let recovered = after_restart.recover(16).unwrap();
        assert_eq!(recovered.len(), 1);
        assert_eq!(recovered[0].status, DurableOperationStatus::Cancelling);
        assert_eq!(recovered[0].result["abort"]["status"], "QUEUED");
        assert_eq!(recovered[0].result["cleanup"]["status"], "BLOCKED");
        let abort_job_id = recovered[0].active_binding("abort").unwrap().job_id.clone();

        // A second restart is idempotent and does not cancel or duplicate ABORT.
        let replay = after_restart.recover(17).unwrap();
        assert_eq!(replay.len(), 1);
        assert_eq!(
            replay[0].active_binding("abort").unwrap().job_id,
            abort_job_id
        );
        assert_eq!(
            after_restart
                .jobs
                .get(&abort_job_id)
                .unwrap()
                .unwrap()
                .status,
            JobStatus::Queued
        );
    }

    #[test]
    fn failed_compensation_requires_attention_instead_of_reporting_cancelled() {
        let mut operations = MemoryOperationStore::default();
        let mut jobs = MemoryJobStore::default();
        {
            let mut coordinator = OperationCoordinator::new(&mut operations, &mut jobs);
            coordinator
                .plan(prepare_and_abort_plan("op-abort-failed"), 0)
                .unwrap();
            coordinator.confirm("op-abort-failed", 1).unwrap();
            coordinator.enqueue("op-abort-failed", 2).unwrap();
        }
        claim_and_complete(
            &mut jobs,
            "control-plane",
            "prepare-token",
            CompletionStatus::Succeeded,
            10,
        );
        {
            let mut coordinator = OperationCoordinator::new(&mut operations, &mut jobs);
            coordinator.project("op-abort-failed", 12).unwrap();
            let cancelling = coordinator.cancel("op-abort-failed", 13).unwrap();
            assert_eq!(cancelling.status, DurableOperationStatus::Cancelling);
        }
        claim_and_complete(
            &mut jobs,
            "control-plane",
            "abort-token",
            CompletionStatus::Failed,
            20,
        );
        let operation = OperationCoordinator::new(&mut operations, &mut jobs)
            .project("op-abort-failed", 22)
            .unwrap();
        assert_eq!(operation.status, DurableOperationStatus::NeedsAttention);
        assert!(operation.error_message.contains("compensation step abort"));
        assert!(operation.finished_at_ms.is_some());
    }

    #[test]
    fn missing_materialized_job_during_cancellation_requires_attention() {
        let mut operations = MemoryOperationStore::default();
        let mut jobs = MemoryJobStore::default();
        {
            let mut coordinator = OperationCoordinator::new(&mut operations, &mut jobs);
            coordinator.plan(plan("op-missing-job", 1), 0).unwrap();
            coordinator.confirm("op-missing-job", 1).unwrap();
            let mut operation = coordinator.required("op-missing-job").unwrap();
            operation.status = DurableOperationStatus::Enqueuing;
            operation.started_at_ms = Some(2);
            let binding = binding_for(&operation, &operation.planned_jobs[0]);
            operation.job_bindings.push(binding);
            coordinator.save(operation, 2).unwrap();
        }

        let operation = OperationCoordinator::new(&mut operations, &mut jobs)
            .cancel("op-missing-job", 3)
            .unwrap();
        assert_eq!(operation.status, DurableOperationStatus::NeedsAttention);
        assert_eq!(operation.result["step-0"]["status"], "UNKNOWN");
        assert!(operation.error_message.contains("missing durable job"));
    }

    #[test]
    fn unknown_mutating_lease_outcome_requires_attention_and_is_not_retried() {
        let mut operations = MemoryOperationStore::default();
        let mut jobs = MemoryJobStore::default();
        run_to_running(&mut operations, &mut jobs, "op-uncertain", 1);
        jobs.claim(ClaimRequest {
            node_id: "node-0".to_string(),
            instance_id: "worker".to_string(),
            lease_token: "lease".to_string(),
            now_ms: 10,
            lease_ms: 30_000,
        })
        .unwrap();

        let mut coordinator = OperationCoordinator::new(&mut operations, &mut jobs);
        coordinator.recover(40_000).unwrap();
        let operation = coordinator.required("op-uncertain").unwrap();
        assert_eq!(operation.status, DurableOperationStatus::NeedsAttention);
        let job_id = &operation.job_bindings[0].job_id;
        assert_eq!(
            coordinator.jobs.get(job_id).unwrap().unwrap().status,
            JobStatus::NeedsAttention
        );
        assert!(matches!(
            coordinator.retry("op-uncertain", 40_001),
            Err(OperationError::InvalidTransition { .. })
        ));
    }

    #[test]
    fn definitively_failed_steps_can_retry_without_rerunning_successes() {
        let mut operations = MemoryOperationStore::default();
        let mut jobs = MemoryJobStore::default();
        run_to_running(&mut operations, &mut jobs, "op-retry", 2);
        claim_and_complete(&mut jobs, "node-0", "a", CompletionStatus::Succeeded, 10);
        claim_and_complete(&mut jobs, "node-1", "b", CompletionStatus::Failed, 10);
        {
            let mut coordinator = OperationCoordinator::new(&mut operations, &mut jobs);
            assert_eq!(
                coordinator.project("op-retry", 12).unwrap().status,
                DurableOperationStatus::Failed
            );
            let retried = coordinator.retry("op-retry", 13).unwrap();
            assert_eq!(retried.status, DurableOperationStatus::Running);
            assert_eq!(retried.generation, 1);
            assert_eq!(
                retried
                    .job_bindings
                    .iter()
                    .filter(|binding| binding.step_id == "step-0")
                    .count(),
                1
            );
            assert_eq!(
                retried
                    .job_bindings
                    .iter()
                    .filter(|binding| binding.step_id == "step-1")
                    .count(),
                2
            );
        }
    }

    #[test]
    fn retry_rematerializes_descendants_skipped_by_a_failed_dependency() {
        let mut operations = MemoryOperationStore::default();
        let mut jobs = MemoryJobStore::default();
        let graph = PlanOperation {
            operation_id: "op-retry-skipped".to_string(),
            action: "topology.apply".to_string(),
            target_type: "Topology".to_string(),
            target_id: "primary".to_string(),
            request: json!({"auto_enqueue": true}),
            jobs: vec![
                PlannedJob {
                    step_id: "prepare".to_string(),
                    node_id: "control-plane".to_string(),
                    kind: JobKind::TopologyApply,
                    depends_on: vec![],
                    condition: PlannedJobCondition::OnSuccess,
                    payload: json!({}),
                    max_attempts: 1,
                },
                PlannedJob {
                    step_id: "finalize".to_string(),
                    node_id: "control-plane".to_string(),
                    kind: JobKind::TopologyApply,
                    depends_on: vec!["prepare".to_string()],
                    condition: PlannedJobCondition::OnSuccess,
                    payload: json!({}),
                    max_attempts: 1,
                },
            ],
        };
        {
            let mut coordinator = OperationCoordinator::new(&mut operations, &mut jobs);
            coordinator.plan(graph, 0).unwrap();
            coordinator.confirm("op-retry-skipped", 1).unwrap();
            coordinator.enqueue("op-retry-skipped", 2).unwrap();
        }
        claim_and_complete(
            &mut jobs,
            "control-plane",
            "prepare-failed",
            CompletionStatus::Failed,
            10,
        );
        {
            let mut coordinator = OperationCoordinator::new(&mut operations, &mut jobs);
            let failed = coordinator.project("op-retry-skipped", 12).unwrap();
            assert_eq!(failed.status, DurableOperationStatus::Failed);
            assert!(failed.active_binding("finalize").is_none());
            let retry = coordinator.retry("op-retry-skipped", 13).unwrap();
            assert_eq!(retry.status, DurableOperationStatus::Running);
            assert_eq!(
                retry
                    .job_bindings
                    .iter()
                    .filter(|binding| binding.step_id == "prepare")
                    .count(),
                2
            );
            assert!(retry.active_binding("finalize").is_none());
        }
        claim_and_complete(
            &mut jobs,
            "control-plane",
            "prepare-retry",
            CompletionStatus::Succeeded,
            20,
        );
        let running = OperationCoordinator::new(&mut operations, &mut jobs)
            .project("op-retry-skipped", 22)
            .unwrap();
        assert_eq!(running.status, DurableOperationStatus::Running);
        assert!(running.active_binding("finalize").is_some());
    }

    #[test]
    fn rollback_is_a_new_auditable_operation() {
        let mut operations = MemoryOperationStore::default();
        let mut jobs = MemoryJobStore::default();
        run_to_running(&mut operations, &mut jobs, "op-source", 1);
        claim_and_complete(
            &mut jobs,
            "node-0",
            "source",
            CompletionStatus::Succeeded,
            10,
        );
        let mut coordinator = OperationCoordinator::new(&mut operations, &mut jobs);
        coordinator.project("op-source", 12).unwrap();
        let rollback = coordinator
            .rollback("op-source", plan("op-rollback", 1), 13)
            .unwrap();
        assert_eq!(rollback.mode, DurableOperationMode::Rollback);
        assert_eq!(
            rollback.rollback_of_operation_id.as_deref(),
            Some("op-source")
        );
        assert_eq!(rollback.status, DurableOperationStatus::Planned);
    }

    #[test]
    fn thirty_two_concurrent_claims_produce_one_lease() {
        let jobs = Arc::new(Mutex::new(MemoryJobStore::default()));
        jobs.lock()
            .unwrap()
            .enqueue(
                NewJob {
                    job_id: "job-concurrent".to_string(),
                    operation_id: "op-concurrent".to_string(),
                    node_id: "node-a".to_string(),
                    kind: JobKind::Install,
                    payload: json!({}),
                    idempotency_key: "concurrent".to_string(),
                    max_attempts: 3,
                },
                0,
            )
            .unwrap();
        let handles = (0..32)
            .map(|index| {
                let jobs = Arc::clone(&jobs);
                thread::spawn(move || {
                    jobs.lock()
                        .unwrap()
                        .claim(ClaimRequest {
                            node_id: "node-a".to_string(),
                            instance_id: format!("worker-{index}"),
                            lease_token: format!("lease-{index}"),
                            now_ms: 1,
                            lease_ms: 30_000,
                        })
                        .unwrap()
                        .is_some()
                })
            })
            .collect::<Vec<_>>();
        let winners = handles
            .into_iter()
            .map(|handle| usize::from(handle.join().unwrap()))
            .sum::<usize>();
        assert_eq!(winners, 1);
    }
}
