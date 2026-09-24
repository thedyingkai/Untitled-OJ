//! Deployment lifecycle coordination, independent of HTTP request/response types.
//! Keep active-binding rejection, contribution compensation, idempotency hashing
//! and OperationCoordinator ordering aligned with the existing lifecycle contract.
use crate::contribution_controller::{
    ContributionUninstallDagV1, append_contribution_uninstall_job_fragment,
    stage_contribution_uninstall,
};
use crate::durable::{DurableError, DurableStore};
use orchestrator_control_plane::{
    JobKind, OperationCoordinator, PlanOperation, PlannedJob, PlannedJobCondition,
};
use orchestrator_storage::{ApiBinding, ApiBindingState};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::time::{SystemTime, UNIX_EPOCH};

pub(crate) struct DeploymentLifecycle<'a> {
    pub(crate) deployment_id: &'a str,
    pub(crate) action: &'a str,
    pub(crate) idempotency_key: &'a str,
}

pub(crate) fn enqueue_lifecycle(
    storage: &DurableStore,
    command: DeploymentLifecycle<'_>,
) -> Result<Value, DeploymentError> {
    let DeploymentLifecycle {
        deployment_id,
        action,
        idempotency_key,
    } = command;
    let deployment = required_deployment(storage, deployment_id)?;
    if action == "uninstall" {
        ensure_uninstall_has_no_active_bindings(storage, deployment_id)?;
    }
    let (action_id, kind, payload) = match action {
        "start" => (
            "deployment.start",
            JobKind::Start,
            json!({"container_id": deployment.instance.container_id}),
        ),
        "stop" => (
            "deployment.stop",
            JobKind::Stop,
            json!({"container_id": deployment.instance.container_id, "timeout_seconds": 30}),
        ),
        "restart" => (
            "deployment.restart",
            JobKind::Restart,
            json!({"container_id": deployment.instance.container_id, "timeout_seconds": 30}),
        ),
        "uninstall" => (
            "deployment.uninstall",
            JobKind::Uninstall,
            json!({
                "deployment_id": deployment_id,
                "container_id": deployment.instance.container_id,
                "force": false,
            }),
        ),
        _ => {
            return Err(DeploymentError {
                status: 404,
                code: "ROUTE_NOT_FOUND",
                detail: format!("unknown deployment action {action}"),
                operation_id: None,
            });
        }
    };
    let digest =
        Sha256::digest(format!("{action_id}\0{deployment_id}\0{idempotency_key}").as_bytes());
    let operation_id = format!("op-deployment-{digest:x}");
    let staged_contribution = if action == "uninstall" {
        stage_contribution_uninstall(
            storage,
            &operation_id,
            "default",
            deployment_id,
            &deployment.instance.service_id,
        )
        .map_err(contribution_error)?
    } else {
        None
    };
    let mut planned_jobs = vec![PlannedJob {
        step_id: action.to_string(),
        node_id: deployment.node_id.clone(),
        kind,
        depends_on: vec![],
        condition: Default::default(),
        payload,
        max_attempts: 3,
    }];
    if let Some(contribution) = &staged_contribution {
        let restore_health_step_id = "contribution-restore-runtime-health".to_string();
        planned_jobs.push(PlannedJob {
            step_id: restore_health_step_id.clone(),
            node_id: deployment.node_id.clone(),
            kind: JobKind::Health,
            depends_on: vec![action.to_string()],
            condition: PlannedJobCondition::OnFailure,
            payload: json!({"container_id": deployment.instance.container_id}),
            max_attempts: 3,
        });
        append_contribution_uninstall_job_fragment(
            &mut planned_jobs,
            contribution,
            ContributionUninstallDagV1 {
                prepare_depends_on: Vec::new(),
                commit_depends_on: Vec::new(),
                runtime_uninstall_step_id: action.to_string(),
                restore_health_step_id,
            },
        )
        .map_err(contribution_error)?;
    }
    let plan = PlanOperation {
        operation_id: operation_id.clone(),
        action: action_id.to_string(),
        target_type: "Deployment".to_string(),
        target_id: deployment_id.to_string(),
        request: json!({"deployment_id": deployment_id, "auto_enqueue": true}),
        jobs: planned_jobs,
    };
    let mut operations = storage.operation_store();
    let mut jobs = storage.job_store();
    let mut coordinator = OperationCoordinator::new(&mut operations, &mut jobs);
    coordinator.plan(plan, now_ms()).map_err(operation_error)?;
    coordinator
        .confirm(&operation_id, now_ms())
        .map_err(operation_error)?;
    let operation = coordinator
        .enqueue(&operation_id, now_ms())
        .map_err(operation_error)?;
    Ok(json!({"operation_id": operation_id, "operation": operation}))
}

fn ensure_uninstall_has_no_active_bindings(
    storage: &DurableStore,
    deployment_id: &str,
) -> Result<(), DeploymentError> {
    let mut active = storage
        .api_bindings_for_deployment(deployment_id)
        .map_err(storage_error)?
        .into_iter()
        .filter(active_binding)
        .map(|binding| {
            format!(
                "consumer:{}:{}",
                binding.topology_id, binding.requirement_name
            )
        })
        .collect::<Vec<_>>();
    for heads in storage.list_topology_heads().map_err(storage_error)? {
        active.extend(
            storage
                .api_bindings_for_topology(&heads.topology_id)
                .map_err(storage_error)?
                .into_iter()
                .filter(|binding| {
                    binding.provider_deployment_id == deployment_id && active_binding(binding)
                })
                .map(|binding| {
                    format!(
                        "provider:{}:{}:{}",
                        binding.topology_id,
                        binding.consumer_deployment_id,
                        binding.requirement_name
                    )
                }),
        );
    }
    active.sort();
    active.dedup();
    if active.is_empty() {
        return Ok(());
    }
    Err(DeploymentError {
        status: 409,
        code: "DEPLOYMENT_ACTIVE_BINDINGS",
        detail: format!(
            "deployment {deployment_id} still participates in active API Bindings ({}); remove the corresponding Topology Links and apply those immutable revisions before uninstall",
            active.join(", ")
        ),
        operation_id: None,
    })
}

fn active_binding(binding: &ApiBinding) -> bool {
    binding.desired_state == "ACTIVE"
        && matches!(
            binding.state,
            ApiBindingState::Pending | ApiBindingState::Resolved | ApiBindingState::Active
        )
}

pub(crate) fn required_deployment(
    storage: &DurableStore,
    deployment_id: &str,
) -> Result<orchestrator_storage::StoredRuntimeInstance, DeploymentError> {
    storage
        .runtime_instance(deployment_id)
        .map_err(storage_error)?
        .ok_or_else(|| DeploymentError {
            status: 404,
            code: "DEPLOYMENT_NOT_FOUND",
            detail: format!("deployment {deployment_id} was not found"),
            operation_id: None,
        })
}

pub(crate) fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(i64::MAX as u128) as i64
}

#[derive(Debug)]
pub(crate) struct DeploymentError {
    pub(crate) status: u16,
    pub(crate) code: &'static str,
    pub(crate) detail: String,
    pub(crate) operation_id: Option<String>,
}

pub(crate) fn invalid(detail: impl Into<String>) -> DeploymentError {
    DeploymentError {
        status: 422,
        code: "DEPLOYMENT_INVALID",
        detail: detail.into(),
        operation_id: None,
    }
}

pub(crate) fn storage_error(error: DurableError) -> DeploymentError {
    DeploymentError {
        status: match error {
            DurableError::Conflict(_) => 409,
            DurableError::Invariant(_) | DurableError::Domain(_) => 422,
            DurableError::Storage(_) => 500,
        },
        code: "DEPLOYMENT_STORAGE_ERROR",
        detail: error.to_string(),
        operation_id: None,
    }
}

fn contribution_error(
    error: crate::contribution_controller::ContributionControllerError,
) -> DeploymentError {
    DeploymentError {
        status: match &error {
            crate::contribution_controller::ContributionControllerError::Conflict(_) => 409,
            crate::contribution_controller::ContributionControllerError::NotFound(_) => 404,
            crate::contribution_controller::ContributionControllerError::NeedsAttention(_) => 409,
            crate::contribution_controller::ContributionControllerError::Retryable(_)
            | crate::contribution_controller::ContributionControllerError::RetryableCompensation(
                _,
            ) => 409,
            crate::contribution_controller::ContributionControllerError::Invalid(_) => 422,
            crate::contribution_controller::ContributionControllerError::Persistence(_) => 500,
        },
        code: error.code(),
        detail: error.to_string(),
        operation_id: None,
    }
}

fn operation_error(error: orchestrator_control_plane::OperationError) -> DeploymentError {
    let operation_id = match &error {
        orchestrator_control_plane::OperationError::NotFound(operation_id) => {
            Some(operation_id.clone())
        }
        _ => None,
    };
    DeploymentError {
        status: match error {
            orchestrator_control_plane::OperationError::NotFound(_) => 404,
            orchestrator_control_plane::OperationError::InvalidPlan(_) => 422,
            orchestrator_control_plane::OperationError::IdempotencyConflict
            | orchestrator_control_plane::OperationError::InvalidTransition { .. } => 409,
            orchestrator_control_plane::OperationError::Store(_)
            | orchestrator_control_plane::OperationError::Job(_) => 500,
        },
        code: "DEPLOYMENT_OPERATION_ERROR",
        detail: error.to_string(),
        operation_id,
    }
}
