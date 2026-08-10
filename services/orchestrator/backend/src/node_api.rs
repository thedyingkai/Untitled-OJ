use crate::durable::{DurableError, DurableStore};
use crate::http::{ApiRequest, ApiResponse, query_value};
use orchestrator_control_plane::{
    JobError, JobKind, OperationCoordinator, OperationError, OperationRepository, PlanOperation,
    PlannedJob,
};
use orchestrator_legacy::OrchestratorActionConsole;
use orchestrator_storage::NodeEnrollmentCode;
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::time::{SystemTime, UNIX_EPOCH};

pub(crate) fn route(
    _console: &mut OrchestratorActionConsole,
    durable_store: Option<&DurableStore>,
    request: &ApiRequest,
    request_id: &str,
) -> Option<ApiResponse> {
    let path = request.path.split('?').next().unwrap_or("/");
    let segments = path.trim_start_matches('/').split('/').collect::<Vec<_>>();
    match (request.method.as_str(), segments.as_slice()) {
        ("POST", ["api", "v1", "nodes", "enrollment-codes"]) => {
            Some(
                match create_enrollment_code(durable_store, request, request_id) {
                    Ok(response) => response,
                    Err(error) => problem(error.status, error.code, error.detail, request_id),
                },
            )
        }
        ("GET", ["api", "v1", "nodes"]) => Some(match list(durable_store, request, request_id) {
            Ok(response) => response,
            Err(error) => problem(error.status, error.code, error.detail, request_id),
        }),
        ("GET", ["api", "v1", "nodes", node_id, "health"]) => {
            Some(match health(durable_store, node_id, request_id) {
                Ok(response) => response,
                Err(error) => problem(error.status, error.code, error.detail, request_id),
            })
        }
        ("GET", ["api", "v1", "nodes", node_id]) => {
            Some(match get(durable_store, node_id, request_id) {
                Ok(response) => response,
                Err(error) => problem(error.status, error.code, error.detail, request_id),
            })
        }
        ("POST", ["api", "v1", "nodes", node_action])
            if node_action.ends_with(":revoke-certificates") =>
        {
            Some(
                match revoke_certificates(
                    durable_store,
                    node_action.trim_end_matches(":revoke-certificates"),
                    request,
                    request_id,
                ) {
                    Ok(response) => response,
                    Err(error) => problem(error.status, error.code, error.detail, request_id),
                },
            )
        }
        ("POST", ["api", "v1", "nodes", node_action]) if node_action.ends_with(":drain") => {
            let node_id = node_action.trim_end_matches(":drain");
            Some(match drain(durable_store, node_id, request, request_id) {
                Ok(response) => response,
                Err(error) => problem(error.status, error.code, error.detail, request_id),
            })
        }
        ("DELETE", ["api", "v1", "nodes", node_id]) => {
            Some(match remove(durable_store, node_id, request, request_id) {
                Ok(response) => response,
                Err(error) => problem(error.status, error.code, error.detail, request_id),
            })
        }
        _ => None,
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct EnrollmentCodeBody {
    node_id: String,
    host_ip: String,
    #[serde(default = "standalone_role")]
    role: String,
    #[serde(default)]
    parent_node_id: String,
    #[serde(default = "empty_labels")]
    labels: Value,
    #[serde(default = "default_enrollment_ttl_seconds")]
    ttl_seconds: u64,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CertificateRevocationBody {
    reason: String,
}

fn create_enrollment_code(
    durable_store: Option<&DurableStore>,
    request: &ApiRequest,
    request_id: &str,
) -> Result<ApiResponse, NodeApiError> {
    let store = durable_store.ok_or_else(storage_unavailable)?;
    let body: EnrollmentCodeBody =
        serde_json::from_str(&request.body).map_err(|error| invalid(error.to_string()))?;
    if !(60..=3_600).contains(&body.ttl_seconds) {
        return Err(invalid("ttl_seconds must be between 60 and 3600"));
    }
    if body.node_id.is_empty()
        || body.node_id.len() > 128
        || !body
            .node_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
    {
        return Err(invalid(
            "node_id must contain 1-128 ASCII letters, digits, '.', '_', or '-'",
        ));
    }
    let existing = store.get_node(&body.node_id).map_err(storage_error)?;
    if existing.as_ref().is_some_and(|node| {
        !node.status.eq_ignore_ascii_case("ENROLLMENT_PENDING")
            && !node.status.eq_ignore_ascii_case("AUTH_REVOKED")
    }) {
        return Err(NodeApiError {
            status: 409,
            code: "NODE_ALREADY_REGISTERED",
            detail: format!(
                "node {} is already registered; revoke its certificates before issuing a replacement enrollment code",
                body.node_id
            ),
        });
    }
    let now = now_ms();
    let secret = crate::node_identity::random_secret("ojos_enroll_")
        .map_err(|error| internal(error.to_string()))?;
    let code_id = crate::node_identity::random_secret("enroll-")
        .map_err(|error| internal(error.to_string()))?;
    let expires_at_ms = now.saturating_add((body.ttl_seconds as i64).saturating_mul(1_000));
    let node = orchestrator_legacy::NodeRecord {
        node_id: body.node_id.clone(),
        host_ip: body.host_ip,
        parent_node_id: body.parent_node_id,
        role: body.role,
        labels: body.labels,
        status: "ENROLLMENT_PENDING".to_string(),
        created_at: existing
            .as_ref()
            .map(|node| node.created_at.clone())
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| format!("unix-ms:{now}")),
        updated_at: format!("unix-ms:{now}"),
    };
    let code = NodeEnrollmentCode {
        code_id: code_id.clone(),
        secret_sha256: crate::node_identity::secret_digest(&secret),
        node_id: body.node_id.clone(),
        created_at_ms: now,
        expires_at_ms,
        redeemed_at_ms: None,
    };
    store
        .register_node_enrollment(&node, &code)
        .map_err(|error| NodeApiError {
            status: if matches!(error, DurableError::Invariant(_) | DurableError::Domain(_)) {
                422
            } else {
                500
            },
            code: "NODE_REGISTRATION_REJECTED",
            detail: error.to_string(),
        })?;
    Ok(success(
        201,
        json!({
            "code_id": code_id,
            "node_id": body.node_id,
            "enrollment_code": secret,
            "expires_at_ms": expires_at_ms,
        }),
        request_id,
    ))
}

fn revoke_certificates(
    durable_store: Option<&DurableStore>,
    node_id: &str,
    request: &ApiRequest,
    request_id: &str,
) -> Result<ApiResponse, NodeApiError> {
    let store = durable_store.ok_or_else(storage_unavailable)?;
    store
        .get_node(node_id)
        .map_err(storage_error)?
        .ok_or_else(|| not_found(node_id))?;
    let body: CertificateRevocationBody =
        serde_json::from_str(&request.body).map_err(|error| invalid(error.to_string()))?;
    if body.reason.trim().is_empty() || body.reason.len() > 512 {
        return Err(invalid("revocation reason must contain 1-512 characters"));
    }
    let revoked = store
        .revoke_node_certificates(node_id, now_ms(), body.reason.trim())
        .map_err(storage_error)?;
    Ok(success(
        200,
        json!({"node_id": node_id, "certificate_status": "REVOKED", "revoked_certificates": revoked}),
        request_id,
    ))
}

fn standalone_role() -> String {
    "standalone".to_string()
}

fn empty_labels() -> Value {
    json!({})
}

fn default_enrollment_ttl_seconds() -> u64 {
    600
}

fn list(
    durable_store: Option<&DurableStore>,
    request: &ApiRequest,
    request_id: &str,
) -> Result<ApiResponse, NodeApiError> {
    let store = durable_store.ok_or_else(storage_unavailable)?;
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
    let mut nodes = store.list_nodes().map_err(storage_error)?;
    nodes.sort_by(|left, right| left.node_id.cmp(&right.node_id));
    let mut items = nodes
        .into_iter()
        .filter(|node| node.node_id.as_str() > cursor.as_str())
        .take(limit + 1)
        .collect::<Vec<_>>();
    let next_cursor = if items.len() > limit {
        items.truncate(limit);
        items.last().map(|node| node.node_id.clone())
    } else {
        None
    };
    Ok(success(
        200,
        json!({"items": items, "next_cursor": next_cursor}),
        request_id,
    ))
}

fn get(
    durable_store: Option<&DurableStore>,
    node_id: &str,
    request_id: &str,
) -> Result<ApiResponse, NodeApiError> {
    let store = durable_store.ok_or_else(storage_unavailable)?;
    let node = store
        .get_node(node_id)
        .map_err(storage_error)?
        .ok_or_else(|| not_found(node_id))?;
    Ok(success(200, json!({"node": node}), request_id))
}

fn health(
    durable_store: Option<&DurableStore>,
    node_id: &str,
    request_id: &str,
) -> Result<ApiResponse, NodeApiError> {
    // Agents report every 30 seconds.  Allow one missed/jittered interval and
    // apply the Service Contract v2 rule that observations older than 60s are
    // unavailable.
    const AGENT_FRESHNESS_MS: i64 = 60_000;

    let store = durable_store.ok_or_else(storage_unavailable)?;
    let node = store
        .get_node(node_id)
        .map_err(storage_error)?
        .ok_or_else(|| not_found(node_id))?;
    let active_jobs = store
        .job_store()
        .active_job_count(node_id)
        .map_err(job_error)?;
    let deployments = store
        .runtime_instances(Some(node_id))
        .map_err(storage_error)?;
    let running_deployments = deployments
        .iter()
        .filter(|deployment| {
            matches!(
                deployment.instance.observed_state,
                orchestrator_runtime::RuntimeObservedState::Running
            )
        })
        .count();
    let unhealthy_deployments = deployments
        .iter()
        .filter(|deployment| {
            matches!(
                deployment.instance.observed_state,
                orchestrator_runtime::RuntimeObservedState::Running
            ) && !deployment.instance.health.eq_ignore_ascii_case("healthy")
        })
        .count();
    // NodeRecord.updated_at also changes during enrollment, drain, revocation,
    // and other control-plane mutations.  It therefore cannot prove that an
    // Agent is reachable.  Only an authenticated runtime report is an Agent
    // observation, and received_at_ms is control-plane time so it is not
    // vulnerable to Node clock skew.
    let runtime_facts = store.node_runtime_facts(node_id).map_err(storage_error)?;
    let observed_at_ms = runtime_facts.as_ref().map(|facts| facts.received_at_ms);
    let observation_age_ms = observed_at_ms.map(|value| now_ms().saturating_sub(value).max(0));
    let agent_reachable = observation_age_ms.is_some_and(|age| age <= AGENT_FRESHNESS_MS);
    let accepting_jobs = node.status.eq_ignore_ascii_case("READY");
    let ready = accepting_jobs && agent_reachable && unhealthy_deployments == 0;
    Ok(success(
        200,
        json!({
            "node_id": node.node_id,
            "status": node.status,
            "ready": ready,
            "accepting_jobs": accepting_jobs,
            "agent_reachable": agent_reachable,
            "last_observed_at": observed_at_ms.map(|value| format!("unix-ms:{value}")),
            "observation_age_ms": observation_age_ms,
            "freshness_threshold_ms": AGENT_FRESHNESS_MS,
            "active_jobs": active_jobs,
            "deployments": deployments.len(),
            "running_deployments": running_deployments,
            "unhealthy_deployments": unhealthy_deployments,
        }),
        request_id,
    ))
}

fn drain(
    durable_store: Option<&DurableStore>,
    node_id: &str,
    request: &ApiRequest,
    request_id: &str,
) -> Result<ApiResponse, NodeApiError> {
    let store = durable_store.ok_or_else(storage_unavailable)?;
    let node = store
        .get_node(node_id)
        .map_err(storage_error)?
        .ok_or_else(|| not_found(node_id))?;
    if !matches!(
        node.status.to_ascii_uppercase().as_str(),
        "READY" | "DRAINING" | "DRAINED"
    ) {
        return Err(NodeApiError {
            status: 409,
            code: "NODE_STATE_CONFLICT",
            detail: format!(
                "node {node_id} is {}; only READY, DRAINING, or DRAINED can be drained",
                node.status
            ),
        });
    }
    let active_jobs = store
        .job_store()
        .active_job_count(node_id)
        .map_err(job_error)?;
    let runtime_instances = store
        .runtime_instances(Some(node_id))
        .map_err(storage_error)?;
    if active_jobs != 0 || !runtime_instances.is_empty() {
        return Err(NodeApiError {
            status: 409,
            code: "NODE_NOT_EMPTY",
            detail: format!(
                "node {node_id} owns {active_jobs} active jobs and {} runtime instances; cancel/finish jobs and uninstall deployments before drain",
                runtime_instances.len()
            ),
        });
    }
    enqueue_lifecycle(
        store,
        node_id,
        "node.drain",
        JobKind::NodeDrain,
        request,
        request_id,
    )
}

fn remove(
    durable_store: Option<&DurableStore>,
    node_id: &str,
    request: &ApiRequest,
    request_id: &str,
) -> Result<ApiResponse, NodeApiError> {
    let store = durable_store.ok_or_else(storage_unavailable)?;
    let node = store
        .get_node(node_id)
        .map_err(storage_error)?
        .ok_or_else(|| not_found(node_id))?;
    if !node.status.eq_ignore_ascii_case("DRAINED") {
        return Err(NodeApiError {
            status: 409,
            code: "NODE_NOT_DRAINED",
            detail: format!("node {node_id} must be DRAINED before removal"),
        });
    }
    let active_jobs = store
        .job_store()
        .active_job_count(node_id)
        .map_err(job_error)?;
    let runtime_instances = store
        .runtime_instances(Some(node_id))
        .map_err(storage_error)?;
    if active_jobs != 0 || !runtime_instances.is_empty() {
        return Err(NodeApiError {
            status: 409,
            code: "NODE_NOT_EMPTY",
            detail: format!(
                "node {node_id} still owns {active_jobs} active jobs and {} runtime instances",
                runtime_instances.len()
            ),
        });
    }
    enqueue_lifecycle(
        store,
        node_id,
        "node.remove",
        JobKind::NodeRemove,
        request,
        request_id,
    )
}

fn enqueue_lifecycle(
    store: &DurableStore,
    node_id: &str,
    action: &str,
    kind: JobKind,
    request: &ApiRequest,
    request_id: &str,
) -> Result<ApiResponse, NodeApiError> {
    if store
        .operation_store()
        .list()
        .map_err(operation_store_error)?
        .into_iter()
        .any(|operation| {
            operation.target_type == "Node"
                && operation.target_id == node_id
                && !operation.status.is_terminal()
        })
    {
        return Err(NodeApiError {
            status: 409,
            code: "NODE_OPERATION_IN_PROGRESS",
            detail: format!("node {node_id} already has an active lifecycle Operation"),
        });
    }
    let operation_id = operation_id(action.trim_start_matches("node."), node_id, request);
    let plan = PlanOperation {
        operation_id,
        action: action.to_string(),
        target_type: "Node".to_string(),
        target_id: node_id.to_string(),
        request: json!({"node_id": node_id, "auto_enqueue": true}),
        jobs: vec![PlannedJob {
            step_id: "node-lifecycle".to_string(),
            node_id: "control-plane".to_string(),
            kind,
            depends_on: vec![],
            condition: Default::default(),
            payload: json!({"node_id": node_id}),
            // A crash after the node transaction but before Job completion is
            // outcome-unknown and must require an explicit retry decision.
            max_attempts: 1,
        }],
    };
    let now = now_ms();
    let mut operations = store.operation_store();
    let mut jobs = store.job_store();
    let mut coordinator = OperationCoordinator::new(&mut operations, &mut jobs);
    let planned = coordinator.plan(plan, now).map_err(operation_error)?;
    let confirmed = coordinator
        .confirm(&planned.operation_id, now)
        .map_err(operation_error)?;
    let operation = coordinator
        .enqueue(&confirmed.operation_id, now)
        .map_err(operation_error)?;
    Ok(success(
        202,
        json!({"operation_id": operation.operation_id, "operation": operation}),
        request_id,
    ))
}

fn operation_id(prefix: &str, node_id: &str, request: &ApiRequest) -> String {
    let key = request
        .headers
        .get("idempotency-key")
        .map(String::as_str)
        .unwrap_or_default();
    let digest = Sha256::digest(format!("{prefix}\0{node_id}\0{key}").as_bytes());
    format!("op-node-{prefix}-{:x}", digest)[..56].to_string()
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
    let response = match status {
        201 => ApiResponse::created(body),
        202 => ApiResponse::accepted(body),
        _ => ApiResponse::ok(body),
    };
    response.with_header("X-Request-ID", request_id)
}

fn problem(
    status: u16,
    code: &'static str,
    detail: impl Into<String>,
    request_id: &str,
) -> ApiResponse {
    ApiResponse::problem(status, code, detail, request_id, None)
        .with_header("X-Request-ID", request_id)
}

#[derive(Debug)]
struct NodeApiError {
    status: u16,
    code: &'static str,
    detail: String,
}

fn not_found(node_id: &str) -> NodeApiError {
    NodeApiError {
        status: 404,
        code: "NODE_NOT_FOUND",
        detail: format!("node {node_id} was not found"),
    }
}

fn storage_unavailable() -> NodeApiError {
    NodeApiError {
        status: 503,
        code: "NODE_STORAGE_UNAVAILABLE",
        detail: "node lifecycle requires durable control-plane storage".to_string(),
    }
}

fn invalid(detail: impl Into<String>) -> NodeApiError {
    NodeApiError {
        status: 422,
        code: "NODE_QUERY_INVALID",
        detail: detail.into(),
    }
}

fn internal(detail: impl Into<String>) -> NodeApiError {
    NodeApiError {
        status: 500,
        code: "NODE_IDENTITY_ERROR",
        detail: detail.into(),
    }
}

fn storage_error(error: DurableError) -> NodeApiError {
    NodeApiError {
        status: 500,
        code: "NODE_STORAGE_ERROR",
        detail: error.to_string(),
    }
}

fn operation_error(error: OperationError) -> NodeApiError {
    NodeApiError {
        status: 409,
        code: "NODE_OPERATION_REJECTED",
        detail: error.to_string(),
    }
}

fn operation_store_error(error: orchestrator_control_plane::OperationStoreError) -> NodeApiError {
    NodeApiError {
        status: 500,
        code: "NODE_OPERATION_STORE_ERROR",
        detail: error.to_string(),
    }
}

fn job_error(error: JobError) -> NodeApiError {
    NodeApiError {
        status: 500,
        code: "NODE_JOB_STATE_ERROR",
        detail: error.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use orchestrator_control_plane::{JobStatus, JobStore};
    use orchestrator_legacy::NodeRecord;
    use orchestrator_storage::{
        CERTIFICATE_LIFETIME_MS, EnrollmentRedemption, NewNodeCertificate, SqliteOrchestratorStore,
        StoredNodeRuntimeFacts,
    };
    use std::collections::BTreeMap;
    use std::path::PathBuf;

    fn fixture() -> (tempfile::TempDir, DurableStore, OrchestratorActionConsole) {
        let directory = tempfile::tempdir().unwrap();
        let sqlite = SqliteOrchestratorStore::open(directory.path().join("orchestrator.db"))
            .expect("open durable store");
        let durable = DurableStore::Sqlite(sqlite);
        durable
            .upsert_node(NodeRecord {
                node_id: "node-1".to_string(),
                host_ip: "127.0.0.2".to_string(),
                parent_node_id: String::new(),
                role: "standalone".to_string(),
                labels: json!({}),
                status: "READY".to_string(),
                created_at: "unix-ms:1".to_string(),
                updated_at: "unix-ms:1".to_string(),
            })
            .unwrap();
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(|path| path.parent())
            .and_then(|path| path.parent())
            .unwrap()
            .to_path_buf();
        let console = OrchestratorActionConsole::load_with_database_url(root, None).unwrap();
        (directory, durable, console)
    }

    fn request(method: &str, path: &str, key: &str) -> ApiRequest {
        ApiRequest {
            method: method.to_string(),
            path: path.to_string(),
            headers: BTreeMap::from([("idempotency-key".to_string(), key.to_string())]),
            body: "{}".to_string(),
        }
    }

    #[test]
    fn drain_returns_a_real_durable_control_plane_operation() {
        let (_directory, durable, mut console) = fixture();
        let response = route(
            &mut console,
            Some(&durable),
            &request("POST", "/api/v1/nodes/node-1:drain", "drain-key-1"),
            "req-drain",
        )
        .expect("node route");
        assert_eq!(response.status, 202, "{:?}", response.body);
        let operation_id = response.body["data"]["operation_id"].as_str().unwrap();
        let operation = durable
            .operation_store()
            .get(operation_id)
            .unwrap()
            .unwrap();
        assert_eq!(operation.action, "node.drain");
        assert_eq!(operation.job_bindings.len(), 1);
        let job = durable
            .job_store()
            .get(&operation.job_bindings[0].job_id)
            .unwrap()
            .unwrap();
        assert_eq!(job.kind, JobKind::NodeDrain);
        assert_eq!(job.node_id, "control-plane");
        assert_eq!(job.status, JobStatus::Queued);
        assert_eq!(durable.get_node("node-1").unwrap().unwrap().status, "READY");
    }

    #[test]
    fn registration_returns_one_time_secret_and_persists_only_its_digest() {
        let (_directory, durable, mut console) = fixture();
        let mut request = request("POST", "/api/v1/nodes/enrollment-codes", "enrollment-key-1");
        request.body = json!({
            "node_id": "node-2",
            "host_ip": "127.0.0.3",
            "role": "standalone",
            "labels": {"runtime": "docker"},
            "ttl_seconds": 600
        })
        .to_string();
        let response =
            route(&mut console, Some(&durable), &request, "req-enrollment").expect("node route");
        assert_eq!(response.status, 201, "{:?}", response.body);
        let secret = response.body["data"]["enrollment_code"].as_str().unwrap();
        assert!(secret.starts_with("ojos_enroll_"));
        let stored = durable
            .node_enrollment_code_by_digest(&crate::node_identity::secret_digest(secret))
            .unwrap()
            .unwrap();
        assert_eq!(stored.node_id, "node-2");
        assert_ne!(stored.secret_sha256, secret);
        assert_eq!(
            durable.get_node("node-2").unwrap().unwrap().status,
            "ENROLLMENT_PENDING"
        );
    }

    #[test]
    fn administrator_revocation_immediately_disables_node_identity() {
        let (_directory, durable, mut console) = fixture();
        let mut request = request(
            "POST",
            "/api/v1/nodes/node-1:revoke-certificates",
            "revoke-key-1",
        );
        request.body = json!({"reason": "node decommissioned"}).to_string();
        let response =
            route(&mut console, Some(&durable), &request, "req-revoke").expect("node route");
        assert_eq!(response.status, 200, "{:?}", response.body);
        assert_eq!(response.body["data"]["certificate_status"], "REVOKED");
        assert_eq!(
            durable.get_node("node-1").unwrap().unwrap().status,
            "AUTH_REVOKED"
        );
    }

    #[test]
    fn revoked_node_can_reenroll_without_consuming_a_second_node_slot() {
        let (_directory, durable, mut console) = fixture();
        let original_created_at = durable.get_node("node-1").unwrap().unwrap().created_at;

        let mut revoke = request(
            "POST",
            "/api/v1/nodes/node-1:revoke-certificates",
            "revoke-key-reenroll",
        );
        revoke.body = json!({"reason": "replace compromised identity"}).to_string();
        assert_eq!(
            route(&mut console, Some(&durable), &revoke, "req-revoke")
                .expect("node revoke route")
                .status,
            200
        );
        assert_eq!(
            durable.get_node("node-1").unwrap().unwrap().status,
            "AUTH_REVOKED"
        );

        let mut enroll = request("POST", "/api/v1/nodes/enrollment-codes", "reenroll-key-1");
        enroll.body = json!({
            "node_id": "node-1",
            "host_ip": "127.0.0.22",
            "role": "standalone",
            "labels": {"runtime": "docker"},
            "ttl_seconds": 600
        })
        .to_string();
        let response = route(&mut console, Some(&durable), &enroll, "req-reenroll")
            .expect("node enrollment route");
        assert_eq!(response.status, 201, "{:?}", response.body);
        let secret = response.body["data"]["enrollment_code"]
            .as_str()
            .expect("one-time enrollment secret");
        let pending = durable.get_node("node-1").unwrap().unwrap();
        assert_eq!(pending.status, "ENROLLMENT_PENDING");
        assert_eq!(pending.created_at, original_created_at);
        assert_eq!(durable.list_nodes().unwrap().len(), 1);

        let issued_at = now_ms();
        let redemption = durable
            .redeem_node_enrollment_code(
                &crate::node_identity::secret_digest(secret),
                &format!("sha256:{}", "a".repeat(64)),
                issued_at,
                NewNodeCertificate {
                    serial_hex: "reenrolled-serial".to_string(),
                    node_id: "node-1".to_string(),
                    spiffe_id: "spiffe://ojos.local/node/node-1".to_string(),
                    certificate_pem:
                        "-----BEGIN CERTIFICATE-----\nreenrolled\n-----END CERTIFICATE-----"
                            .to_string(),
                    fingerprint_sha256: "sha256:reenrolled".to_string(),
                    issued_at_ms: issued_at,
                    not_before_ms: issued_at,
                    not_after_ms: issued_at + CERTIFICATE_LIFETIME_MS,
                },
            )
            .unwrap();
        assert!(matches!(redemption, EnrollmentRedemption::Redeemed(_)));
        assert_eq!(durable.get_node("node-1").unwrap().unwrap().status, "READY");
        assert_eq!(durable.list_nodes().unwrap().len(), 1);
    }

    #[test]
    fn ready_node_must_be_revoked_before_reenrollment() {
        let (_directory, durable, mut console) = fixture();
        let mut enroll = request(
            "POST",
            "/api/v1/nodes/enrollment-codes",
            "parallel-enroll-key-1",
        );
        enroll.body = json!({
            "node_id": "node-1",
            "host_ip": "127.0.0.2",
            "ttl_seconds": 600
        })
        .to_string();
        let response = route(&mut console, Some(&durable), &enroll, "req-parallel-enroll")
            .expect("node enrollment route");
        assert_eq!(response.status, 409);
        assert_eq!(response.body["code"], "NODE_ALREADY_REGISTERED");
        assert_eq!(durable.get_node("node-1").unwrap().unwrap().status, "READY");
    }

    #[test]
    fn remove_rejects_a_node_that_has_not_been_drained_without_creating_an_operation() {
        let (_directory, durable, mut console) = fixture();
        let response = route(
            &mut console,
            Some(&durable),
            &request("DELETE", "/api/v1/nodes/node-1", "remove-key-1"),
            "req-remove",
        )
        .expect("node route");
        assert_eq!(response.status, 409);
        assert_eq!(response.body["code"], "NODE_NOT_DRAINED");
        assert!(durable.operation_store().list().unwrap().is_empty());
    }

    #[test]
    fn list_get_and_health_are_backed_by_durable_state() {
        let (_directory, durable, mut console) = fixture();
        let list_response = route(
            &mut console,
            Some(&durable),
            &request("GET", "/api/v1/nodes?limit=1", ""),
            "req-list",
        )
        .expect("node route");
        assert_eq!(list_response.status, 200);
        assert_eq!(list_response.body["data"]["items"][0]["node_id"], "node-1");

        let get_response = route(
            &mut console,
            Some(&durable),
            &request("GET", "/api/v1/nodes/node-1", ""),
            "req-get",
        )
        .expect("node route");
        assert_eq!(get_response.status, 200);
        assert_eq!(get_response.body["data"]["node"]["status"], "READY");

        let health_response = route(
            &mut console,
            Some(&durable),
            &request("GET", "/api/v1/nodes/node-1/health", ""),
            "req-health",
        )
        .expect("node route");
        assert_eq!(health_response.status, 200);
        assert_eq!(health_response.body["data"]["active_jobs"], 0);
        assert_eq!(health_response.body["data"]["deployments"], 0);
        assert_eq!(health_response.body["data"]["agent_reachable"], false);
        assert_eq!(health_response.body["data"]["ready"], false);
    }

    #[test]
    fn only_an_authenticated_runtime_report_proves_agent_reachability() {
        let (_directory, durable, mut console) = fixture();
        let received_at_ms = now_ms();
        let mut node = durable.get_node("node-1").unwrap().unwrap();
        node.updated_at = format!("unix-ms:{received_at_ms}");
        durable.upsert_node(node).unwrap();

        let before_report = route(
            &mut console,
            Some(&durable),
            &request("GET", "/api/v1/nodes/node-1/health", ""),
            "req-health-before-report",
        )
        .expect("node route");
        assert_eq!(before_report.body["data"]["agent_reachable"], false);
        assert_eq!(before_report.body["data"]["ready"], false);
        assert_eq!(before_report.body["data"]["last_observed_at"], Value::Null);

        durable
            .put_node_runtime_facts(&StoredNodeRuntimeFacts {
                node_id: "node-1".to_string(),
                observed_at_ms: received_at_ms,
                received_at_ms,
                facts: json!({"report_id": "report-node-1"}),
            })
            .unwrap();
        let after_report = route(
            &mut console,
            Some(&durable),
            &request("GET", "/api/v1/nodes/node-1/health", ""),
            "req-health-after-report",
        )
        .expect("node route");
        assert_eq!(after_report.body["data"]["agent_reachable"], true);
        assert_eq!(after_report.body["data"]["ready"], true);
        assert_eq!(
            after_report.body["data"]["last_observed_at"],
            format!("unix-ms:{received_at_ms}")
        );

        durable
            .put_node_runtime_facts(&StoredNodeRuntimeFacts {
                node_id: "node-1".to_string(),
                observed_at_ms: received_at_ms - 60_001,
                received_at_ms: received_at_ms - 60_001,
                facts: json!({"report_id": "stale-report-node-1"}),
            })
            .unwrap();
        let stale_report = route(
            &mut console,
            Some(&durable),
            &request("GET", "/api/v1/nodes/node-1/health", ""),
            "req-health-stale-report",
        )
        .expect("node route");
        assert_eq!(stale_report.body["data"]["agent_reachable"], false);
        assert_eq!(stale_report.body["data"]["ready"], false);
    }
}
