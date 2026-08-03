use crate::durable::{DurableError, DurableStore};
use crate::http::{ApiRequest, ApiResponse, query_value};
use orchestrator_control_plane::{JobKind, OperationCoordinator, PlanOperation, PlannedJob};
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
            Ok(success(200, json!({"deployment": deployment}), request_id))
        }
        ("GET", ["api", "v1", "deployments", deployment_id, "health"]) => {
            let deployment = required_deployment(storage, deployment_id)?;
            Ok(success(
                200,
                json!({
                    "deployment_id": deployment_id,
                    "health": deployment.instance.health,
                    "observed_state": deployment.instance.observed_state,
                    "updated_at": deployment.updated_at,
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
    let mut deployments = storage.runtime_instances(None).map_err(storage_error)?;
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
    let plan = PlanOperation {
        operation_id: operation_id.clone(),
        action: action_id.to_string(),
        target_type: "Deployment".to_string(),
        target_id: deployment_id.to_string(),
        request: json!({"deployment_id": deployment_id, "auto_enqueue": true}),
        jobs: vec![PlannedJob {
            step_id: action.to_string(),
            node_id: deployment.node_id,
            kind,
            depends_on: vec![],
            condition: Default::default(),
            payload,
            max_attempts: 3,
        }],
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

#[cfg(test)]
mod tests {
    use super::*;
    use orchestrator_control_plane::{JobStatus, JobStore, OperationRepository};
    use orchestrator_storage::{SqliteOrchestratorStore, StoredRuntimeInstance};

    fn fixture() -> (tempfile::TempDir, DurableStore) {
        let directory = tempfile::tempdir().expect("temporary store");
        let sqlite = SqliteOrchestratorStore::open(directory.path().join("orchestrator.db"))
            .expect("open sqlite store");
        let instance: StoredRuntimeInstance = serde_json::from_value(json!({
            "node_id": "node-1",
            "instance": {
                "deployment_id": "deployment-1",
                "service_id": "judge",
                "release_version": "1.0.0",
                "container_id": "container-1",
                "artifact_digest": format!("sha256:{}", "a".repeat(64)),
                "desired_state": "RUNNING",
                "observed_state": "RUNNING",
                "health": "healthy"
            },
            "management_mode": "MANAGED",
            "endpoint": "127.0.0.1:18080:judge",
            "updated_at": "unix-ms:1"
        }))
        .expect("runtime fixture");
        sqlite
            .put_runtime_instance(&instance)
            .expect("persist runtime fixture");
        (directory, DurableStore::Sqlite(sqlite))
    }

    fn request(method: &str, path: &str, idempotency_key: Option<&str>) -> ApiRequest {
        ApiRequest {
            method: method.to_string(),
            path: path.to_string(),
            headers: idempotency_key
                .map(|key| [("idempotency-key".to_string(), key.to_string())].into())
                .unwrap_or_default(),
            body: "{}".to_string(),
        }
    }

    #[test]
    fn lifecycle_action_creates_one_durable_job_for_the_owning_node() {
        let (_directory, storage) = fixture();
        let response = route(
            Some(&storage),
            &request(
                "POST",
                "/api/v1/deployments/deployment-1:restart",
                Some("restart-1"),
            ),
            "req-1",
        )
        .expect("deployment route");
        assert_eq!(response.status, 202, "{:?}", response.body);
        let operation_id = response.body["data"]["operation_id"]
            .as_str()
            .expect("operation id");
        let operation = storage
            .operation_store()
            .get(operation_id)
            .expect("load operation")
            .expect("stored operation");
        assert_eq!(operation.action, "deployment.restart");
        assert_eq!(operation.job_bindings.len(), 1);
        let job = storage
            .job_store()
            .get(&operation.job_bindings[0].job_id)
            .expect("load job")
            .expect("stored job");
        assert_eq!(job.status, JobStatus::Queued);
        assert_eq!(job.node_id, "node-1");
        assert_eq!(job.kind, JobKind::Restart);
        assert_eq!(job.payload["container_id"], "container-1");
    }

    #[test]
    fn list_is_cursor_paginated_and_health_uses_observed_projection() {
        let (_directory, storage) = fixture();
        let list = route(
            Some(&storage),
            &request("GET", "/api/v1/deployments?limit=1", None),
            "req-list",
        )
        .expect("deployment list route");
        assert_eq!(list.status, 200);
        assert_eq!(list.body["data"]["items"].as_array().unwrap().len(), 1);
        assert!(list.body["data"]["next_cursor"].is_null());

        let health = route(
            Some(&storage),
            &request("GET", "/api/v1/deployments/deployment-1/health", None),
            "req-health",
        )
        .expect("deployment health route");
        assert_eq!(health.status, 200);
        assert_eq!(health.body["data"]["health"], "healthy");
        assert_eq!(health.body["data"]["observed_state"], "RUNNING");
    }

    #[test]
    fn missing_projection_never_enqueues_a_fake_lifecycle_job() {
        let (_directory, storage) = fixture();
        let response = route(
            Some(&storage),
            &request(
                "POST",
                "/api/v1/deployments/missing:start",
                Some("missing-1"),
            ),
            "req-missing",
        )
        .expect("deployment route");
        assert_eq!(response.status, 404);
        assert_eq!(response.body["code"], "DEPLOYMENT_NOT_FOUND");
        assert!(storage.operation_store().list().unwrap().is_empty());
    }
}
