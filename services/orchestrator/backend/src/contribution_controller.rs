//! Deployment-scoped Contribution activation orchestration.
//!
//! This module deliberately owns no HTTP route and does not mutate permission
//! assignments. Store/Composition code performs a read-only preflight and
//! durably enqueues the returned DAG fragment. The control-plane worker stages
//! the full immutable revision during PREPARE, so a failed Operation enqueue
//! cannot leave orphaned contribution state.

use crate::contribution_snapshot::active_contribution_snapshot;
use crate::durable::DurableStore;
use orchestrator_control_plane::{
    Job, JobKind, JobStatus, JobStore, OperationRepository, PlannedJob, PlannedJobCondition,
    canonical_payload_sha256,
};
use orchestrator_core::{
    ContributionActivationStateV1, ContributionActivationV1, ContributionApiSurfaceV1,
    ContributionFrontendModuleV1, ContributionHeadV1, ContributionOperationRouteV1,
    ContributionPermissionDefinitionV1, ContributionRevisionStatusV1, ContributionRevisionV1,
    ContributionTerminationIntentV1, ProjectionReceiptStateV1, ProjectionReceiptV1,
    ProjectionTargetV1, stage_route_collisions,
};
use orchestrator_protocol::{RuntimeDesiredState, RuntimeObservedState};
use orchestrator_storage::{ContributionRepository, ContributionRepositoryError};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use thiserror::Error;

pub(crate) const CONTRIBUTION_JOB_CONTROLLER: &str = "ojos.dev/contribution-job/v1";
pub(crate) const CONTROL_PLANE_NODE_ID: &str = "control-plane";

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub(crate) enum ContributionJobPhase {
    Prepare,
    Commit,
    AckGate,
    Abort,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct ContributionJobPayloadV1 {
    pub controller: String,
    pub phase: ContributionJobPhase,
    pub activation_id: String,
    pub revision: ContributionRevisionV1,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_head_etag: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub restore_runtime_gate_step_id: Option<String>,
}

impl ContributionJobPayloadV1 {
    fn new(phase: ContributionJobPhase, staged: &StagedContributionV1) -> Self {
        Self {
            controller: CONTRIBUTION_JOB_CONTROLLER.to_string(),
            phase,
            activation_id: staged.activation_id.clone(),
            revision: staged.revision.clone(),
            expected_head_etag: staged.expected_head_etag.clone(),
            restore_runtime_gate_step_id: None,
        }
    }

    fn validate(&self) -> Result<(), ContributionControllerError> {
        if self.controller != CONTRIBUTION_JOB_CONTROLLER {
            return Err(invalid("unsupported contribution controller schema"));
        }
        if self.activation_id.trim().is_empty() {
            return Err(invalid("activation_id must not be empty"));
        }
        self.revision
            .validate()
            .map_err(|error| invalid(error.to_string()))?;
        if self.revision.status() != ContributionRevisionStatusV1::Staged {
            return Err(invalid("job payload revision must be STAGED"));
        }
        if self
            .restore_runtime_gate_step_id
            .as_deref()
            .is_some_and(|step| step.trim().is_empty() || step.len() > 128)
        {
            return Err(invalid("restore runtime gate step id is invalid"));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct StagedContributionV1 {
    pub activation_id: String,
    pub revision: ContributionRevisionV1,
    pub expected_head_etag: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ContributionJobStepsV1 {
    pub prepare_step_id: String,
    pub commit_step_id: String,
    pub ack_gate_step_id: String,
    pub abort_step_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(dead_code)] // Store replacement wiring consumes this in the next integration batch.
pub(crate) struct SignedContributionSuccessorV1 {
    pub scope_id: String,
    pub replaces_deployment_id: String,
    pub deployment_id: String,
    pub service_id: String,
    pub release_digest: String,
    pub contract_digest: String,
    pub api_surfaces: Vec<ContributionApiSurfaceV1>,
    pub operation_routes: Vec<ContributionOperationRouteV1>,
    pub permission_definitions: Vec<ContributionPermissionDefinitionV1>,
    pub user_frontend_modules: Vec<ContributionFrontendModuleV1>,
    pub admin_frontend_modules: Vec<ContributionFrontendModuleV1>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(dead_code)] // Store replacement wiring consumes this in the next integration batch.
pub(crate) struct ContributionReplacementDagV1 {
    /// Durable gates that must finish before Contribution PREPARE.
    pub prepare_depends_on: Vec<String>,
    /// Replacement runtime Job. It is made dependent on Contribution PREPARE.
    pub runtime_step_id: String,
    /// Runtime/health/context gates required by Contribution COMMIT.
    pub commit_depends_on: Vec<String>,
    /// Topology FINALIZE nodes. They are made dependent on Contribution COMMIT.
    pub topology_finalize_step_ids: Vec<String>,
    /// Topology ABORT nodes. They directly observe Contribution PREPARE/COMMIT
    /// failure so an already-staged topology cannot be stranded.
    pub topology_abort_step_ids: Vec<String>,
    /// Old-runtime cleanup after a successful cutover.
    pub success_cleanup_step_ids: Vec<String>,
    /// Candidate-runtime cleanup after compensation.
    pub failure_cleanup_step_ids: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(dead_code)] // Store uninstall wiring consumes this in the next integration batch.
pub(crate) struct ContributionUninstallDagV1 {
    pub prepare_depends_on: Vec<String>,
    /// Optional pre-uninstall health/context gates. COMMIT always re-reads the
    /// runtime facts and the runtime Uninstall is made dependent on COMMIT.
    pub commit_depends_on: Vec<String>,
    pub runtime_uninstall_step_id: String,
    /// ON_FAILURE Health step depending on the Uninstall. It refreshes durable
    /// runtime evidence before ABORT is allowed to republish the old revision.
    pub restore_health_step_id: String,
}

/// Returns the stable Operation step identities for a staged Contribution.
///
/// Store planners may call this before appending the fragment so runtime and
/// topology jobs can reference the controller-owned identities without
/// duplicating the suffix/hash algorithm.
pub(crate) fn contribution_job_steps(staged: &StagedContributionV1) -> ContributionJobStepsV1 {
    let suffix = stable_step_suffix(&staged.activation_id);
    ContributionJobStepsV1 {
        prepare_step_id: format!("contribution-prepare-{suffix}"),
        commit_step_id: format!("contribution-commit-{suffix}"),
        ack_gate_step_id: format!("contribution-ack-gate-{suffix}"),
        abort_step_id: format!("contribution-abort-{suffix}"),
    }
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ContributionJobOutcomeV1 {
    pub result: Value,
}

#[derive(Debug, Error)]
pub(crate) enum ContributionControllerError {
    #[error("invalid contribution controller request: {0}")]
    Invalid(String),
    #[error("contribution controller conflict: {0}")]
    Conflict(String),
    #[error("contribution controller record not found: {0}")]
    NotFound(String),
    #[error("contribution controller needs attention: {0}")]
    NeedsAttention(String),
    #[error("contribution projection acknowledgement is pending: {0}")]
    Retryable(String),
    #[error("contribution restore acknowledgement is pending: {0}")]
    RetryableCompensation(String),
    #[error("contribution persistence failed: {0}")]
    Persistence(String),
}

impl ContributionControllerError {
    pub(crate) fn code(&self) -> &'static str {
        match self {
            Self::Invalid(_) => "INVALID_CONTRIBUTION_JOB_PAYLOAD",
            Self::Conflict(_) => "CONTRIBUTION_CONFLICT",
            Self::NotFound(_) => "CONTRIBUTION_NOT_FOUND",
            Self::NeedsAttention(_) => "CONTRIBUTION_NEEDS_ATTENTION",
            Self::Retryable(_) => "CONTRIBUTION_ACK_PENDING",
            Self::RetryableCompensation(_) => "CONTRIBUTION_RESTORE_ACK_PENDING",
            Self::Persistence(_) => "CONTRIBUTION_PERSISTENCE_FAILED",
        }
    }

    pub(crate) fn needs_attention(&self) -> bool {
        matches!(self, Self::NeedsAttention(_))
    }

    pub(crate) fn retryable(&self) -> bool {
        matches!(self, Self::Retryable(_) | Self::RetryableCompensation(_))
    }

    pub(crate) fn retry_exhaustion_needs_attention(&self) -> bool {
        matches!(self, Self::RetryableCompensation(_))
    }
}

impl From<ContributionRepositoryError> for ContributionControllerError {
    fn from(value: ContributionRepositoryError) -> Self {
        match value {
            ContributionRepositoryError::Invalid(message) => Self::Invalid(message),
            ContributionRepositoryError::Conflict(message) => Self::Conflict(message),
            ContributionRepositoryError::NotFound(message) => Self::NotFound(message),
            ContributionRepositoryError::Persistence(message) => Self::Persistence(message),
        }
    }
}

/// Read-only preflight used while building an Operation plan. The authoritative
/// lineage and same-scope route-collision check is repeated inside the atomic
/// PREPARE transaction after the Job has been durably enqueued.
///
/// Despite the compatibility name, this function never writes repository
/// state. Keeping the established name lets Store wiring migrate without an
/// intermediate broken build.
pub(crate) fn stage_contribution(
    repository: &impl ContributionRepository,
    activation_id: &str,
    revision: &ContributionRevisionV1,
) -> Result<StagedContributionV1, ContributionControllerError> {
    revision
        .validate()
        .map_err(|error| invalid(error.to_string()))?;
    if revision.status() != ContributionRevisionStatusV1::Staged {
        return Err(invalid("stage requires a STAGED contribution revision"));
    }
    let live = repository.contribution_revisions(revision.scope_id(), None)?;
    let head = repository.contribution_head(revision.scope_id(), revision.service_id())?;
    validate_stage_lineage(head.as_ref(), revision, &live)?;
    let collisions =
        stage_route_collisions(revision, &live).map_err(|error| invalid(error.to_string()))?;
    if !collisions.is_empty() {
        return Err(ContributionControllerError::Conflict(format!(
            "{} live route collision(s) in scope {}",
            collisions.len(),
            revision.scope_id()
        )));
    }

    let expected_head_etag = head.as_ref().map(|head| head.etag().to_string());
    ContributionActivationV1::prepare(activation_id, revision, expected_head_etag.clone())
        .map_err(|error| invalid(error.to_string()))?;

    Ok(StagedContributionV1 {
        activation_id: activation_id.to_string(),
        revision: revision.clone(),
        expected_head_etag,
    })
}

/// Compiles a signed replacement Contribution against the current head.
/// Generation and lineage are controller-owned so Store callers cannot
/// accidentally create a sibling from stale or unrelated deployment state.
#[allow(dead_code)] // Public inside the crate for Store replacement wiring.
pub(crate) fn stage_signed_contribution_successor(
    repository: &impl ContributionRepository,
    activation_id: &str,
    signed: SignedContributionSuccessorV1,
) -> Result<StagedContributionV1, ContributionControllerError> {
    let head = repository
        .contribution_head(&signed.scope_id, &signed.service_id)?
        .ok_or_else(|| {
            ContributionControllerError::Conflict(
                "replacement contribution requires an existing ACTIVE head".to_string(),
            )
        })?;
    let previous = repository
        .contribution_revision(head.active_revision_id())?
        .ok_or_else(|| {
            ContributionControllerError::NeedsAttention(format!(
                "contribution head {} references a missing revision",
                head.etag()
            ))
        })?;
    if previous.scope_id() != signed.scope_id
        || previous.service_id() != signed.service_id
        || previous.status() != ContributionRevisionStatusV1::Active
    {
        return Err(ContributionControllerError::NeedsAttention(
            "replacement contribution head does not reference its exact ACTIVE revision"
                .to_string(),
        ));
    }
    if previous.deployment_id() != signed.replaces_deployment_id {
        return Err(ContributionControllerError::Conflict(
            "replacement source deployment does not own the ACTIVE contribution head".to_string(),
        ));
    }
    if signed.replaces_deployment_id == signed.deployment_id {
        return Err(ContributionControllerError::Conflict(
            "replacement contribution must bind a new deployment identity".to_string(),
        ));
    }
    let generation = next_contribution_generation(
        repository,
        &signed.scope_id,
        &signed.service_id,
        head.generation(),
    )?;
    let successor = ContributionRevisionV1::stage(
        signed.scope_id,
        signed.deployment_id,
        signed.service_id,
        signed.release_digest,
        signed.contract_digest,
        generation,
        Some(previous.revision_id().to_string()),
        signed.api_surfaces,
        signed.operation_routes,
        signed.permission_definitions,
        signed.user_frontend_modules,
        signed.admin_frontend_modules,
    )
    .map_err(|error| invalid(error.to_string()))?;
    stage_contribution(repository, activation_id, &successor)
}

/// Builds and preflights an empty successor for a deployment uninstall.
///
/// The successor deliberately keeps the Contribution head and advances its
/// generation instead of physically deleting it. That preserves lineage/ETag
/// monotonicity across a later reinstall while publishing no API, route,
/// permission definition, or frontend module. Permission assignments are a
/// separate aggregate and are never read or mutated here.
#[allow(dead_code)] // Public inside the crate for Store uninstall wiring.
pub(crate) fn stage_contribution_uninstall(
    repository: &impl ContributionRepository,
    activation_id: &str,
    scope_id: &str,
    deployment_id: &str,
    service_id: &str,
) -> Result<Option<StagedContributionV1>, ContributionControllerError> {
    let Some(head) = repository.contribution_head(scope_id, service_id)? else {
        return Ok(None);
    };
    let active = repository
        .contribution_revision(head.active_revision_id())?
        .ok_or_else(|| {
            ContributionControllerError::NeedsAttention(format!(
                "contribution head {} references a missing revision",
                head.etag()
            ))
        })?;
    if active.scope_id() != scope_id
        || active.service_id() != service_id
        || active.deployment_id() != deployment_id
        || active.status() != ContributionRevisionStatusV1::Active
    {
        return Err(ContributionControllerError::Conflict(
            "uninstall target is not the deployment owning the ACTIVE contribution head"
                .to_string(),
        ));
    }
    let generation =
        next_contribution_generation(repository, scope_id, service_id, head.generation())?;
    let successor = ContributionRevisionV1::stage(
        scope_id,
        deployment_id,
        service_id,
        active.release_digest(),
        active.contract_digest(),
        generation,
        Some(active.revision_id().to_string()),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        Vec::new(),
    )
    .map_err(|error| invalid(error.to_string()))?;
    stage_contribution(repository, activation_id, &successor).map(Some)
}

/// Appends the four contribution controller nodes without knowing Store's
/// runtime step names. Callers must add `prepare_step_id` as a dependency of
/// the first resource/runtime step, and pass the final runtime/health gate as
/// `commit_depends_on`. `abort_depends_on` should contain the topology FINALIZE
/// gate so a post-COMMIT topology failure restores the prior Contribution.
pub(crate) fn append_contribution_job_fragment(
    jobs: &mut Vec<PlannedJob>,
    staged: &StagedContributionV1,
    prepare_depends_on: Vec<String>,
    commit_depends_on: Vec<String>,
    abort_depends_on: Vec<String>,
) -> ContributionJobStepsV1 {
    let steps = contribution_job_steps(staged);
    jobs.push(PlannedJob {
        step_id: steps.prepare_step_id.clone(),
        node_id: CONTROL_PLANE_NODE_ID.to_string(),
        kind: JobKind::ContributionProjection,
        depends_on: prepare_depends_on,
        condition: PlannedJobCondition::OnSuccess,
        payload: serde_json::to_value(ContributionJobPayloadV1::new(
            ContributionJobPhase::Prepare,
            staged,
        ))
        .expect("typed contribution job payload is serializable"),
        max_attempts: 1,
    });

    let mut commit_dependencies = commit_depends_on;
    if !commit_dependencies.contains(&steps.prepare_step_id) {
        commit_dependencies.push(steps.prepare_step_id.clone());
    }
    jobs.push(PlannedJob {
        step_id: steps.commit_step_id.clone(),
        node_id: CONTROL_PLANE_NODE_ID.to_string(),
        kind: JobKind::ContributionProjection,
        depends_on: commit_dependencies.clone(),
        condition: PlannedJobCondition::OnSuccess,
        payload: serde_json::to_value(ContributionJobPayloadV1::new(
            ContributionJobPhase::Commit,
            staged,
        ))
        .expect("typed contribution job payload is serializable"),
        max_attempts: 1,
    });
    jobs.push(PlannedJob {
        step_id: steps.ack_gate_step_id.clone(),
        node_id: CONTROL_PLANE_NODE_ID.to_string(),
        kind: JobKind::ContributionProjection,
        depends_on: vec![steps.commit_step_id.clone()],
        condition: PlannedJobCondition::OnSuccess,
        payload: serde_json::to_value(ContributionJobPayloadV1::new(
            ContributionJobPhase::AckGate,
            staged,
        ))
        .expect("typed contribution job payload is serializable"),
        // 1s + 5s + repeated 30s backoff gives both consumers time to poll,
        // while preserving a bounded failure that materializes ABORT.
        max_attempts: 8,
    });
    commit_dependencies.push(steps.ack_gate_step_id.clone());
    for dependency in abort_depends_on {
        if !commit_dependencies.contains(&dependency) {
            commit_dependencies.push(dependency);
        }
    }
    jobs.push(PlannedJob {
        step_id: steps.abort_step_id.clone(),
        node_id: CONTROL_PLANE_NODE_ID.to_string(),
        kind: JobKind::ContributionProjection,
        depends_on: commit_dependencies,
        condition: PlannedJobCondition::OnFailure,
        payload: serde_json::to_value(ContributionJobPayloadV1::new(
            ContributionJobPhase::Abort,
            staged,
        ))
        .expect("typed contribution job payload is serializable"),
        // ABORT re-publishes the last-known-good snapshot, then waits for the
        // same authoritative consumers before candidate cleanup may run.
        max_attempts: 8,
    });
    steps
}

/// Wires a replacement cutover without allowing Store to reorder Contribution
/// activation around runtime health or topology finalization.
#[allow(dead_code)] // Public inside the crate for Store replacement wiring.
pub(crate) fn append_contribution_replacement_job_fragment(
    jobs: &mut Vec<PlannedJob>,
    staged: &StagedContributionV1,
    dag: ContributionReplacementDagV1,
) -> Result<ContributionJobStepsV1, ContributionControllerError> {
    require_job(jobs, &dag.runtime_step_id, PlannedJobCondition::OnSuccess)?;
    for step_id in dag
        .prepare_depends_on
        .iter()
        .chain(dag.commit_depends_on.iter())
        .chain(dag.topology_finalize_step_ids.iter())
        .chain(dag.topology_abort_step_ids.iter())
        .chain(dag.success_cleanup_step_ids.iter())
        .chain(dag.failure_cleanup_step_ids.iter())
    {
        require_job_exists(jobs, step_id)?;
    }
    for step_id in &dag.topology_finalize_step_ids {
        require_job(jobs, step_id, PlannedJobCondition::OnSuccess)?;
    }
    for step_id in &dag.topology_abort_step_ids {
        require_job(jobs, step_id, PlannedJobCondition::OnFailure)?;
    }
    for step_id in dag
        .success_cleanup_step_ids
        .iter()
        .chain(dag.failure_cleanup_step_ids.iter())
    {
        require_job(jobs, step_id, PlannedJobCondition::OnSuccess)?;
    }

    let mut commit_depends_on = dag.commit_depends_on;
    push_unique(&mut commit_depends_on, &dag.runtime_step_id);
    if dag
        .topology_finalize_step_ids
        .iter()
        .any(|step| commit_depends_on.contains(step))
    {
        return Err(invalid(
            "Contribution COMMIT cannot depend on a topology FINALIZE that it gates",
        ));
    }
    let steps = append_contribution_job_fragment(
        jobs,
        staged,
        dag.prepare_depends_on,
        commit_depends_on,
        dag.topology_finalize_step_ids.clone(),
    );

    add_planned_dependency(jobs, &dag.runtime_step_id, &steps.prepare_step_id)?;
    for step_id in &dag.topology_finalize_step_ids {
        add_planned_dependency(jobs, step_id, &steps.commit_step_id)?;
    }
    for step_id in &dag.topology_abort_step_ids {
        add_planned_dependency(jobs, step_id, &steps.prepare_step_id)?;
        add_planned_dependency(jobs, step_id, &steps.commit_step_id)?;
    }
    for step_id in &dag.success_cleanup_step_ids {
        add_planned_dependency(jobs, step_id, &steps.ack_gate_step_id)?;
    }
    for step_id in &dag.failure_cleanup_step_ids {
        add_planned_dependency(jobs, step_id, &steps.abort_step_id)?;
    }
    Ok(steps)
}

/// Wires logical Contribution removal before physical runtime removal.
///
/// The empty successor commits while the old runtime is still healthy. The
/// Uninstall then runs. On Uninstall failure/cancellation, a fresh Health
/// observation completes first and Contribution ABORT restores the old head;
/// a failed health check leaves the activation in NEEDS_ATTENTION instead of
/// publishing routes to an unproven runtime.
#[allow(dead_code)] // Public inside the crate for Store uninstall wiring.
pub(crate) fn append_contribution_uninstall_job_fragment(
    jobs: &mut Vec<PlannedJob>,
    staged: &StagedContributionV1,
    dag: ContributionUninstallDagV1,
) -> Result<ContributionJobStepsV1, ContributionControllerError> {
    require_job(
        jobs,
        &dag.runtime_uninstall_step_id,
        PlannedJobCondition::OnSuccess,
    )?;
    let restore_health = jobs
        .iter()
        .find(|job| job.step_id == dag.restore_health_step_id)
        .ok_or_else(|| invalid(format!("unknown Job step {}", dag.restore_health_step_id)))?;
    if restore_health.kind != JobKind::Health
        || restore_health.condition != PlannedJobCondition::OnFailure
        || !restore_health
            .depends_on
            .contains(&dag.runtime_uninstall_step_id)
    {
        return Err(invalid(
            "uninstall restore health must be an ON_FAILURE Health Job depending on runtime Uninstall",
        ));
    }
    for step_id in dag
        .prepare_depends_on
        .iter()
        .chain(dag.commit_depends_on.iter())
    {
        require_job_exists(jobs, step_id)?;
    }
    if dag
        .commit_depends_on
        .contains(&dag.runtime_uninstall_step_id)
    {
        return Err(invalid(
            "Contribution uninstall COMMIT must precede runtime Uninstall",
        ));
    }
    let steps = append_contribution_job_fragment(
        jobs,
        staged,
        dag.prepare_depends_on,
        dag.commit_depends_on,
        vec![
            dag.runtime_uninstall_step_id.clone(),
            dag.restore_health_step_id.clone(),
        ],
    );
    let abort = jobs
        .iter_mut()
        .find(|job| job.step_id == steps.abort_step_id)
        .expect("appended Contribution ABORT exists");
    abort.payload["restore_runtime_gate_step_id"] = Value::String(dag.restore_health_step_id);
    add_planned_dependency(
        jobs,
        &dag.runtime_uninstall_step_id,
        &steps.ack_gate_step_id,
    )?;
    Ok(steps)
}

fn require_job_exists(
    jobs: &[PlannedJob],
    step_id: &str,
) -> Result<(), ContributionControllerError> {
    jobs.iter()
        .any(|job| job.step_id == step_id)
        .then_some(())
        .ok_or_else(|| invalid(format!("unknown Job step {step_id}")))
}

fn require_job(
    jobs: &[PlannedJob],
    step_id: &str,
    condition: PlannedJobCondition,
) -> Result<(), ContributionControllerError> {
    let job = jobs
        .iter()
        .find(|job| job.step_id == step_id)
        .ok_or_else(|| invalid(format!("unknown Job step {step_id}")))?;
    if job.condition != condition {
        return Err(invalid(format!(
            "Job step {step_id} must use {condition:?}"
        )));
    }
    Ok(())
}

fn add_planned_dependency(
    jobs: &mut [PlannedJob],
    step_id: &str,
    dependency: &str,
) -> Result<(), ContributionControllerError> {
    let job = jobs
        .iter_mut()
        .find(|job| job.step_id == step_id)
        .ok_or_else(|| invalid(format!("unknown Job step {step_id}")))?;
    push_unique(&mut job.depends_on, dependency);
    Ok(())
}

fn push_unique(values: &mut Vec<String>, value: &str) {
    if !values.iter().any(|existing| existing == value) {
        values.push(value.to_string());
    }
}

pub(crate) fn is_contribution_job(job: &Job) -> bool {
    job.kind == JobKind::ContributionProjection
        && job.payload.get("controller").and_then(Value::as_str)
            == Some(CONTRIBUTION_JOB_CONTROLLER)
}

pub(crate) fn execute_contribution_job(
    storage: &DurableStore,
    payload: &Value,
    operation_id: &str,
    mut checkpoint: impl FnMut() -> Result<(), String>,
) -> Result<ContributionJobOutcomeV1, ContributionControllerError> {
    let payload: ContributionJobPayloadV1 = serde_json::from_value(payload.clone())
        .map_err(|error| invalid(format!("decode contribution job: {error}")))?;
    payload.validate()?;
    if payload.activation_id != operation_id {
        return Err(invalid(
            "activation_id must equal the durable Operation owning this Job",
        ));
    }
    checkpoint().map_err(ContributionControllerError::NeedsAttention)?;
    let outcome = match payload.phase {
        ContributionJobPhase::Prepare => execute_prepare(storage, &payload, &mut checkpoint),
        ContributionJobPhase::Commit
        | ContributionJobPhase::AckGate
        | ContributionJobPhase::Abort => {
            let activation = storage.contribution_activation(&payload.activation_id)?;
            if activation.is_none() && payload.phase == ContributionJobPhase::Abort {
                return execute_unstaged_abort(storage, &payload).map(|head| {
                    ContributionJobOutcomeV1 {
                        result: json!({
                            "controller": CONTRIBUTION_JOB_CONTROLLER,
                            "phase": "ABORT",
                            "activation_id": payload.activation_id,
                            "candidate_revision_id": payload.revision.revision_id(),
                            "head_etag": head.as_ref().map(ContributionHeadV1::etag),
                            "not_staged": true,
                        }),
                    }
                });
            }
            let activation = activation.ok_or_else(|| {
                ContributionControllerError::NotFound(format!(
                    "activation {}",
                    payload.activation_id
                ))
            })?;
            ensure_payload_identity(&payload, &activation)?;
            let candidate = required_candidate(storage, &payload, &activation)?;
            let receipts = required_receipts(storage, &payload)?;
            match payload.phase {
                ContributionJobPhase::Commit => execute_commit(
                    storage,
                    &payload,
                    &activation,
                    &candidate,
                    receipts,
                    &mut checkpoint,
                ),
                ContributionJobPhase::AckGate => {
                    execute_ack_gate(storage, &payload, &activation, &candidate, receipts)
                }
                ContributionJobPhase::Abort => execute_abort(
                    storage,
                    &payload,
                    &activation,
                    &candidate,
                    receipts,
                    &mut checkpoint,
                ),
                ContributionJobPhase::Prepare => unreachable!("matched above"),
            }
        }
    }?;
    checkpoint().map_err(ContributionControllerError::NeedsAttention)?;
    Ok(ContributionJobOutcomeV1 {
        result: json!({
            "controller": CONTRIBUTION_JOB_CONTROLLER,
            "phase": format!("{:?}", payload.phase).to_ascii_uppercase(),
            "activation_id": payload.activation_id,
            "candidate_revision_id": payload.revision.revision_id(),
            "head_etag": outcome.as_ref().map(ContributionHeadV1::etag),
        }),
    })
}

fn execute_prepare(
    storage: &DurableStore,
    payload: &ContributionJobPayloadV1,
    checkpoint: &mut impl FnMut() -> Result<(), String>,
) -> Result<Option<ContributionHeadV1>, ContributionControllerError> {
    let candidate = &payload.revision;
    let activation = ContributionActivationV1::prepare(
        &payload.activation_id,
        candidate,
        payload.expected_head_etag.clone(),
    )
    .map_err(|error| invalid(error.to_string()))?;
    let pending_receipts = ProjectionTargetV1::ALL
        .into_iter()
        .map(|target| ProjectionReceiptV1::pending(&payload.activation_id, target, candidate))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| invalid(error.to_string()))?;

    // This is the first durable Contribution mutation. Repository
    // implementations serialize the scope and re-check head/route ownership
    // inside the same transaction that inserts all three aggregates. A replay
    // after receipts progressed must never attempt to overwrite them with the
    // original PENDING values.
    if storage
        .contribution_revision(candidate.revision_id())?
        .is_none()
    {
        storage.stage_contribution_bundle(candidate, &activation, &pending_receipts)?;
    }
    let activation = required_activation(storage, payload)?;
    let receipts = required_receipts(storage, payload)?;
    if receipts
        .iter()
        .all(|receipt| receipt.state() == ProjectionReceiptStateV1::Staged)
    {
        return Ok(None);
    }
    if activation.state() != ContributionActivationStateV1::Preparing {
        if receipts
            .iter()
            .all(|receipt| receipt.state() != ProjectionReceiptStateV1::Pending)
        {
            return Ok(None);
        }
        return Err(ContributionControllerError::Conflict(
            "PREPARE replay disagrees with persisted activation state".to_string(),
        ));
    }
    let mut staged = Vec::with_capacity(receipts.len());
    for receipt in receipts {
        checkpoint().map_err(ContributionControllerError::NeedsAttention)?;
        staged.push(
            receipt
                .record(
                    ProjectionReceiptStateV1::Staged,
                    None,
                    Some(target_projection_digest(candidate, receipt.target())),
                    None,
                    None,
                )
                .map_err(|error| invalid(error.to_string()))?,
        );
    }
    storage.put_contribution_activation_bundle(&activation, &staged)?;
    ensure_payload_identity(payload, &activation)?;
    Ok(None)
}

fn execute_unstaged_abort(
    storage: &DurableStore,
    payload: &ContributionJobPayloadV1,
) -> Result<Option<ContributionHeadV1>, ContributionControllerError> {
    if storage
        .contribution_revision(payload.revision.revision_id())?
        .is_some()
    {
        return Err(ContributionControllerError::NeedsAttention(
            "candidate revision exists without its atomic activation bundle".to_string(),
        ));
    }
    // Absence of both unique identities proves PREPARE never committed. No
    // contribution content needs restoring, regardless of later head changes.
    storage
        .contribution_head(payload.revision.scope_id(), payload.revision.service_id())
        .map_err(Into::into)
}

fn execute_commit(
    storage: &DurableStore,
    payload: &ContributionJobPayloadV1,
    activation: &ContributionActivationV1,
    candidate: &ContributionRevisionV1,
    receipts: Vec<ProjectionReceiptV1>,
    checkpoint: &mut impl FnMut() -> Result<(), String>,
) -> Result<Option<ContributionHeadV1>, ContributionControllerError> {
    if activation.state() == ContributionActivationStateV1::Succeeded {
        let head = storage
            .contribution_head(payload.revision.scope_id(), payload.revision.service_id())?
            .ok_or_else(|| {
                ContributionControllerError::NeedsAttention(
                    "SUCCEEDED activation has no head".to_string(),
                )
            })?;
        if head.active_revision_id() != candidate.revision_id()
            || !required_consumer_receipts_match(&receipts, ProjectionReceiptStateV1::Active, None)
        {
            return Err(ContributionControllerError::NeedsAttention(
                "SUCCEEDED activation lacks matching head/receipt evidence".to_string(),
            ));
        }
        return Ok(Some(head));
    }
    if activation.state() == ContributionActivationStateV1::Committing {
        let head = publish_or_recover_candidate_head(storage, payload, candidate)?;
        record_api_registry_snapshot(storage, payload, candidate.generation())?;
        return Ok(Some(head));
    }
    if activation.state() != ContributionActivationStateV1::Preparing {
        return Err(ContributionControllerError::Conflict(format!(
            "COMMIT requires PREPARING activation, found {:?}",
            activation.state()
        )));
    }
    if receipts
        .iter()
        .any(|receipt| receipt.state() != ProjectionReceiptStateV1::Staged)
    {
        return Err(ContributionControllerError::Conflict(
            "COMMIT requires every projection receipt to be STAGED".to_string(),
        ));
    }
    ensure_runtime_gate(storage, candidate)?;
    checkpoint().map_err(ContributionControllerError::NeedsAttention)?;

    let committing = activation
        .begin_commit()
        .map_err(|error| invalid(error.to_string()))?;
    storage.put_contribution_activation_bundle(&committing, &receipts)?;
    let head = publish_or_recover_candidate_head(storage, payload, candidate)?;
    checkpoint().map_err(ContributionControllerError::NeedsAttention)?;
    record_api_registry_snapshot(storage, payload, candidate.generation())?;
    Ok(Some(head))
}

fn publish_or_recover_candidate_head(
    storage: &DurableStore,
    payload: &ContributionJobPayloadV1,
    candidate: &ContributionRevisionV1,
) -> Result<ContributionHeadV1, ContributionControllerError> {
    let head = storage.contribution_head(candidate.scope_id(), candidate.service_id())?;
    if head
        .as_ref()
        .is_some_and(|head| head.active_revision_id() == candidate.revision_id())
    {
        return Ok(head.expect("matching head exists"));
    }
    if head.as_ref().map(ContributionHeadV1::etag) != payload.expected_head_etag.as_deref()
        || candidate.status() != ContributionRevisionStatusV1::Staged
    {
        return Err(ContributionControllerError::NeedsAttention(
            "COMMITTING activation has neither its candidate head nor the exact pre-CAS head"
                .to_string(),
        ));
    }
    let active_candidate = candidate
        .activate()
        .map_err(|error| invalid(error.to_string()))?;
    storage
        .compare_and_swap_contribution_head(
            payload.expected_head_etag.as_deref(),
            &active_candidate,
        )
        .map_err(Into::into)
}

fn record_api_registry_snapshot(
    storage: &DurableStore,
    payload: &ContributionJobPayloadV1,
    candidate_generation: u64,
) -> Result<String, ContributionControllerError> {
    let snapshot = active_contribution_snapshot(storage, payload.revision.scope_id())
        .map_err(|error| ContributionControllerError::Persistence(error.to_string()))?;
    let snapshot_digest = snapshot
        .get("digest")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            ContributionControllerError::NeedsAttention(
                "published Contribution snapshot has no digest".to_string(),
            )
        })?
        .to_string();
    let current = required_receipts(storage, payload)?
        .into_iter()
        .find(|receipt| receipt.target() == ProjectionTargetV1::ApiRegistry)
        .ok_or_else(|| {
            ContributionControllerError::NeedsAttention(
                "activation has no API Registry receipt".to_string(),
            )
        })?;
    if current.state() == ProjectionReceiptStateV1::Active
        && current.active_digest() == Some(snapshot_digest.as_str())
    {
        return Ok(snapshot_digest);
    }
    let observed = current
        .record(
            ProjectionReceiptStateV1::Active,
            Some(candidate_generation),
            current.staged_digest().map(str::to_string),
            Some(snapshot_digest.clone()),
            None,
        )
        .map_err(|error| invalid(error.to_string()))?;
    storage.compare_and_swap_contribution_projection_receipt(&current, &observed)?;
    Ok(snapshot_digest)
}

fn execute_ack_gate(
    storage: &DurableStore,
    payload: &ContributionJobPayloadV1,
    activation: &ContributionActivationV1,
    candidate: &ContributionRevisionV1,
    receipts: Vec<ProjectionReceiptV1>,
) -> Result<Option<ContributionHeadV1>, ContributionControllerError> {
    let head = storage
        .contribution_head(payload.revision.scope_id(), payload.revision.service_id())?
        .ok_or_else(|| {
            ContributionControllerError::NeedsAttention(
                "ACK gate found no published Contribution head".to_string(),
            )
        })?;
    if head.active_revision_id() != candidate.revision_id() {
        return Err(ContributionControllerError::NeedsAttention(
            "ACK gate head no longer names its candidate".to_string(),
        ));
    }
    if activation.state() == ContributionActivationStateV1::Succeeded {
        if required_consumer_receipts_match(&receipts, ProjectionReceiptStateV1::Active, None) {
            return Ok(Some(head));
        }
        return Err(ContributionControllerError::NeedsAttention(
            "SUCCEEDED activation lacks authoritative consumer receipts".to_string(),
        ));
    }
    if activation.state() != ContributionActivationStateV1::Committing {
        return Err(ContributionControllerError::Conflict(format!(
            "ACK gate requires COMMITTING activation, found {:?}",
            activation.state()
        )));
    }
    let snapshot_digest = record_api_registry_snapshot(storage, payload, candidate.generation())?;
    let receipts = required_receipts(storage, payload)?;
    if !required_consumer_receipts_match(
        &receipts,
        ProjectionReceiptStateV1::Active,
        Some(&snapshot_digest),
    ) {
        return Err(ContributionControllerError::Retryable(format!(
            "Gateway/Auth have not both applied generation {} snapshot {snapshot_digest}",
            candidate.generation()
        )));
    }

    let succeeded = activation
        .succeed()
        .map_err(|error| invalid(error.to_string()))?;
    storage.put_contribution_activation_bundle(&succeeded, &receipts)?;
    if let Some(previous_id) = candidate.previous_revision_id() {
        let previous = storage.contribution_revision(previous_id)?.ok_or_else(|| {
            ContributionControllerError::NotFound(format!("revision {previous_id}"))
        })?;
        if previous.status() == ContributionRevisionStatusV1::Active {
            storage.transition_contribution_revision(
                &previous
                    .retire()
                    .map_err(|error| invalid(error.to_string()))?,
            )?;
        }
    }
    Ok(Some(head))
}

fn execute_abort(
    storage: &DurableStore,
    payload: &ContributionJobPayloadV1,
    activation: &ContributionActivationV1,
    candidate: &ContributionRevisionV1,
    receipts: Vec<ProjectionReceiptV1>,
    checkpoint: &mut impl FnMut() -> Result<(), String>,
) -> Result<Option<ContributionHeadV1>, ContributionControllerError> {
    let candidate_was_staged = candidate.status() == ContributionRevisionStatusV1::Staged;
    if activation.state() == ContributionActivationStateV1::Aborted {
        return verify_abort_evidence(storage, payload, candidate, &receipts);
    }
    let intent = operation_termination_intent(storage, payload)?;
    let compensating = match activation.state() {
        ContributionActivationStateV1::Preparing
        | ContributionActivationStateV1::Committing
        | ContributionActivationStateV1::Succeeded => activation
            .begin_compensation(intent)
            .map_err(|error| invalid(error.to_string()))?,
        ContributionActivationStateV1::Compensating => activation.clone(),
        ContributionActivationStateV1::NeedsAttention => {
            return Err(ContributionControllerError::NeedsAttention(
                "activation already requires explicit reconciliation".to_string(),
            ));
        }
        ContributionActivationStateV1::Aborted => unreachable!("handled above"),
    };
    if activation.state() != ContributionActivationStateV1::Compensating {
        storage.put_contribution_activation_bundle(&compensating, &receipts)?;
    }
    checkpoint().map_err(ContributionControllerError::NeedsAttention)?;

    let head =
        storage.contribution_head(payload.revision.scope_id(), payload.revision.service_id())?;
    let restored_head = if candidate.status() == ContributionRevisionStatusV1::Staged {
        if head.as_ref().map(ContributionHeadV1::etag) != payload.expected_head_etag.as_deref() {
            return Err(ContributionControllerError::NeedsAttention(
                "staged abort found a changed head".to_string(),
            ));
        }
        storage.transition_contribution_revision(
            &candidate
                .abort()
                .map_err(|error| invalid(error.to_string()))?,
        )?;
        head
    } else if candidate.status() == ContributionRevisionStatusV1::Active {
        let current = head.ok_or_else(|| {
            ContributionControllerError::NeedsAttention(
                "active candidate abort found no contribution head".to_string(),
            )
        })?;
        checkpoint().map_err(ContributionControllerError::NeedsAttention)?;
        Some(
            if let Some(previous_id) = candidate.previous_revision_id() {
                let previous = storage.contribution_revision(previous_id)?.ok_or_else(|| {
                    ContributionControllerError::NeedsAttention(format!(
                        "revision {previous_id} required for Contribution restore is missing"
                    ))
                })?;
                ensure_restore_runtime_gate(storage, payload, &previous)?;
                storage.restore_contribution_head(
                    current.etag(),
                    candidate.revision_id(),
                    previous_id,
                )?
            } else {
                storage.clear_initial_contribution_head(current.etag(), candidate.revision_id())?
            },
        )
    } else {
        head
    };

    // PREPARE-only cancellation has never exposed the candidate to any
    // external consumer. No restore observation is meaningful or required.
    if candidate_was_staged {
        let mut terminal_receipts = Vec::with_capacity(receipts.len());
        for receipt in receipts {
            terminal_receipts.push(match receipt.state() {
                ProjectionReceiptStateV1::Pending => receipt
                    .record(
                        ProjectionReceiptStateV1::Failed,
                        None,
                        None,
                        None,
                        Some("activation aborted before projection stage".to_string()),
                    )
                    .map_err(|error| invalid(error.to_string()))?,
                ProjectionReceiptStateV1::Staged => receipt
                    .record(
                        ProjectionReceiptStateV1::Failed,
                        None,
                        receipt.staged_digest().map(str::to_string),
                        None,
                        Some("candidate projection was never published".to_string()),
                    )
                    .map_err(|error| invalid(error.to_string()))?,
                other => {
                    return Err(ContributionControllerError::NeedsAttention(format!(
                        "unpublished candidate has unexpected {other:?} projection evidence"
                    )));
                }
            });
        }
        let aborted = compensating
            .finish_abort()
            .map_err(|error| invalid(error.to_string()))?;
        storage.put_contribution_activation_bundle(&aborted, &terminal_receipts)?;
        return Ok(restored_head);
    }

    // The in-process API registry can prove its own restored snapshot. The
    // Gateway/Auth receipts must be advanced only by their consumer endpoint.
    let snapshot = active_contribution_snapshot(storage, candidate.scope_id())
        .map_err(|error| ContributionControllerError::Persistence(error.to_string()))?;
    let snapshot_digest = snapshot
        .get("digest")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            ContributionControllerError::NeedsAttention(
                "restored Contribution snapshot has no digest".to_string(),
            )
        })?;
    let mut api_registry_receipt = None;
    for receipt in receipts {
        checkpoint().map_err(ContributionControllerError::NeedsAttention)?;
        let restored = if receipt.target() != ProjectionTargetV1::ApiRegistry {
            receipt
        } else {
            match receipt.state() {
                ProjectionReceiptStateV1::Staged
                | ProjectionReceiptStateV1::Active
                | ProjectionReceiptStateV1::Failed
                | ProjectionReceiptStateV1::Unknown => receipt.record(
                    ProjectionReceiptStateV1::Restored,
                    restored_head.as_ref().map(ContributionHeadV1::generation),
                    receipt.staged_digest().map(str::to_string),
                    Some(snapshot_digest.to_string()),
                    None,
                ),
                ProjectionReceiptStateV1::Pending => receipt.record(
                    ProjectionReceiptStateV1::Failed,
                    None,
                    None,
                    None,
                    Some("activation aborted before projection stage".to_string()),
                ),
                ProjectionReceiptStateV1::Restored => Ok(receipt),
            }
            .map_err(|error| invalid(error.to_string()))?
        };
        if restored.target() == ProjectionTargetV1::ApiRegistry {
            api_registry_receipt = Some(restored);
        }
    }
    storage.put_contribution_activation_bundle(
        &compensating,
        &[api_registry_receipt.ok_or_else(|| {
            ContributionControllerError::NeedsAttention(
                "activation has no API Registry receipt".to_string(),
            )
        })?],
    )?;
    let restored_receipts = required_receipts(storage, payload)?;
    if !required_consumer_receipts_match(
        &restored_receipts,
        ProjectionReceiptStateV1::Restored,
        Some(snapshot_digest),
    ) {
        return Err(ContributionControllerError::RetryableCompensation(format!(
            "Gateway/Auth have not both applied restored snapshot {snapshot_digest}; candidate runtime is retained",
        )));
    }
    let aborted = compensating
        .finish_abort()
        .map_err(|error| invalid(error.to_string()))?;
    storage.put_contribution_activation_bundle(&aborted, &restored_receipts)?;
    Ok(restored_head)
}

pub(crate) fn recover_expired_contribution_job(
    storage: &DurableStore,
    job: &Job,
) -> Result<Option<Value>, ContributionControllerError> {
    let payload: ContributionJobPayloadV1 = serde_json::from_value(job.payload.clone())
        .map_err(|error| invalid(format!("decode expired contribution job: {error}")))?;
    payload.validate()?;
    let activation = storage.contribution_activation(&payload.activation_id)?;
    if activation.is_none() {
        let candidate = storage.contribution_revision(payload.revision.revision_id())?;
        return if payload.phase == ContributionJobPhase::Abort && candidate.is_none() {
            Ok(Some(json!({
                "controller": CONTRIBUTION_JOB_CONTROLLER,
                "phase": "ABORT",
                "activation_id": payload.activation_id,
                "recovered_from_durable_evidence": true,
                "not_staged": true,
            })))
        } else {
            Ok(None)
        };
    }
    let activation = activation.expect("checked above");
    ensure_payload_identity(&payload, &activation)?;
    let candidate = required_candidate(storage, &payload, &activation)?;
    let receipts = required_receipts(storage, &payload)?;
    let proved = match payload.phase {
        ContributionJobPhase::Prepare => receipts.iter().all(|receipt| {
            matches!(
                receipt.state(),
                ProjectionReceiptStateV1::Staged
                    | ProjectionReceiptStateV1::Active
                    | ProjectionReceiptStateV1::Restored
            )
        }),
        ContributionJobPhase::Commit => {
            matches!(
                activation.state(),
                ContributionActivationStateV1::Committing
                    | ContributionActivationStateV1::Succeeded
            ) && storage
                .contribution_head(payload.revision.scope_id(), payload.revision.service_id())?
                .as_ref()
                .is_some_and(|head| head.active_revision_id() == candidate.revision_id())
        }
        ContributionJobPhase::AckGate => {
            activation.state() == ContributionActivationStateV1::Succeeded
                && storage
                    .contribution_head(payload.revision.scope_id(), payload.revision.service_id())?
                    .as_ref()
                    .is_some_and(|head| head.active_revision_id() == candidate.revision_id())
                && required_consumer_receipts_match(
                    &receipts,
                    ProjectionReceiptStateV1::Active,
                    None,
                )
        }
        ContributionJobPhase::Abort => {
            activation.state() == ContributionActivationStateV1::Aborted
                && verify_abort_evidence(storage, &payload, &candidate, &receipts).is_ok()
        }
    };
    Ok(proved.then(|| {
        json!({
            "controller": CONTRIBUTION_JOB_CONTROLLER,
            "phase": format!("{:?}", payload.phase).to_ascii_uppercase(),
            "activation_id": payload.activation_id,
            "recovered_from_durable_evidence": true,
        })
    }))
}

fn validate_stage_lineage(
    head: Option<&ContributionHeadV1>,
    candidate: &ContributionRevisionV1,
    history: &[ContributionRevisionV1],
) -> Result<(), ContributionControllerError> {
    let historical_generation = history
        .iter()
        .filter(|revision| revision.service_id() == candidate.service_id())
        .map(ContributionRevisionV1::generation)
        .max()
        .unwrap_or(0);
    let expected_generation = historical_generation.checked_add(1).ok_or_else(|| {
        ContributionControllerError::Conflict("contribution generation overflow".to_string())
    })?;
    match head {
        None if candidate.generation() == expected_generation
            && candidate.previous_revision_id().is_none() =>
        {
            Ok(())
        }
        Some(head)
            if candidate.generation() == expected_generation
                && candidate.generation() > head.generation()
                && candidate.previous_revision_id() == Some(head.active_revision_id()) =>
        {
            Ok(())
        }
        None => Err(ContributionControllerError::Conflict(format!(
            "headless contribution must be generation {expected_generation} without previous_revision_id"
        ))),
        Some(head) => Err(ContributionControllerError::Conflict(format!(
            "candidate must extend head {} at generation {}",
            head.active_revision_id(),
            head.generation()
        ))),
    }
}

pub(crate) fn next_contribution_generation(
    repository: &impl ContributionRepository,
    scope_id: &str,
    service_id: &str,
    head_generation: u64,
) -> Result<u64, ContributionControllerError> {
    repository
        .contribution_revisions(scope_id, Some(service_id))?
        .into_iter()
        .map(|revision| revision.generation())
        .fold(head_generation, u64::max)
        .checked_add(1)
        .ok_or_else(|| {
            ContributionControllerError::Conflict("contribution generation overflow".to_string())
        })
}

fn required_activation(
    storage: &DurableStore,
    payload: &ContributionJobPayloadV1,
) -> Result<ContributionActivationV1, ContributionControllerError> {
    let activation = storage
        .contribution_activation(&payload.activation_id)?
        .ok_or_else(|| {
            ContributionControllerError::NotFound(format!("activation {}", payload.activation_id))
        })?;
    ensure_payload_identity(payload, &activation)?;
    Ok(activation)
}

fn ensure_payload_identity(
    payload: &ContributionJobPayloadV1,
    activation: &ContributionActivationV1,
) -> Result<(), ContributionControllerError> {
    if activation.scope_id() != payload.revision.scope_id()
        || activation.service_id() != payload.revision.service_id()
        || activation.candidate_revision_id() != payload.revision.revision_id()
        || activation.expected_head_etag() != payload.expected_head_etag.as_deref()
    {
        return Err(ContributionControllerError::Conflict(
            "job payload identity does not match durable activation".to_string(),
        ));
    }
    Ok(())
}

fn required_candidate(
    storage: &DurableStore,
    payload: &ContributionJobPayloadV1,
    activation: &ContributionActivationV1,
) -> Result<ContributionRevisionV1, ContributionControllerError> {
    let candidate = storage
        .contribution_revision(payload.revision.revision_id())?
        .ok_or_else(|| {
            ContributionControllerError::NotFound(format!(
                "revision {}",
                payload.revision.revision_id()
            ))
        })?;
    if candidate.scope_id() != payload.revision.scope_id()
        || candidate.service_id() != payload.revision.service_id()
        || candidate.previous_revision_id() != activation.previous_revision_id()
        || candidate.revision_id() != payload.revision.revision_id()
    {
        return Err(ContributionControllerError::Conflict(
            "candidate identity does not match durable activation".to_string(),
        ));
    }
    Ok(candidate)
}

fn required_receipts(
    storage: &DurableStore,
    payload: &ContributionJobPayloadV1,
) -> Result<Vec<ProjectionReceiptV1>, ContributionControllerError> {
    let receipts = storage.contribution_projection_receipts(&payload.activation_id)?;
    if receipts.len() != ProjectionTargetV1::ALL.len()
        || !ProjectionTargetV1::ALL
            .iter()
            .all(|target| receipts.iter().any(|receipt| receipt.target() == *target))
    {
        return Err(ContributionControllerError::NeedsAttention(
            "activation is missing one or more projection receipts".to_string(),
        ));
    }
    Ok(receipts)
}

fn required_consumer_receipts_match(
    receipts: &[ProjectionReceiptV1],
    state: ProjectionReceiptStateV1,
    snapshot_digest: Option<&str>,
) -> bool {
    [ProjectionTargetV1::Gateway, ProjectionTargetV1::Auth]
        .into_iter()
        .all(|target| {
            receipts.iter().any(|receipt| {
                receipt.target() == target
                    && receipt.state() == state
                    && snapshot_digest.is_none_or(|digest| receipt.active_digest() == Some(digest))
            })
        })
}

fn ensure_runtime_gate(
    storage: &DurableStore,
    candidate: &ContributionRevisionV1,
) -> Result<(), ContributionControllerError> {
    ensure_runtime_gate_at_least(storage, candidate, None)
}

fn ensure_runtime_gate_at_least(
    storage: &DurableStore,
    candidate: &ContributionRevisionV1,
    minimum_observed_at_ms: Option<i64>,
) -> Result<(), ContributionControllerError> {
    let runtime = storage
        .runtime_instance(candidate.deployment_id())
        .map_err(|error| ContributionControllerError::Persistence(error.to_string()))?
        .ok_or_else(|| {
            ContributionControllerError::Conflict("candidate runtime is absent".to_string())
        })?;
    if runtime.instance.service_id != candidate.service_id()
        || canonical_artifact_digest(&runtime.instance.artifact_digest)
            != Some(candidate.release_digest())
        || runtime.instance.desired_state != RuntimeDesiredState::Running
        || runtime.instance.observed_state != RuntimeObservedState::Running
        || !runtime.instance.health.eq_ignore_ascii_case("HEALTHY")
        || minimum_observed_at_ms.is_some_and(|watermark| runtime.last_observed_at_ms < watermark)
        || !runtime.drift_reason.trim().is_empty()
        || (runtime.management_mode == orchestrator_storage::RuntimeManagementMode::Managed
            && !runtime.instance.runtime_attested)
    {
        return Err(ContributionControllerError::Conflict(
            "candidate runtime/release/health evidence does not satisfy activation gate"
                .to_string(),
        ));
    }
    Ok(())
}

fn ensure_restore_runtime_gate(
    storage: &DurableStore,
    payload: &ContributionJobPayloadV1,
    previous: &ContributionRevisionV1,
) -> Result<(), ContributionControllerError> {
    let publishes_content = !previous.api_surfaces().is_empty()
        || !previous.operation_routes().is_empty()
        || !previous.permission_definitions().is_empty()
        || !previous.user_frontend_modules().is_empty()
        || !previous.admin_frontend_modules().is_empty();
    if !publishes_content {
        return Ok(());
    }
    let minimum_observed_at_ms = payload
        .restore_runtime_gate_step_id
        .as_deref()
        .map(|step_id| required_succeeded_runtime_gate(storage, payload, step_id))
        .transpose()?;
    ensure_runtime_gate_at_least(storage, previous, minimum_observed_at_ms).map_err(|error| {
        ContributionControllerError::NeedsAttention(format!(
            "refusing to restore Contribution without healthy previous runtime evidence: {error}"
        ))
    })
}

fn required_succeeded_runtime_gate(
    storage: &DurableStore,
    payload: &ContributionJobPayloadV1,
    step_id: &str,
) -> Result<i64, ContributionControllerError> {
    let operation = storage
        .operation_store()
        .get(&payload.activation_id)
        .map_err(|error| ContributionControllerError::Persistence(error.to_string()))?
        .ok_or_else(|| {
            ContributionControllerError::NeedsAttention(
                "restore runtime gate has no owning durable Operation".to_string(),
            )
        })?;
    let binding = operation.active_binding(step_id).ok_or_else(|| {
        ContributionControllerError::NeedsAttention(format!(
            "restore runtime gate {step_id} has no materialized Job"
        ))
    })?;
    let job = storage
        .job_store()
        .get(&binding.job_id)
        .map_err(|error| ContributionControllerError::Persistence(error.to_string()))?
        .ok_or_else(|| {
            ContributionControllerError::NeedsAttention(format!(
                "restore runtime gate Job {} is missing",
                binding.job_id
            ))
        })?;
    if job.kind != JobKind::Health || job.status != JobStatus::Succeeded {
        return Err(ContributionControllerError::NeedsAttention(format!(
            "restore runtime gate {step_id} is not a successful Health Job"
        )));
    }
    job.result
        .as_ref()
        .and_then(|result| result.get("runtime_observed_at_ms"))
        .and_then(Value::as_i64)
        .filter(|watermark| *watermark > 0)
        .ok_or_else(|| {
            ContributionControllerError::NeedsAttention(format!(
                "restore runtime gate {step_id} lacks a causal observation watermark"
            ))
        })
}

fn canonical_artifact_digest(value: &str) -> Option<&str> {
    let value = value.trim();
    if let Some(digest) = value.strip_prefix("sha256:") {
        return Some(value).filter(|_| digest.len() == 64);
    }
    value
        .rsplit_once("@sha256:")
        .map(|(_, digest)| digest)
        .filter(|digest| digest.len() == 64)
        .map(|digest| &value[value.len() - digest.len() - "sha256:".len()..])
}

fn operation_termination_intent(
    storage: &DurableStore,
    payload: &ContributionJobPayloadV1,
) -> Result<ContributionTerminationIntentV1, ContributionControllerError> {
    let operation = storage
        .operation_store()
        .get(&payload.activation_id)
        .map_err(|error| ContributionControllerError::Persistence(error.to_string()))?;
    Ok(
        if operation.is_some_and(|operation| {
            matches!(
                operation.status,
                orchestrator_control_plane::DurableOperationStatus::Cancelling
                    | orchestrator_control_plane::DurableOperationStatus::Cancelled
            )
        }) {
            ContributionTerminationIntentV1::Cancelled
        } else {
            ContributionTerminationIntentV1::Failed
        },
    )
}

fn verify_abort_evidence(
    storage: &DurableStore,
    payload: &ContributionJobPayloadV1,
    candidate: &ContributionRevisionV1,
    receipts: &[ProjectionReceiptV1],
) -> Result<Option<ContributionHeadV1>, ContributionControllerError> {
    let head =
        storage.contribution_head(payload.revision.scope_id(), payload.revision.service_id())?;
    if candidate.status() == ContributionRevisionStatusV1::Aborted {
        if head.as_ref().map(ContributionHeadV1::etag) != payload.expected_head_etag.as_deref()
            || receipts
                .iter()
                .any(|receipt| receipt.state() != ProjectionReceiptStateV1::Failed)
        {
            return Err(ContributionControllerError::NeedsAttention(
                "unpublished ABORTED activation lacks terminal no-publication evidence".to_string(),
            ));
        }
        return Ok(head);
    }
    if !required_consumer_receipts_match(receipts, ProjectionReceiptStateV1::Restored, None)
        || receipts.iter().any(|receipt| {
            receipt.target() == ProjectionTargetV1::ApiRegistry
                && receipt.state() != ProjectionReceiptStateV1::Restored
        })
    {
        return Err(ContributionControllerError::NeedsAttention(
            "ABORTED activation has unproven projection restoration".to_string(),
        ));
    }
    if let Some(previous) = candidate.previous_revision_id() {
        if head.as_ref().map(ContributionHeadV1::active_revision_id) != Some(previous) {
            return Err(ContributionControllerError::NeedsAttention(
                "ABORTED activation did not restore the previous head".to_string(),
            ));
        }
    } else {
        let head = head.as_ref().ok_or_else(|| {
            ContributionControllerError::NeedsAttention(
                "initial ABORTED activation has no monotonic tombstone head".to_string(),
            )
        })?;
        if head.active_revision_id() == candidate.revision_id()
            || head.generation() <= candidate.generation()
        {
            return Err(ContributionControllerError::NeedsAttention(
                "initial ABORTED activation still publishes the candidate head".to_string(),
            ));
        }
        let tombstone = storage
            .contribution_revision(head.active_revision_id())?
            .ok_or_else(|| {
                ContributionControllerError::NeedsAttention(
                    "initial ABORTED activation tombstone revision is absent".to_string(),
                )
            })?;
        if tombstone.status() != ContributionRevisionStatusV1::Active
            || tombstone.previous_revision_id() != Some(candidate.revision_id())
            || !tombstone.api_surfaces().is_empty()
            || !tombstone.operation_routes().is_empty()
            || !tombstone.permission_definitions().is_empty()
            || !tombstone.user_frontend_modules().is_empty()
            || !tombstone.admin_frontend_modules().is_empty()
        {
            return Err(ContributionControllerError::NeedsAttention(
                "initial ABORTED activation tombstone still exposes contribution content"
                    .to_string(),
            ));
        }
    }
    Ok(head)
}

fn target_projection_digest(
    candidate: &ContributionRevisionV1,
    target: ProjectionTargetV1,
) -> String {
    let mut hasher = Sha256::new();
    hasher.update(candidate.revision_id().as_bytes());
    hasher.update([0]);
    hasher.update(target.as_str().as_bytes());
    format!("sha256:{:x}", hasher.finalize())
}

fn stable_step_suffix(activation_id: &str) -> String {
    let digest = canonical_payload_sha256(&json!({"activation_id": activation_id}));
    digest
        .strip_prefix("sha256:")
        .unwrap_or(&digest)
        .chars()
        .take(16)
        .collect()
}

fn invalid(message: impl Into<String>) -> ContributionControllerError {
    ContributionControllerError::Invalid(message.into())
}
