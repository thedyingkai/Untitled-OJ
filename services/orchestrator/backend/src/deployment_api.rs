use crate::durable::{DurableError, DurableStore};
use crate::http::{ApiRequest, ApiResponse, query_value};
use orchestrator_control_plane::{JobKind, OperationCoordinator, PlanOperation, PlannedJob};
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
    use orchestrator_legacy::{TopologyEndpointSpec, TopologySpec};
    use orchestrator_storage::{
        SqliteOrchestratorStore, StoredNodeRuntimeFacts, StoredRuntimeInstance,
    };

    fn fixture() -> (tempfile::TempDir, DurableStore) {
        let directory = tempfile::tempdir().expect("temporary store");
        let sqlite = SqliteOrchestratorStore::open(directory.path().join("orchestrator.db"))
            .expect("open sqlite store");
        let observed_at_ms = now_ms();
        let instance: StoredRuntimeInstance = serde_json::from_value(json!({
            "node_id": "node-1",
            "instance": {
                "deployment_id": "deployment-1",
                "service_id": "judge",
                "release_version": "1.0.0",
                "container_id": "container-1",
                "artifact_digest": format!("sha256:{}", "a".repeat(64)),
                "runtime_attested": true,
                "desired_state": "RUNNING",
                "observed_state": "RUNNING",
                "health": "healthy"
            },
            "management_mode": "MANAGED",
            "endpoint": "127.0.0.1:18080:judge",
            "last_observed_at_ms": observed_at_ms,
            "updated_at": "unix-ms:1"
        }))
        .expect("runtime fixture");
        sqlite
            .put_runtime_instance(&instance)
            .expect("persist runtime fixture");
        sqlite
            .put_node_runtime_facts(&StoredNodeRuntimeFacts {
                node_id: "node-1".to_string(),
                observed_at_ms,
                received_at_ms: observed_at_ms,
                facts: json!({
                    "schema_version": 1,
                    "report_id": "deployment-api-fixture",
                    "inventory_complete": true
                }),
            })
            .expect("persist runtime facts fixture");
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

    fn binding(consumer_deployment_id: &str, provider_deployment_id: &str) -> ApiBinding {
        ApiBinding {
            binding_id: format!(
                "binding-{consumer_deployment_id}-{provider_deployment_id}-judge-control"
            ),
            requirement_name: "judge_control".to_string(),
            api_id: "judge.worker.control".to_string(),
            api_version: "1.0.0".to_string(),
            consumer_deployment_id: consumer_deployment_id.to_string(),
            consumer_service_id: "judge-worker".to_string(),
            consumer_node_id: "node-b".to_string(),
            consumer_endpoint: "10.0.0.2:9101:judge-worker".to_string(),
            provider_deployment_id: provider_deployment_id.to_string(),
            provider_service_id: "judge-api".to_string(),
            provider_node_id: "node-a".to_string(),
            provider_endpoint: "10.0.0.1:8080:judge-api".to_string(),
            provider_path: "/api/judge/worker".to_string(),
            virtual_endpoint: "/internal/apis/judge.worker.control".to_string(),
            protocol: "https".to_string(),
            methods: vec!["GET".to_string(), "POST".to_string()],
            auth_mode: "workload".to_string(),
            provider_auth_mode: "workload".to_string(),
            permission: "judge.worker".to_string(),
            timeout_ms: Some(35_000),
            topology_id: "primary".to_string(),
            topology_revision_id: "revision-1".to_string(),
            link_source_endpoint: "10.0.0.2:9101:judge-worker".to_string(),
            link_target_endpoint: "10.0.0.1:8080:judge-api".to_string(),
            credential_ref: String::new(),
            credential_generation: 1,
            context_generation: 1,
            desired_state: "ACTIVE".to_string(),
            observed_state: "ACTIVE".to_string(),
            health: "HEALTHY".to_string(),
            drift: Vec::new(),
            last_operation_id: "operation-1".to_string(),
            state: ApiBindingState::Active,
            optional: false,
            reason: String::new(),
            created_at: "unix-ms:1".to_string(),
            updated_at: "unix-ms:1".to_string(),
        }
    }

    fn create_topology(storage: &DurableStore) {
        let root = "127.0.0.1:8080:gateway";
        let spec = TopologySpec::new(
            "primary",
            root,
            "private",
            vec![TopologyEndpointSpec {
                endpoint: root.to_string(),
                service_id: "gateway".to_string(),
                protocol: "https".to_string(),
                health_path: "/healthz/ready".to_string(),
                display_name: "Gateway".to_string(),
                note: String::new(),
                config: json!({}),
            }],
            Vec::new(),
        )
        .expect("topology fixture");
        storage
            .create_initial_topology_revision(
                spec,
                "unix-ms:1".to_string(),
                "admin".to_string(),
                "initial".to_string(),
            )
            .expect("persist topology fixture");
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
    fn stale_runtime_evidence_degrades_deployment_and_both_binding_roles() {
        let (_directory, storage) = fixture();
        create_topology(&storage);
        for (deployment_id, service_id) in [
            ("provider-external", "judge-api"),
            ("consumer-external", "judge-worker"),
        ] {
            let node_id = if deployment_id == "consumer-external" {
                "node-b"
            } else {
                "node-a"
            };
            let external: StoredRuntimeInstance = serde_json::from_value(json!({
                "node_id": node_id,
                "instance": {
                    "deployment_id": deployment_id,
                    "service_id": service_id,
                    "release_version": "1.0.0",
                    "container_id": "",
                    "artifact_digest": format!("sha256:{}", "b".repeat(64)),
                    "desired_state": "RUNNING",
                    "observed_state": "RUNNING",
                    "health": "HEALTHY"
                },
                "management_mode": "EXTERNAL",
                "endpoint": "https://a.example",
                "external_probe_protocol": "https",
                "external_probe_health_path": "/health",
                "last_observed_at_ms": now_ms(),
                "updated_at": "unix-ms:1"
            }))
            .expect("external runtime fixture");
            storage
                .put_runtime_instance(&external)
                .expect("persist external runtime");
        }
        let mut facts = storage
            .node_runtime_facts("node-1")
            .expect("load runtime facts")
            .expect("runtime facts fixture");
        facts.received_at_ms =
            now_ms().saturating_sub(crate::durable::MANAGED_RUNTIME_REPORT_STALE_MS + 1);
        storage
            .put_node_runtime_facts(&facts)
            .expect("make runtime facts stale");

        for path in [
            "/api/v1/deployments",
            "/api/v1/deployments/deployment-1",
            "/api/v1/deployments/deployment-1/health",
        ] {
            let response = route(Some(&storage), &request("GET", path, None), "req-stale")
                .expect("deployment read route");
            assert_eq!(response.status, 200, "{}", response.body);
            let deployment = if path == "/api/v1/deployments" {
                response.body["data"]["items"]
                    .as_array()
                    .and_then(|items| {
                        items
                            .iter()
                            .find(|item| item["instance"]["deployment_id"] == "deployment-1")
                    })
                    .expect("managed deployment in list")
            } else if path.ends_with("/health") {
                &response.body["data"]
            } else {
                &response.body["data"]["deployment"]
            };
            let runtime = if path.ends_with("/health") {
                deployment
            } else {
                &deployment["instance"]
            };
            assert_eq!(runtime["observed_state"], "UNKNOWN", "{path}");
            assert_eq!(runtime["runtime_attested"], false, "{path}");
            assert!(
                deployment["drift_reason"]
                    .as_str()
                    .is_some_and(|reason| reason.contains("older than 60 seconds")),
                "{path}: {deployment}"
            );
        }

        storage
            .replace_topology_api_bindings(
                "primary",
                &[binding("deployment-1", "provider-external")],
            )
            .expect("persist consumer binding");
        let consumer = route(
            Some(&storage),
            &request("GET", "/api/v1/deployments/deployment-1/bindings", None),
            "req-consumer-binding",
        )
        .expect("consumer binding route");
        assert_eq!(consumer.body["data"]["items"][0]["health"], "DEGRADED");
        assert!(
            consumer.body["data"]["items"][0]["drift"][0]
                .as_str()
                .is_some_and(|reason| reason.contains("consumer deployment deployment-1"))
        );

        storage
            .replace_topology_api_bindings(
                "primary",
                &[binding("consumer-external", "deployment-1")],
            )
            .expect("persist provider binding");
        let provider = route(
            Some(&storage),
            &request("GET", "/api/v1/deployments/deployment-1/bindings", None),
            "req-provider-binding",
        )
        .expect("provider binding route");
        assert_eq!(
            provider.body["data"]["provider_items"][0]["health"],
            "DEGRADED"
        );
        assert!(
            provider.body["data"]["provider_items"][0]["drift"][0]
                .as_str()
                .is_some_and(|reason| reason.contains("provider deployment deployment-1"))
        );
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

    #[test]
    fn uninstall_is_rejected_while_deployment_consumes_an_active_binding() {
        let (_directory, storage) = fixture();
        create_topology(&storage);
        storage
            .replace_topology_api_bindings("primary", &[binding("deployment-1", "provider-1")])
            .expect("persist consumer binding");

        let response = route(
            Some(&storage),
            &request(
                "POST",
                "/api/v1/deployments/deployment-1:uninstall",
                Some("uninstall-consumer"),
            ),
            "req-uninstall-consumer",
        )
        .expect("deployment route");

        assert_eq!(response.status, 409, "{:?}", response.body);
        assert_eq!(response.body["code"], "DEPLOYMENT_ACTIVE_BINDINGS");
        assert!(storage.operation_store().list().unwrap().is_empty());
    }

    #[test]
    fn uninstall_is_rejected_while_deployment_provides_an_active_binding() {
        let (_directory, storage) = fixture();
        create_topology(&storage);
        storage
            .replace_topology_api_bindings("primary", &[binding("consumer-1", "deployment-1")])
            .expect("persist provider binding");

        let response = route(
            Some(&storage),
            &request(
                "POST",
                "/api/v1/deployments/deployment-1:uninstall",
                Some("uninstall-provider"),
            ),
            "req-uninstall-provider",
        )
        .expect("deployment route");

        assert_eq!(response.status, 409, "{:?}", response.body);
        assert_eq!(response.body["code"], "DEPLOYMENT_ACTIVE_BINDINGS");
        assert!(storage.operation_store().list().unwrap().is_empty());
    }
}
