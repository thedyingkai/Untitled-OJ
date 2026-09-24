//! Deployment routes and response mapping. Lifecycle coordination lives in `deployment`.
use crate::deployment::{
    DeploymentError as DeploymentApiError, DeploymentLifecycle, invalid, now_ms,
    required_deployment, storage_error,
};
use crate::durable::DurableStore;
use crate::http::{ApiRequest, ApiResponse, query_value};
use serde_json::{Value, json};

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

fn enqueue_lifecycle(
    storage: &DurableStore,
    deployment_id: &str,
    action: &str,
    request: &ApiRequest,
    request_id: &str,
) -> Result<ApiResponse, DeploymentApiError> {
    let command = DeploymentLifecycle {
        deployment_id,
        action,
        idempotency_key: request
            .headers
            .get("idempotency-key")
            .map(String::as_str)
            .unwrap_or_default(),
    };
    let result = crate::deployment::enqueue_lifecycle(storage, command)?;
    Ok(success(202, result, request_id))
}
