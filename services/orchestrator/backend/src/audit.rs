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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::Principal;
    use crate::durable::DurableStore;
    use orchestrator_storage::{AuditOutcome, SqliteOptions, SqliteOrchestratorStore};
    use std::collections::BTreeMap;

    #[test]
    fn writes_intent_and_terminal_rows_without_request_body() {
        let temp = tempfile::tempdir().expect("tempdir");
        let store = DurableStore::Sqlite(
            SqliteOrchestratorStore::open_with_options(
                temp.path().join("audit.db"),
                SqliteOptions {
                    acquire_instance_lock: false,
                    ..SqliteOptions::default()
                },
            )
            .expect("open"),
        );
        let request = ApiRequest {
            method: "POST".to_string(),
            path: "/api/v1/deployments/deployment-1:start?wait=false".to_string(),
            headers: BTreeMap::from([
                ("idempotency-key".to_string(), "key-1".to_string()),
                ("x-actor-id".to_string(), "operator-1".to_string()),
            ]),
            body: "{\"secret\":\"must-not-be-audited\"}".to_string(),
        };
        let audit = MutationAudit::begin(
            &store,
            &request,
            "request-1",
            "key-1",
            "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            &Principal::internal_admin(),
            10,
        )
        .expect("intent");
        let response = ApiResponse::accepted(serde_json::json!({
            "data": {"operation_id": "operation-1"}
        }));
        audit.finish(&store, &response, 11).expect("result");

        let rows = store.audit_records(Some("request-1"), 0, 10).expect("rows");
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].outcome, AuditOutcome::Intent);
        assert_eq!(rows[0].actor, "internal-admin");
        assert_eq!(rows[1].outcome, AuditOutcome::Succeeded);
        assert_eq!(rows[1].response_status, Some(202));
        assert_eq!(rows[1].operation_id.as_deref(), Some("operation-1"));
        assert!(rows.iter().all(|row| !row.resource.contains("secret")));
    }

    #[test]
    fn terminal_write_failure_leaves_an_unpaired_intent() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("audit.db");
        let store = DurableStore::Sqlite(
            SqliteOrchestratorStore::open_with_options(
                &path,
                SqliteOptions {
                    acquire_instance_lock: false,
                    ..SqliteOptions::default()
                },
            )
            .expect("open"),
        );
        let request = ApiRequest {
            method: "POST".to_string(),
            path: "/api/v1/operations:plan".to_string(),
            headers: BTreeMap::new(),
            body: "{}".to_string(),
        };
        let audit = MutationAudit::begin(
            &store,
            &request,
            "request-2",
            "key-2",
            "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            &Principal::desktop_admin(),
            20,
        )
        .expect("intent");
        rusqlite::Connection::open(path)
            .expect("raw connection")
            .execute_batch(
                "CREATE TRIGGER reject_terminal_audit BEFORE INSERT ON orchestrator_audit_log WHEN NEW.outcome <> 'INTENT' BEGIN SELECT RAISE(ABORT, 'forced audit failure'); END;",
            )
            .expect("failure trigger");
        assert!(
            audit
                .finish(&store, &ApiResponse::accepted(serde_json::json!({})), 21)
                .is_err()
        );
        let rows = store
            .audit_records(Some("request-2"), 0, 10)
            .expect("unpaired intent");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].outcome, AuditOutcome::Intent);
    }
}
