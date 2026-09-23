use crate::durable::{DurableError, DurableStore};
use crate::http::{ApiRequest, ApiResponse};
use orchestrator_control_plane::{
    DurableOperationStatus, JobKind, OperationCoordinator, OperationError, PlanOperation,
    PlannedJob,
};
use orchestrator_runtime::{
    RESOURCE_PURGE_JOB_SCHEMA_VERSION, ResourcePurgeAuditIntentV1, ResourcePurgePayloadV1,
};
use serde::Deserialize;
use serde_json::json;
use sha2::{Digest, Sha256};
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct PurgeRequest {
    node_id: String,
    claim_digest: String,
    generation: u64,
    confirmation: String,
    reason: String,
}

pub(crate) fn route(
    storage: Option<&DurableStore>,
    request: &ApiRequest,
    request_id: &str,
) -> Option<ApiResponse> {
    let path = request.path.split('?').next().unwrap_or("/");
    let segments = path.trim_start_matches('/').split('/').collect::<Vec<_>>();
    if segments.get(0..3) != Some(&["api", "v1", "resources"]) {
        return None;
    }
    let response = match storage {
        None => Err(ResourceApiError::new(
            503,
            "RESOURCE_PURGE_STORAGE_UNAVAILABLE",
            "resource purge requires durable Operation and Job storage",
        )),
        Some(storage) => route_with_store(storage, request, &segments, request_id),
    };
    Some(match response {
        Ok(response) => response,
        Err(error) => ApiResponse::problem(
            error.status,
            error.code,
            error.detail,
            request_id,
            error.operation_id.as_deref(),
        )
        .with_header("X-Request-ID", request_id),
    })
}

fn route_with_store(
    storage: &DurableStore,
    request: &ApiRequest,
    segments: &[&str],
    request_id: &str,
) -> Result<ApiResponse, ResourceApiError> {
    let (claim_id, action) = match (request.method.as_str(), segments) {
        ("POST", ["api", "v1", "resources", target]) => target
            .rsplit_once(':')
            .ok_or_else(|| invalid("resource mutation route requires an action suffix"))?,
        _ => {
            return Err(ResourceApiError::new(
                404,
                "ROUTE_NOT_FOUND",
                "the requested Resource v1 route does not exist",
            ));
        }
    };
    if action != "purge" {
        return Err(ResourceApiError::new(
            404,
            "ROUTE_NOT_FOUND",
            "the requested Resource v1 action does not exist",
        ));
    }
    let input: PurgeRequest = serde_json::from_str(&request.body)
        .map_err(|error| invalid(format!("resource purge body is invalid: {error}")))?;
    let node = storage
        .get_node(&input.node_id)
        .map_err(storage_error)?
        .ok_or_else(|| {
            ResourceApiError::new(
                404,
                "RESOURCE_PURGE_NODE_NOT_FOUND",
                format!("resource purge node {} is not registered", input.node_id),
            )
        })?;
    if matches!(
        node.status.to_ascii_uppercase().as_str(),
        "REMOVED" | "REVOKED"
    ) {
        return Err(ResourceApiError::new(
            409,
            "RESOURCE_PURGE_NODE_UNAVAILABLE",
            format!("resource purge node {} is {}", input.node_id, node.status),
        ));
    }
    let actor_id = request
        .headers
        .get("x-actor-id")
        .map(String::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| invalid("resource purge requires a verified actor"))?;
    let idempotency_key = request
        .headers
        .get("idempotency-key")
        .map(String::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| invalid("resource purge requires Idempotency-Key"))?;
    let operation_digest =
        Sha256::digest(format!("resource.purge\0{claim_id}\0{idempotency_key}").as_bytes());
    let operation_id = format!("op-resource-purge-{operation_digest:x}");
    let payload = ResourcePurgePayloadV1 {
        schema_version: RESOURCE_PURGE_JOB_SCHEMA_VERSION.to_string(),
        node_id: input.node_id.clone(),
        claim_id: claim_id.to_string(),
        claim_digest: input.claim_digest.clone(),
        generation: input.generation,
        confirmation: input.confirmation.clone(),
        reason: input.reason.clone(),
        audit_intent: ResourcePurgeAuditIntentV1 {
            intent_id: operation_id.clone(),
            actor_id: actor_id.to_string(),
            claim_digest: input.claim_digest.clone(),
            generation: input.generation,
        },
    };
    payload.validate().map_err(invalid)?;
    let payload = serde_json::to_value(payload)
        .map_err(|_| invalid("resource purge payload could not be encoded"))?;
    let plan = PlanOperation {
        operation_id: operation_id.clone(),
        action: "resource.purge".to_string(),
        target_type: "ResourceClaim".to_string(),
        target_id: claim_id.to_string(),
        request: json!({
            "claim_id": claim_id,
            "claim_digest": input.claim_digest,
            "generation": input.generation,
            "node_id": input.node_id,
            "actor_id": actor_id,
            "reason": input.reason,
            "auto_enqueue": true,
        }),
        jobs: vec![PlannedJob {
            step_id: "resource-purge".to_string(),
            node_id: payload["node_id"].as_str().unwrap_or_default().to_string(),
            kind: JobKind::ResourcePurge,
            depends_on: vec![],
            condition: Default::default(),
            payload,
            max_attempts: 1,
        }],
    };
    let mut operations = storage.operation_store();
    let mut jobs = storage.job_store();
    let mut coordinator = OperationCoordinator::new(&mut operations, &mut jobs);
    let operation = coordinator.plan(plan, now_ms()).map_err(operation_error)?;
    let operation = match operation.status {
        DurableOperationStatus::Planned => {
            coordinator
                .confirm(&operation_id, now_ms())
                .map_err(operation_error)?;
            coordinator
                .enqueue(&operation_id, now_ms())
                .map_err(operation_error)?
        }
        DurableOperationStatus::Confirmed
        | DurableOperationStatus::Enqueuing
        | DurableOperationStatus::Running => coordinator
            .enqueue(&operation_id, now_ms())
            .map_err(operation_error)?,
        DurableOperationStatus::Cancelling
        | DurableOperationStatus::Succeeded
        | DurableOperationStatus::Failed
        | DurableOperationStatus::Cancelled
        | DurableOperationStatus::NeedsAttention
        | DurableOperationStatus::RolledBack => operation,
    };
    Ok(ApiResponse::accepted(json!({
        "data": {"operation_id": operation_id, "operation": operation},
        "meta": {"request_id": request_id, "api_version": "v1"},
    }))
    .with_header("X-Request-ID", request_id))
}

#[derive(Debug)]
struct ResourceApiError {
    status: u16,
    code: &'static str,
    detail: String,
    operation_id: Option<String>,
}

impl ResourceApiError {
    fn new(status: u16, code: &'static str, detail: impl Into<String>) -> Self {
        Self {
            status,
            code,
            detail: detail.into(),
            operation_id: None,
        }
    }
}

fn invalid(detail: impl Into<String>) -> ResourceApiError {
    ResourceApiError::new(400, "RESOURCE_PURGE_REQUEST_INVALID", detail)
}

fn operation_error(error: OperationError) -> ResourceApiError {
    let status = match error {
        OperationError::IdempotencyConflict => 409,
        OperationError::Store(_) | OperationError::Job(_) => 503,
        _ => 422,
    };
    ResourceApiError::new(status, "RESOURCE_PURGE_OPERATION_FAILED", error.to_string())
}

fn storage_error(error: DurableError) -> ResourceApiError {
    ResourceApiError::new(503, "RESOURCE_PURGE_STORAGE_ERROR", error.to_string())
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(i64::MAX as u128) as i64
}
