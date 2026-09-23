use crate::auth::Principal;
use crate::durable::{DurableError, DurableStore};
use crate::http::{ApiRequest, ApiResponse};
use orchestrator_storage::{AuditOutcome, NewAuditRecord};
use serde_json::Value;

/// Immutable metadata shared by the intent and terminal audit rows for one
/// execution attempt. Idempotency replays do not create another execution
/// attempt and therefore do not enter this gate.
#[derive(Debug, Clone)]
pub(crate) struct MutationAudit {
    request_id: String,
    actor: String,
    action: String,
    resource: String,
    idempotency_key: String,
    request_digest: String,
}

impl MutationAudit {
    pub(crate) fn begin(
        store: &DurableStore,
        request: &ApiRequest,
        request_id: &str,
        idempotency_key: &str,
        request_digest: &str,
        principal: &Principal,
        timestamp_ms: i64,
    ) -> Result<Self, DurableError> {
        let resource = request.path.split('?').next().unwrap_or("/").to_string();
        let actor = principal.id().to_string();
        let audit = Self {
            request_id: request_id.to_string(),
            actor,
            action: format!("{} {resource}", request.method),
            resource,
            idempotency_key: idempotency_key.to_string(),
            request_digest: request_digest.to_string(),
        };
        store.append_audit_record(audit.record(AuditOutcome::Intent, None, None, timestamp_ms))?;
        Ok(audit)
    }

    pub(crate) fn finish(
        &self,
        store: &DurableStore,
        response: &ApiResponse,
        timestamp_ms: i64,
    ) -> Result<(), DurableError> {
        let outcome = if response.status < 400 {
            AuditOutcome::Succeeded
        } else {
            AuditOutcome::Rejected
        };
        store.append_audit_record(self.record(
            outcome,
            Some(response.status),
            operation_id(response),
            timestamp_ms,
        ))?;
        Ok(())
    }

    fn record(
        &self,
        outcome: AuditOutcome,
        response_status: Option<u16>,
        operation_id: Option<String>,
        timestamp_ms: i64,
    ) -> NewAuditRecord {
        NewAuditRecord {
            request_id: self.request_id.clone(),
            actor: self.actor.clone(),
            action: self.action.clone(),
            resource: self.resource.clone(),
            idempotency_key: self.idempotency_key.clone(),
            request_digest: self.request_digest.clone(),
            outcome,
            response_status,
            operation_id,
            timestamp_ms,
        }
    }
}

pub(crate) fn operation_id(response: &ApiResponse) -> Option<String> {
    [
        "/data/operation_id",
        "/data/operation/operation_id",
        "/operation_id",
        "/operation/operation_id",
    ]
    .into_iter()
    .find_map(|pointer| response.body.pointer(pointer).and_then(Value::as_str))
    .map(str::to_string)
}
