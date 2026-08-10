use crate::durable::DurableStore;
use crate::http::{ApiRequest, ApiResponse, query_value};
use orchestrator_control_plane::{
    DurableOperation, JobKind, JobStore, OperationCoordinator, OperationError, OperationRepository,
    PlanOperation, PlannedJob,
};
use orchestrator_legacy::v1_action;
use orchestrator_runtime::ReleaseReplacementPayload;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::time::{SystemTime, UNIX_EPOCH};

const MAX_SSE_EVENTS_PER_RESPONSE: usize = 500;
const MAX_LAST_EVENT_ID_BYTES: usize = 16 * 1024;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ActionPlanRequest {
    action: String,
    fields: Map<String, Value>,
}

pub(crate) fn route(
    storage: Option<&DurableStore>,
    request: &ApiRequest,
    request_id: &str,
) -> Option<ApiResponse> {
    let path = request.path.split('?').next().unwrap_or("/");
    let segments = path.trim_start_matches('/').split('/').collect::<Vec<_>>();
    let operation_route = matches!(segments.get(0..3), Some(["api", "v1", "operations"]))
        || path == "/api/v1/operations:plan";
    if !operation_route {
        return None;
    }
    let Some(storage) = storage else {
        return Some(problem(
            503,
            "OPERATION_STORAGE_UNAVAILABLE",
            "durable Operation storage is unavailable",
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
) -> Result<ApiResponse, OperationApiError> {
    match (request.method.as_str(), segments) {
        ("POST", ["api", "v1", "operations:plan"]) => {
            let plan = parse_plan(request)?;
            let mut operations = storage.operation_store();
            let mut jobs = storage.job_store();
            let operation = OperationCoordinator::new(&mut operations, &mut jobs)
                .plan(plan, now_ms())
                .map_err(operation_error)?;
            Ok(success(201, json!({"operation": operation}), request_id))
        }
        ("GET", ["api", "v1", "operations"]) => {
            let (cursor, limit) = page_request(request)?;
            let mut operations = storage.operation_store().list().map_err(store_error)?;
            operations.sort_by(|left, right| left.operation_id.cmp(&right.operation_id));
            let mut operations = operations
                .into_iter()
                .filter(|operation| operation.operation_id.as_str() > cursor.as_str())
                .take(limit + 1)
                .collect::<Vec<_>>();
            let next_cursor = if operations.len() > limit {
                operations.truncate(limit);
                operations
                    .last()
                    .map(|operation| operation.operation_id.clone())
            } else {
                None
            };
            Ok(success(
                200,
                json!({"items": operations, "next_cursor": next_cursor}),
                request_id,
            ))
        }
        ("GET", ["api", "v1", "operations", operation_id]) => {
            let operation = storage
                .operation_store()
                .get(operation_id)
                .map_err(store_error)?
                .ok_or_else(|| not_found(operation_id))?;
            Ok(success(200, json!({"operation": operation}), request_id))
        }
        ("GET", ["api", "v1", "operations", operation_id, "logs"]) => {
            let operations = storage.operation_store();
            let operation = operations
                .get(operation_id)
                .map_err(store_error)?
                .ok_or_else(|| not_found(operation_id))?;
            let query = request
                .path
                .split_once('?')
                .map(|(_, query)| query)
                .unwrap_or("");
            let mut cursor = decode_event_cursor(
                query_value(query, "cursor")
                    .map_err(|_| invalid_log_cursor())?
                    .as_deref()
                    .unwrap_or_default(),
            )
            .map_err(|_| invalid_log_cursor())?;
            let (_, limit) = page_request(request)?;
            let jobs = storage.job_store();
            let mut events = Vec::new();
            for binding in &operation.job_bindings {
                let after = cursor
                    .job_sequences
                    .get(&binding.job_id)
                    .copied()
                    .unwrap_or_default();
                events.extend(jobs.events(&binding.job_id, after).map_err(job_error)?);
            }
            events.sort_by(|left, right| {
                (left.created_at_ms, left.job_id.as_str(), left.sequence).cmp(&(
                    right.created_at_ms,
                    right.job_id.as_str(),
                    right.sequence,
                ))
            });
            let has_more = events.len() > limit;
            events.truncate(limit);
            for event in &events {
                cursor
                    .job_sequences
                    .insert(event.job_id.clone(), event.sequence);
            }
            let next_cursor = has_more.then(|| encode_event_cursor(&cursor)).transpose()?;
            Ok(success(
                200,
                json!({"items": events, "next_cursor": next_cursor}),
                request_id,
            ))
        }
        ("GET", ["api", "v1", "operations", operation_id, "events"]) => {
            operation_events(storage, operation_id, request, request_id)
        }
        ("POST", ["api", "v1", "operations", operation_action]) => {
            let (operation_id, action) =
                operation_action
                    .rsplit_once(':')
                    .ok_or_else(|| OperationApiError {
                        status: 404,
                        code: "ROUTE_NOT_FOUND",
                        detail: "operation action route is malformed".to_string(),
                        operation_id: None,
                    })?;
            let mut operations = storage.operation_store();
            let mut jobs = storage.job_store();
            let rollback_source = if action == "rollback" {
                Some(
                    operations
                        .get(operation_id)
                        .map_err(store_error)?
                        .ok_or_else(|| not_found(operation_id))?,
                )
            } else {
                None
            };
            let mut coordinator = OperationCoordinator::new(&mut operations, &mut jobs);
            let operation = match action {
                "confirm" => coordinator
                    .confirm(operation_id, now_ms())
                    .map_err(operation_error),
                "apply" => coordinator
                    .enqueue(operation_id, now_ms())
                    .map_err(operation_error),
                "cancel" => coordinator
                    .cancel(operation_id, now_ms())
                    .map_err(operation_error),
                "retry" => coordinator
                    .retry(operation_id, now_ms())
                    .map_err(operation_error),
                "rollback" => {
                    let rollback_plan =
                        if request.body.trim().is_empty() || request.body.trim() == "{}" {
                            derive_rollback_plan(
                                rollback_source
                                    .as_ref()
                                    .expect("rollback source loaded before coordinator"),
                                request,
                            )?
                        } else {
                            serde_json::from_str::<PlanOperation>(&request.body).map_err(|error| {
                            OperationApiError {
                                status: 422,
                                code: "ROLLBACK_PLAN_REQUIRED",
                                detail: format!(
                                    "rollback requires an executable PlanOperation body: {error}"
                                ),
                                operation_id: Some(operation_id.to_string()),
                            }
                        })?
                        };
                    let rollback = coordinator
                        .rollback(operation_id, rollback_plan, now_ms())
                        .map_err(operation_error)?;
                    coordinator
                        .confirm(&rollback.operation_id, now_ms())
                        .map_err(operation_error)?;
                    coordinator
                        .enqueue(&rollback.operation_id, now_ms())
                        .map_err(operation_error)
                }
                _ => {
                    return Err(OperationApiError {
                        status: 404,
                        code: "ROUTE_NOT_FOUND",
                        detail: format!("unknown operation action {action}"),
                        operation_id: Some(operation_id.to_string()),
                    });
                }
            }?;
            let status = if matches!(action, "apply" | "cancel" | "retry" | "rollback") {
                202
            } else {
                200
            };
            let data = if status == 202 {
                let operation_id = operation.operation_id.clone();
                json!({
                    "operation_id": operation_id,
                    "operation": operation,
                })
            } else {
                json!({"operation": operation})
            };
            Ok(success(status, data, request_id))
        }
        _ => Err(OperationApiError {
            status: 404,
            code: "ROUTE_NOT_FOUND",
            detail: "the requested Operation v1 route does not exist".to_string(),
            operation_id: None,
        }),
    }
}

fn page_request(request: &ApiRequest) -> Result<(String, usize), OperationApiError> {
    let query = request
        .path
        .split_once('?')
        .map(|(_, query)| query)
        .unwrap_or("");
    let cursor = query_value(query, "cursor")
        .map_err(|error| invalid_page(error.to_string()))?
        .unwrap_or_default();
    let limit = query_value(query, "limit")
        .map_err(|error| invalid_page(error.to_string()))?
        .map(|value| value.parse::<usize>())
        .transpose()
        .map_err(|_| invalid_page("limit must be an integer"))?
        .unwrap_or(50);
    if !(1..=200).contains(&limit) {
        return Err(invalid_page("limit must be between 1 and 200"));
    }
    Ok((cursor, limit))
}

fn invalid_page(detail: impl Into<String>) -> OperationApiError {
    OperationApiError {
        status: 422,
        code: "OPERATION_PAGE_INVALID",
        detail: detail.into(),
        operation_id: None,
    }
}

fn invalid_log_cursor() -> OperationApiError {
    OperationApiError {
        status: 422,
        code: "OPERATION_LOG_CURSOR_INVALID",
        detail: "cursor is not a valid Operation log cursor".to_string(),
        operation_id: None,
    }
}

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct EventCursor {
    #[serde(default)]
    operation_revision: u64,
    #[serde(default)]
    job_sequences: BTreeMap<String, u64>,
}

fn operation_events(
    storage: &DurableStore,
    operation_id: &str,
    request: &ApiRequest,
    request_id: &str,
) -> Result<ApiResponse, OperationApiError> {
    let operation = storage
        .operation_store()
        .get(operation_id)
        .map_err(store_error)?
        .ok_or_else(|| not_found(operation_id))?;
    let mut cursor = decode_event_cursor(
        request
            .headers
            .get("last-event-id")
            .map(String::as_str)
            .unwrap_or_default(),
    )?;
    let jobs = storage.job_store();
    let mut pending = Vec::new();
    for binding in &operation.job_bindings {
        let after = cursor
            .job_sequences
            .get(&binding.job_id)
            .copied()
            .unwrap_or_default();
        pending.extend(jobs.events(&binding.job_id, after).map_err(job_error)?);
    }
    pending.sort_by(|left, right| {
        (left.created_at_ms, left.job_id.as_str(), left.sequence).cmp(&(
            right.created_at_ms,
            right.job_id.as_str(),
            right.sequence,
        ))
    });
    pending.truncate(MAX_SSE_EVENTS_PER_RESPONSE);

    let mut body = String::new();
    for event in pending {
        cursor
            .job_sequences
            .insert(event.job_id.clone(), event.sequence);
        append_sse_event(
            &mut body,
            &encode_event_cursor(&cursor)?,
            "job",
            &json!({"request_id": request_id, "event": event}),
        )?;
    }
    if operation.revision > cursor.operation_revision {
        cursor.operation_revision = operation.revision;
        append_sse_event(
            &mut body,
            &encode_event_cursor(&cursor)?,
            "operation",
            &json!({"request_id": request_id, "operation": operation}),
        )?;
    }
    if body.is_empty() {
        body.push_str(": keep-alive\nretry: 1000\n\n");
    } else {
        body.push_str("retry: 1000\n\n");
    }
    Ok(ApiResponse::event_stream(body)
        .with_header("Cache-Control", "no-cache, no-transform")
        .with_header("X-Accel-Buffering", "no")
        .with_header("X-Request-ID", request_id))
}

fn append_sse_event(
    output: &mut String,
    id: &str,
    event_type: &str,
    data: &Value,
) -> Result<(), OperationApiError> {
    let data = serde_json::to_string(data).map_err(|error| OperationApiError {
        status: 500,
        code: "OPERATION_EVENT_ENCODING_ERROR",
        detail: error.to_string(),
        operation_id: None,
    })?;
    output.push_str("id: ");
    output.push_str(id);
    output.push_str("\nevent: ");
    output.push_str(event_type);
    output.push_str("\ndata: ");
    output.push_str(&data);
    output.push_str("\n\n");
    Ok(())
}

fn encode_event_cursor(cursor: &EventCursor) -> Result<String, OperationApiError> {
    let bytes = serde_json::to_vec(cursor).map_err(|error| OperationApiError {
        status: 500,
        code: "OPERATION_EVENT_ENCODING_ERROR",
        detail: error.to_string(),
        operation_id: None,
    })?;
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut encoded = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        encoded.push(char::from(HEX[(byte >> 4) as usize]));
        encoded.push(char::from(HEX[(byte & 0x0f) as usize]));
    }
    Ok(encoded)
}

fn decode_event_cursor(value: &str) -> Result<EventCursor, OperationApiError> {
    if value.is_empty() {
        return Ok(EventCursor::default());
    }
    if value.len() > MAX_LAST_EVENT_ID_BYTES || !value.len().is_multiple_of(2) {
        return Err(invalid_last_event_id());
    }
    let bytes = value
        .as_bytes()
        .chunks_exact(2)
        .map(|pair| {
            let pair = std::str::from_utf8(pair).map_err(|_| invalid_last_event_id())?;
            u8::from_str_radix(pair, 16).map_err(|_| invalid_last_event_id())
        })
        .collect::<Result<Vec<_>, _>>()?;
    serde_json::from_slice(&bytes).map_err(|_| invalid_last_event_id())
}

fn invalid_last_event_id() -> OperationApiError {
    OperationApiError {
        status: 400,
        code: "INVALID_LAST_EVENT_ID",
        detail: "Last-Event-ID is not a valid Operation event cursor".to_string(),
        operation_id: None,
    }
}

fn derive_rollback_plan(
    source: &DurableOperation,
    request: &ApiRequest,
) -> Result<PlanOperation, OperationApiError> {
    let mut jobs = Vec::with_capacity(source.planned_jobs.len());
    for planned in source.planned_jobs.iter().rev() {
        let kind = match planned.kind {
            JobKind::Install => JobKind::Uninstall,
            JobKind::Start => JobKind::Stop,
            JobKind::Stop => JobKind::Start,
            JobKind::Restart => JobKind::Restart,
            JobKind::Upgrade
            | JobKind::ReleasePipeline
            | JobKind::Rollback
            | JobKind::Uninstall
            | JobKind::Health
            | JobKind::BindingContextApply
            | JobKind::Inventory
            | JobKind::TopologyApply
            | JobKind::ExternalHealth
            | JobKind::NodeDrain
            | JobKind::NodeRemove => {
                return Err(OperationApiError {
                    status: 422,
                    code: "ROLLBACK_NOT_AVAILABLE",
                    detail: format!(
                        "operation {} contains {:?}, whose prior side-effect state cannot be proven from the persisted plan",
                        source.operation_id, planned.kind
                    ),
                    operation_id: Some(source.operation_id.clone()),
                });
            }
        };
        jobs.push(PlannedJob {
            step_id: format!("rollback-{}", planned.step_id),
            node_id: planned.node_id.clone(),
            kind,
            depends_on: planned.depends_on.clone(),
            condition: planned.condition,
            payload: planned.payload.clone(),
            max_attempts: planned.max_attempts,
        });
    }
    let idempotency_key = request
        .headers
        .get("idempotency-key")
        .map(String::as_str)
        .unwrap_or_default();
    let digest =
        Sha256::digest(format!("rollback\0{}\0{idempotency_key}", source.operation_id).as_bytes());
    Ok(PlanOperation {
        operation_id: format!("op-rollback-{digest:x}"),
        action: "operation.rollback".to_string(),
        target_type: source.target_type.clone(),
        target_id: source.target_id.clone(),
        request: json!({
            "rollback_of_operation_id": source.operation_id,
            "source_action": source.action,
        }),
        jobs,
    })
}

fn parse_plan(request: &ApiRequest) -> Result<PlanOperation, OperationApiError> {
    if let Ok(plan) = serde_json::from_str::<PlanOperation>(&request.body) {
        if plan.request.get("auto_enqueue").is_some() {
            return Err(invalid(
                "request.auto_enqueue is reserved for control-plane workflows",
            ));
        }
        return Ok(plan);
    }
    let input: ActionPlanRequest = serde_json::from_str(&request.body).map_err(json_error)?;
    if input.fields.contains_key("auto_enqueue") {
        return Err(invalid(
            "fields.auto_enqueue is reserved for control-plane workflows",
        ));
    }
    let descriptor = v1_action(&input.action).ok_or_else(|| OperationApiError {
        status: 422,
        code: "ACTION_NOT_PUBLISHED",
        detail: format!("{} is not part of the v1 action contract", input.action),
        operation_id: None,
    })?;
    let kind = match input.action.as_str() {
        "release.install" => JobKind::Install,
        "release.upgrade" => JobKind::Upgrade,
        "release.rollback" => JobKind::Rollback,
        "deployment.start" => JobKind::Start,
        "deployment.stop" => JobKind::Stop,
        "deployment.restart" => JobKind::Restart,
        "deployment.uninstall" => JobKind::Uninstall,
        "deployment.health" => JobKind::Health,
        "node.drain" => JobKind::NodeDrain,
        "node.remove" => JobKind::NodeRemove,
        _ => {
            return Err(OperationApiError {
                status: 422,
                code: "ACTION_HAS_NO_JOB_PLANNER",
                detail: format!("{} has no Node Job planner", input.action),
                operation_id: None,
            });
        }
    };
    let control_plane_job = matches!(&kind, JobKind::NodeDrain | JobKind::NodeRemove);
    let node_id = if control_plane_job {
        "control-plane".to_string()
    } else {
        required_string(&input.fields, "target_node_id")?
    };
    let target_id = if control_plane_job {
        required_string(&input.fields, "node_id")?
    } else {
        input
            .fields
            .get("deployment_id")
            .or_else(|| input.fields.get("service_id"))
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| invalid("fields.deployment_id or fields.service_id is required"))?
            .to_string()
    };
    let payload = input
        .fields
        .get("payload")
        .cloned()
        .unwrap_or_else(|| Value::Object(input.fields.clone()));
    if matches!(&kind, JobKind::Upgrade | JobKind::Rollback) {
        let replacement: ReleaseReplacementPayload = serde_json::from_value(payload.clone())
            .map_err(|error| invalid(format!("invalid release replacement payload: {error}")))?;
        replacement
            .validate()
            .map_err(|error| invalid(error.to_string()))?;
    }
    let key = request
        .headers
        .get("idempotency-key")
        .map(String::as_str)
        .unwrap_or_default();
    let digest = Sha256::digest(format!("{}\0{}\0{}", input.action, target_id, key).as_bytes());
    Ok(PlanOperation {
        operation_id: format!("op-{:x}", digest),
        action: input.action,
        target_type: descriptor.target_type.to_string(),
        target_id,
        request: Value::Object(input.fields),
        jobs: vec![PlannedJob {
            step_id: "runtime".to_string(),
            node_id,
            kind,
            depends_on: vec![],
            condition: Default::default(),
            payload,
            max_attempts: 3,
        }],
    })
}

fn required_string(fields: &Map<String, Value>, name: &str) -> Result<String, OperationApiError> {
    fields
        .get(name)
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(str::to_string)
        .ok_or_else(|| invalid(format!("fields.{name} is required")))
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
    operation_id: Option<&str>,
) -> ApiResponse {
    ApiResponse::problem(status, code, detail, request_id, operation_id)
        .with_header("X-Request-ID", request_id)
}

#[derive(Debug)]
struct OperationApiError {
    status: u16,
    code: &'static str,
    detail: String,
    operation_id: Option<String>,
}

fn operation_error(error: OperationError) -> OperationApiError {
    let operation_id = match &error {
        OperationError::NotFound(operation_id) => Some(operation_id.clone()),
        _ => None,
    };
    let status = match error {
        OperationError::NotFound(_) => 404,
        OperationError::InvalidPlan(_) => 422,
        OperationError::IdempotencyConflict | OperationError::InvalidTransition { .. } => 409,
        OperationError::Store(_) | OperationError::Job(_) => 500,
    };
    OperationApiError {
        status,
        code: if status == 404 {
            "OPERATION_NOT_FOUND"
        } else if status == 409 {
            "OPERATION_CONFLICT"
        } else if status == 422 {
            "OPERATION_INVALID"
        } else {
            "OPERATION_STORAGE_ERROR"
        },
        detail: error.to_string(),
        operation_id,
    }
}

fn store_error(error: orchestrator_control_plane::OperationStoreError) -> OperationApiError {
    OperationApiError {
        status: 500,
        code: "OPERATION_STORAGE_ERROR",
        detail: error.to_string(),
        operation_id: None,
    }
}

fn job_error(error: orchestrator_control_plane::JobError) -> OperationApiError {
    OperationApiError {
        status: 500,
        code: "OPERATION_JOB_ERROR",
        detail: error.to_string(),
        operation_id: None,
    }
}

fn json_error(error: serde_json::Error) -> OperationApiError {
    OperationApiError {
        status: 400,
        code: "INVALID_JSON",
        detail: error.to_string(),
        operation_id: None,
    }
}

fn invalid(detail: impl Into<String>) -> OperationApiError {
    OperationApiError {
        status: 422,
        code: "OPERATION_INVALID",
        detail: detail.into(),
        operation_id: None,
    }
}

fn not_found(operation_id: &str) -> OperationApiError {
    OperationApiError {
        status: 404,
        code: "OPERATION_NOT_FOUND",
        detail: format!("operation {operation_id} was not found"),
        operation_id: Some(operation_id.to_string()),
    }
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(i64::MAX as u128) as i64
}

#[cfg(test)]
mod tests {
    use super::*;
    use orchestrator_control_plane::{
        ClaimRequest, DEFAULT_LEASE_MS, HeartbeatRequest, NewJobEvent,
    };
    use orchestrator_storage::SqliteOrchestratorStore;

    #[test]
    fn event_cursor_round_trips_and_rejects_malformed_input() {
        let cursor = EventCursor {
            operation_revision: 9,
            job_sequences: [("job-1".to_string(), 7)].into(),
        };
        let encoded = encode_event_cursor(&cursor).unwrap();
        let decoded = decode_event_cursor(&encoded).unwrap();
        assert_eq!(decoded.operation_revision, 9);
        assert_eq!(decoded.job_sequences.get("job-1"), Some(&7));
        assert_eq!(decode_event_cursor("not-hex").unwrap_err().status, 400);
    }

    #[test]
    fn release_upgrade_plans_the_dedicated_upgrade_job_kind() {
        let request = ApiRequest {
            method: "POST".to_string(),
            path: "/api/v1/operations:plan".to_string(),
            headers: [("idempotency-key".to_string(), "upgrade-1".to_string())].into(),
            body: json!({
                "action": "release.upgrade",
                "fields": {
                    "target_node_id": "node-1",
                    "deployment_id": "deployment-new",
                    "payload": {
                        "old_deployment_id": "deployment-old",
                        "old_container_id": "container-old",
                        "new_spec": {
                            "deployment_id": "deployment-new",
                            "service_id": "service-1",
                            "generation": 2,
                            "image": {
                                "repository": "registry.example/ojos/service",
                                "digest": format!("sha256:{}", "a".repeat(64))
                            },
                            "command": [],
                            "environment": [],
                            "labels": {}
                        },
                        "start": true,
                        "health_gate": {
                            "timeout_ms": 60000,
                            "poll_interval_ms": 1000,
                            "missing_healthcheck": "reject",
                            "compensation_timeout_ms": 30000
                        }
                    }
                }
            })
            .to_string(),
        };

        let plan = parse_plan(&request).unwrap();

        assert_eq!(plan.action, "release.upgrade");
        assert_eq!(plan.jobs.len(), 1);
        assert_eq!(plan.jobs[0].kind, JobKind::Upgrade);

        let mut invalid_request = request;
        let mut invalid_body: Value = serde_json::from_str(&invalid_request.body).unwrap();
        invalid_body["fields"]["payload"]["start"] = json!(false);
        invalid_request.body = invalid_body.to_string();
        let error = parse_plan(&invalid_request).unwrap_err();
        assert_eq!(error.status, 422);
    }

    #[test]
    fn node_lifecycle_plan_is_reserved_for_the_control_plane_worker() {
        let request = ApiRequest {
            method: "POST".to_string(),
            path: "/api/v1/operations:plan".to_string(),
            headers: [("idempotency-key".to_string(), "drain-plan-1".to_string())].into(),
            body: json!({
                "action": "node.drain",
                "fields": {"node_id": "node-1"}
            })
            .to_string(),
        };
        let plan = parse_plan(&request).unwrap();
        assert_eq!(plan.target_type, "Node");
        assert_eq!(plan.target_id, "node-1");
        assert_eq!(plan.jobs[0].node_id, "control-plane");
        assert_eq!(plan.jobs[0].kind, JobKind::NodeDrain);
        assert_eq!(plan.jobs[0].payload["node_id"], "node-1");
    }

    #[test]
    fn operation_collection_uses_a_stable_cursor() {
        let directory = tempfile::tempdir().unwrap();
        let storage = DurableStore::Sqlite(
            SqliteOrchestratorStore::open(directory.path().join("orchestrator.db")).unwrap(),
        );
        let mut operations = storage.operation_store();
        let mut jobs = storage.job_store();
        for operation_id in ["op-b", "op-a"] {
            OperationCoordinator::new(&mut operations, &mut jobs)
                .plan(
                    PlanOperation {
                        operation_id: operation_id.to_string(),
                        action: "deployment.health".to_string(),
                        target_type: "Deployment".to_string(),
                        target_id: operation_id.to_string(),
                        request: json!({}),
                        jobs: vec![PlannedJob {
                            step_id: "health".to_string(),
                            node_id: "node-1".to_string(),
                            kind: JobKind::Health,
                            depends_on: vec![],
                            condition: Default::default(),
                            payload: json!({"container_id": operation_id}),
                            max_attempts: 1,
                        }],
                    },
                    1,
                )
                .unwrap();
        }
        drop(operations);
        drop(jobs);

        let first = route_with_store(
            &storage,
            &ApiRequest {
                method: "GET".to_string(),
                path: "/api/v1/operations?limit=1".to_string(),
                headers: BTreeMap::new(),
                body: String::new(),
            },
            &["api", "v1", "operations"],
            "req-page-1",
        )
        .unwrap();
        assert_eq!(first.body["data"]["items"][0]["operation_id"], "op-a");
        assert_eq!(first.body["data"]["next_cursor"], "op-a");

        let second = route_with_store(
            &storage,
            &ApiRequest {
                method: "GET".to_string(),
                path: "/api/v1/operations?limit=1&cursor=op-a".to_string(),
                headers: BTreeMap::new(),
                body: String::new(),
            },
            &["api", "v1", "operations"],
            "req-page-2",
        )
        .unwrap();
        assert_eq!(second.body["data"]["items"][0]["operation_id"], "op-b");
        assert!(second.body["data"]["next_cursor"].is_null());
    }

    #[test]
    fn asynchronous_operation_actions_expose_the_top_level_operation_id() {
        let directory = tempfile::tempdir().unwrap();
        let storage = DurableStore::Sqlite(
            SqliteOrchestratorStore::open(directory.path().join("orchestrator.db")).unwrap(),
        );
        let mut operations = storage.operation_store();
        let mut jobs = storage.job_store();
        {
            let mut coordinator = OperationCoordinator::new(&mut operations, &mut jobs);
            coordinator
                .plan(
                    PlanOperation {
                        operation_id: "op-async-contract".to_string(),
                        action: "deployment.start".to_string(),
                        target_type: "Deployment".to_string(),
                        target_id: "deployment-1".to_string(),
                        request: json!({}),
                        jobs: vec![PlannedJob {
                            step_id: "start".to_string(),
                            node_id: "node-1".to_string(),
                            kind: JobKind::Start,
                            depends_on: vec![],
                            condition: Default::default(),
                            payload: json!({"container_id": "container-1"}),
                            max_attempts: 3,
                        }],
                    },
                    1,
                )
                .unwrap();
            coordinator.confirm("op-async-contract", 2).unwrap();
        }

        let response = route_with_store(
            &storage,
            &ApiRequest {
                method: "POST".to_string(),
                path: "/api/v1/operations/op-async-contract:apply".to_string(),
                headers: [("idempotency-key".to_string(), "apply-1".to_string())].into(),
                body: "{}".to_string(),
            },
            &["api", "v1", "operations", "op-async-contract:apply"],
            "req-async-contract",
        )
        .unwrap();

        assert_eq!(response.status, 202);
        assert_eq!(response.body["data"]["operation_id"], "op-async-contract");
        assert_eq!(
            response.body["data"]["operation"]["operation_id"],
            "op-async-contract"
        );
    }

    #[test]
    fn event_stream_resumes_after_last_event_id_without_replaying() {
        let directory = tempfile::tempdir().unwrap();
        let storage = DurableStore::Sqlite(
            SqliteOrchestratorStore::open(directory.path().join("orchestrator.db")).unwrap(),
        );
        let mut operations = storage.operation_store();
        let mut jobs = storage.job_store();
        let running = {
            let mut coordinator = OperationCoordinator::new(&mut operations, &mut jobs);
            coordinator
                .plan(
                    PlanOperation {
                        operation_id: "op-events".to_string(),
                        action: "deployment.health".to_string(),
                        target_type: "Deployment".to_string(),
                        target_id: "deployment-1".to_string(),
                        request: json!({}),
                        jobs: vec![PlannedJob {
                            step_id: "health".to_string(),
                            node_id: "node-1".to_string(),
                            kind: JobKind::Health,
                            depends_on: vec![],
                            condition: Default::default(),
                            payload: json!({"container_id": "container-1"}),
                            max_attempts: 3,
                        }],
                    },
                    1,
                )
                .unwrap();
            coordinator.confirm("op-events", 2).unwrap();
            coordinator.enqueue("op-events", 3).unwrap()
        };
        let claimed = jobs
            .claim(ClaimRequest {
                node_id: "node-1".to_string(),
                instance_id: "agent-1".to_string(),
                lease_token: "lease-1".to_string(),
                now_ms: 4,
                lease_ms: DEFAULT_LEASE_MS,
            })
            .unwrap()
            .unwrap();
        jobs.heartbeat(HeartbeatRequest {
            job_id: claimed.job_id,
            lease_token: "lease-1".to_string(),
            now_ms: 5,
            lease_ms: DEFAULT_LEASE_MS,
            events: vec![NewJobEvent {
                sequence: 1,
                event_type: "health".to_string(),
                level: "INFO".to_string(),
                message: "probe completed".to_string(),
                data: json!({"health": "healthy"}),
            }],
        })
        .unwrap();

        let first = operation_events(
            &storage,
            "op-events",
            &ApiRequest {
                method: "GET".to_string(),
                path: "/api/v1/operations/op-events/events".to_string(),
                headers: BTreeMap::new(),
                body: String::new(),
            },
            "req-events-1",
        )
        .unwrap();
        assert_eq!(first.content_type, "text/event-stream; charset=utf-8");
        let first_body = first.body.as_str().unwrap();
        assert!(first_body.contains("event: job"));
        assert!(first_body.contains("event: operation"));
        assert!(first_body.contains("probe completed"));
        let cursor = first_body
            .lines()
            .filter_map(|line| line.strip_prefix("id: "))
            .next_back()
            .unwrap()
            .to_string();

        let resumed = operation_events(
            &storage,
            "op-events",
            &ApiRequest {
                method: "GET".to_string(),
                path: "/api/v1/operations/op-events/events".to_string(),
                headers: [("last-event-id".to_string(), cursor)].into(),
                body: String::new(),
            },
            "req-events-2",
        )
        .unwrap();
        assert_eq!(
            resumed.body.as_str().unwrap(),
            ": keep-alive\nretry: 1000\n\n"
        );
        assert_eq!(running.operation_id, "op-events");
    }
}
