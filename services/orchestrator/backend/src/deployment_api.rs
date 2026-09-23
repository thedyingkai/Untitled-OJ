use crate::contribution_controller::{
    ContributionUninstallDagV1, append_contribution_uninstall_job_fragment,
    stage_contribution_uninstall,
};
use crate::durable::{DurableError, DurableStore};
use crate::http::{ApiRequest, ApiResponse, query_value};
use orchestrator_control_plane::{
    JobKind, OperationCoordinator, PlanOperation, PlannedJob, PlannedJobCondition,
};
use orchestrator_storage::{ApiBinding, ApiBindingState};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::time::{SystemTime, UNIX_EPOCH};

pub(crate) fn route(
    storage: Option<&DurableStore>,
    request: &ApiRequest,
    request_id: &str,
) -> Option<ApiResponse> {
    let path = request.path.split('?').next().unwrap_or("/");
    let segments = path.trim_start_matches('/').split('/').collect::<Vec<_>>();
    if segments.get(0..3) != Some(&["api", "v1", "deployments"]) {
        return None;
    }
    let Some(storage) = storage else {
        return Some(problem(
            503,
            "DEPLOYMENT_STORAGE_UNAVAILABLE",
            "deployment lifecycle requires durable runtime projections",
            request_id,
            None,
        ));
    };
    Some(
        match route_with_store(storage, request, &segments, request_id) {
            Ok(response) => response,
            Err(error) => problem(
                error.status,
                error.code,
                error.detail,
                request_id,
                error.operation_id.as_deref(),
            ),
        },
    )
}

fn route_with_store(
    storage: &DurableStore,
    request: &ApiRequest,
    segments: &[&str],
    request_id: &str,
) -> Result<ApiResponse, DeploymentApiError> {
    match (request.method.as_str(), segments) {
        ("GET", ["api", "v1", "deployments"]) => list(storage, request, request_id),
        ("GET", ["api", "v1", "deployments", deployment_id]) => {
            let deployment = required_deployment(storage, deployment_id)?;
            let deployment = storage
                .runtime_with_current_evidence(deployment, now_ms())
                .map_err(storage_error)?;
            Ok(success(200, json!({"deployment": deployment}), request_id))
        }
        ("GET", ["api", "v1", "deployments", deployment_id, "health"]) => {
            let deployment = required_deployment(storage, deployment_id)?;
            let deployment = storage
                .runtime_with_current_evidence(deployment, now_ms())
                .map_err(storage_error)?;
            Ok(success(
                200,
                json!({
                    "deployment_id": deployment_id,
                    "health": deployment.instance.health,
                    "observed_state": deployment.instance.observed_state,
                    "runtime_attested": deployment.instance.runtime_attested,
                    "drift_reason": deployment.drift_reason,
                    "observed_at_ms": deployment.last_observed_at_ms,
                    "credential_expires_at_ms": deployment.credential_expires_at_ms,
                    "credential_last_success_at_ms": deployment.credential_last_success_at_ms,
                    "credential_last_error": deployment.credential_last_error,
                    "updated_at": deployment.updated_at,
                }),
                request_id,
            ))
        }
        ("GET", ["api", "v1", "deployments", deployment_id, "bindings"]) => {
            let deployment = required_deployment(storage, deployment_id)?;
            let evidence_at_ms = now_ms();
            let deployment = storage
                .runtime_with_current_evidence(deployment, evidence_at_ms)
                .map_err(storage_error)?;
            let bindings = storage
                .api_bindings_for_deployment(deployment_id)
                .map_err(storage_error)?
                .into_iter()
                .map(|binding| {
                    storage
                        .binding_with_current_runtime_evidence(binding, evidence_at_ms)
                        .map_err(storage_error)
                })
                .collect::<Result<Vec<_>, _>>()?;
            let mut provider_bindings = Vec::new();
            for heads in storage.list_topology_heads().map_err(storage_error)? {
                provider_bindings.extend(
                    storage
                        .api_bindings_for_topology(&heads.topology_id)
                        .map_err(storage_error)?
                        .into_iter()
                        .filter(|binding| binding.provider_deployment_id == *deployment_id)
                        .map(|binding| {
                            storage
                                .binding_with_current_runtime_evidence(binding, evidence_at_ms)
                                .map_err(storage_error)
                        })
                        .collect::<Result<Vec<_>, _>>()?,
                );
            }
            provider_bindings.sort_by(|left, right| {
                (
                    &left.topology_id,
                    &left.consumer_deployment_id,
                    &left.requirement_name,
                )
                    .cmp(&(
                        &right.topology_id,
                        &right.consumer_deployment_id,
                        &right.requirement_name,
                    ))
            });
            Ok(success(
                200,
                json!({
                    "deployment_id": deployment_id,
                    "service_id": deployment.instance.service_id,
                    "items": bindings,
                    "provider_items": provider_bindings,
                }),
                request_id,
            ))
        }
        ("POST", ["api", "v1", "deployments", deployment_action]) => {
            let (deployment_id, action) = deployment_action.rsplit_once(':').ok_or_else(|| {
                invalid("deployment lifecycle route must contain an action suffix")
            })?;
            enqueue_lifecycle(storage, deployment_id, action, request, request_id)
        }
        _ => Err(DeploymentApiError {
            status: 404,
            code: "ROUTE_NOT_FOUND",
            detail: "the requested Deployment v1 route does not exist".to_string(),
            operation_id: None,
        }),
    }
}

fn list(
    storage: &DurableStore,
    request: &ApiRequest,
    request_id: &str,
) -> Result<ApiResponse, DeploymentApiError> {
    let query = request
        .path
        .split_once('?')
        .map(|(_, query)| query)
        .unwrap_or("");
    let cursor = query_value(query, "cursor")
        .map_err(|error| invalid(error.to_string()))?
        .unwrap_or_default();
    let limit = query_value(query, "limit")
        .map_err(|error| invalid(error.to_string()))?
        .map(|value| value.parse::<usize>())
        .transpose()
        .map_err(|_| invalid("limit must be an integer"))?
        .unwrap_or(50);
    if !(1..=200).contains(&limit) {
        return Err(invalid("limit must be between 1 and 200"));
    }
    let evidence_at_ms = now_ms();
    let mut deployments = storage
        .runtime_instances(None)
        .map_err(storage_error)?
        .into_iter()
        .map(|deployment| {
            storage
                .runtime_with_current_evidence(deployment, evidence_at_ms)
                .map_err(storage_error)
        })
        .collect::<Result<Vec<_>, _>>()?;
    deployments.sort_by(|left, right| {
        left.instance
            .deployment_id
            .cmp(&right.instance.deployment_id)
    });
    let mut items = deployments
        .into_iter()
        .filter(|deployment| deployment.instance.deployment_id.as_str() > cursor.as_str())
        .take(limit + 1)
        .collect::<Vec<_>>();
    let next_cursor = if items.len() > limit {
        items.truncate(limit);
        items
            .last()
            .map(|deployment| deployment.instance.deployment_id.clone())
    } else {
        None
    };
    Ok(success(
        200,
        json!({"items": items, "next_cursor": next_cursor}),
        request_id,
    ))
}

fn enqueue_lifecycle(
    storage: &DurableStore,
    deployment_id: &str,
    action: &str,
    request: &ApiRequest,
    request_id: &str,
) -> Result<ApiResponse, DeploymentApiError> {
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
            return Err(DeploymentApiError {
                status: 404,
                code: "ROUTE_NOT_FOUND",
                detail: format!("unknown deployment action {action}"),
                operation_id: None,
            });
        }
    };
    let idempotency_key = request
        .headers
        .get("idempotency-key")
        .map(String::as_str)
        .unwrap_or_default();
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
    Ok(success(
        202,
        json!({"operation_id": operation_id, "operation": operation}),
        request_id,
    ))
}

fn ensure_uninstall_has_no_active_bindings(
    storage: &DurableStore,
    deployment_id: &str,
) -> Result<(), DeploymentApiError> {
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
    Err(DeploymentApiError {
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

fn required_deployment(
    storage: &DurableStore,
    deployment_id: &str,
) -> Result<orchestrator_storage::StoredRuntimeInstance, DeploymentApiError> {
    storage
        .runtime_instance(deployment_id)
        .map_err(storage_error)?
        .ok_or_else(|| DeploymentApiError {
            status: 404,
            code: "DEPLOYMENT_NOT_FOUND",
            detail: format!("deployment {deployment_id} was not found"),
            operation_id: None,
        })
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(i64::MAX as u128) as i64
}

fn success(status: u16, data: Value, request_id: &str) -> ApiResponse {
    let body = json!({
        "data": data,
        "meta": {"request_id": request_id, "api_version": "v1"},
    });
    let response = if status == 202 {
        ApiResponse::accepted(body)
    } else {
        ApiResponse::ok(body)
    };
    response.with_header("X-Request-ID", request_id)
}

fn problem(
    status: u16,
    code: &'static str,
    detail: impl Into<String>,
    request_id: &str,
    operation_id: Option<&str>,
) -> ApiResponse {
    ApiResponse::problem(status, code, detail, request_id, operation_id)
        .with_header("X-Request-ID", request_id)
}

#[derive(Debug)]
struct DeploymentApiError {
    status: u16,
    code: &'static str,
    detail: String,
    operation_id: Option<String>,
}

fn invalid(detail: impl Into<String>) -> DeploymentApiError {
    DeploymentApiError {
        status: 422,
        code: "DEPLOYMENT_INVALID",
        detail: detail.into(),
        operation_id: None,
    }
}

fn storage_error(error: DurableError) -> DeploymentApiError {
    DeploymentApiError {
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
) -> DeploymentApiError {
    DeploymentApiError {
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

fn operation_error(error: orchestrator_control_plane::OperationError) -> DeploymentApiError {
    let operation_id = match &error {
        orchestrator_control_plane::OperationError::NotFound(operation_id) => {
            Some(operation_id.clone())
        }
        _ => None,
    };
    DeploymentApiError {
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
