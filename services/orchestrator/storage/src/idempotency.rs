use crate::{SqliteOrchestratorStore, StorageError, StorageResult};
use rusqlite::{OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

const STARTED_STALE_AFTER_MS: i64 = 5 * 60 * 1_000;
const RETENTION_MS: i64 = 24 * 60 * 60 * 1_000;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StoredIdempotentResponse {
    pub status: u16,
    pub content_type: String,
    pub headers: BTreeMap<String, String>,
    pub body: Value,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IdempotencyBegin {
    Started,
    Replay(StoredIdempotentResponse),
    InProgress,
    NeedsAttention,
}

impl SqliteOrchestratorStore {
    pub fn begin_idempotent_request(
        &self,
        scope: &str,
        key: &str,
        request_sha256: &str,
        now_ms: i64,
    ) -> StorageResult<IdempotencyBegin> {
        validate_input(scope, key, request_sha256)?;
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let existing = transaction
            .query_row(
                "SELECT request_sha256, state, response_status, response_content_type, response_headers, response_body, started_at_ms FROM orchestrator_idempotency WHERE scope = ?1 AND idempotency_key = ?2",
                params![scope, key],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, Option<u16>>(2)?,
                        row.get::<_, Option<String>>(3)?,
                        row.get::<_, Option<String>>(4)?,
                        row.get::<_, Option<String>>(5)?,
                        row.get::<_, i64>(6)?,
                    ))
                },
            )
            .optional()?;
        let outcome = if let Some((
            existing_sha,
            state,
            response_status,
            response_content_type,
            response_headers,
            response_body,
            started_at_ms,
        )) = existing
        {
            if existing_sha != request_sha256 {
                return Err(StorageError::Conflict(
                    "Idempotency-Key was already used with a different request".to_string(),
                ));
            }
            match state.as_str() {
                "STARTED" if now_ms.saturating_sub(started_at_ms) >= STARTED_STALE_AFTER_MS => {
                    IdempotencyBegin::NeedsAttention
                }
                "STARTED" => IdempotencyBegin::InProgress,
                "COMPLETED" => IdempotencyBegin::Replay(StoredIdempotentResponse {
                    status: response_status.ok_or_else(|| {
                        StorageError::Invariant(
                            "completed idempotency record has no response status".to_string(),
                        )
                    })?,
                    content_type: response_content_type.ok_or_else(|| {
                        StorageError::Invariant(
                            "completed idempotency record has no content type".to_string(),
                        )
                    })?,
                    headers: serde_json::from_str(response_headers.as_deref().ok_or_else(
                        || {
                            StorageError::Invariant(
                                "completed idempotency record has no headers".to_string(),
                            )
                        },
                    )?)?,
                    body: serde_json::from_str(response_body.as_deref().ok_or_else(|| {
                        StorageError::Invariant(
                            "completed idempotency record has no body".to_string(),
                        )
                    })?)?,
                }),
                other => {
                    return Err(StorageError::Invariant(format!(
                        "unknown idempotency state {other}"
                    )));
                }
            }
        } else {
            transaction.execute(
                "INSERT INTO orchestrator_idempotency(scope, idempotency_key, request_sha256, state, started_at_ms, expires_at_ms) VALUES (?1, ?2, ?3, 'STARTED', ?4, ?5)",
                params![scope, key, request_sha256, now_ms, now_ms.saturating_add(RETENTION_MS)],
            )?;
            IdempotencyBegin::Started
        };
        transaction.commit()?;
        Ok(outcome)
    }

    pub fn complete_idempotent_request(
        &self,
        scope: &str,
        key: &str,
        request_sha256: &str,
        response: &StoredIdempotentResponse,
        now_ms: i64,
    ) -> StorageResult<()> {
        validate_input(scope, key, request_sha256)?;
        if response.content_type.trim().is_empty() {
            return Err(StorageError::Invariant(
                "idempotent response content type must not be empty".to_string(),
            ));
        }
        let changed = self.connection()?.execute(
            "UPDATE orchestrator_idempotency SET state = 'COMPLETED', response_status = ?4, response_content_type = ?5, response_headers = ?6, response_body = ?7, completed_at_ms = ?8, expires_at_ms = ?9 WHERE scope = ?1 AND idempotency_key = ?2 AND request_sha256 = ?3 AND state = 'STARTED'",
            params![
                scope,
                key,
                request_sha256,
                response.status,
                response.content_type,
                serde_json::to_string(&response.headers)?,
                serde_json::to_string(&response.body)?,
                now_ms,
                now_ms.saturating_add(RETENTION_MS),
            ],
        )?;
        if changed != 1 {
            return Err(StorageError::Conflict(
                "idempotency reservation is missing or already completed".to_string(),
            ));
        }
        Ok(())
    }

    /// Releases a reservation only when the exact request still owns a
    /// `STARTED` row. This is used when a pre-dispatch fail-closed gate (for
    /// example the audit-intent write) rejects the mutation before any side
    /// effect can have occurred.
    pub fn abort_idempotent_request(
        &self,
        scope: &str,
        key: &str,
        request_sha256: &str,
    ) -> StorageResult<()> {
        validate_input(scope, key, request_sha256)?;
        let changed = self.connection()?.execute(
            "DELETE FROM orchestrator_idempotency WHERE scope = ?1 AND idempotency_key = ?2 AND request_sha256 = ?3 AND state = 'STARTED'",
            params![scope, key, request_sha256],
        )?;
        if changed != 1 {
            return Err(StorageError::Conflict(
                "idempotency reservation is missing, completed, or owned by another request"
                    .to_string(),
            ));
        }
        Ok(())
    }

    pub fn purge_expired_idempotency(&self, now_ms: i64) -> StorageResult<u64> {
        let deleted = self.connection()?.execute(
            "DELETE FROM orchestrator_idempotency WHERE expires_at_ms < ?1 AND state = 'COMPLETED'",
            [now_ms],
        )?;
        Ok(deleted as u64)
    }
}

fn validate_input(scope: &str, key: &str, request_sha256: &str) -> StorageResult<()> {
    if scope.trim().is_empty() || scope.len() > 512 {
        return Err(StorageError::Invariant(
            "idempotency scope must contain 1..=512 characters".to_string(),
        ));
    }
    if key.trim().len() < 8 || key.len() > 200 {
        return Err(StorageError::Invariant(
            "Idempotency-Key must contain 8..=200 characters".to_string(),
        ));
    }
    let digest = request_sha256.strip_prefix("sha256:").unwrap_or_default();
    if digest.len() != 64
        || !digest
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(StorageError::Invariant(
            "idempotency request digest must be sha256 lowercase hex".to_string(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tempfile::tempdir;

    fn response() -> StoredIdempotentResponse {
        StoredIdempotentResponse {
            status: 202,
            content_type: "application/json".to_string(),
            headers: BTreeMap::from([("X-Request-ID".to_string(), "req-1".to_string())]),
            body: json!({"operation_id": "op-1"}),
        }
    }

    #[test]
    fn completed_request_replays_and_conflicting_payload_is_rejected() {
        let directory = tempdir().unwrap();
        let store = SqliteOrchestratorStore::open(directory.path().join("db.sqlite")).unwrap();
        let digest = format!("sha256:{}", "a".repeat(64));
        assert_eq!(
            store
                .begin_idempotent_request("POST /api/v1/test", "request-1", &digest, 1)
                .unwrap(),
            IdempotencyBegin::Started
        );
        assert_eq!(
            store
                .begin_idempotent_request("POST /api/v1/test", "request-1", &digest, 2)
                .unwrap(),
            IdempotencyBegin::InProgress
        );
        store
            .complete_idempotent_request("POST /api/v1/test", "request-1", &digest, &response(), 3)
            .unwrap();
        assert_eq!(
            store
                .begin_idempotent_request("POST /api/v1/test", "request-1", &digest, 4)
                .unwrap(),
            IdempotencyBegin::Replay(response())
        );
        let other = format!("sha256:{}", "b".repeat(64));
        assert!(matches!(
            store.begin_idempotent_request("POST /api/v1/test", "request-1", &other, 5),
            Err(StorageError::Conflict(_))
        ));
    }

    #[test]
    fn stale_started_request_requires_attention_instead_of_reexecution() {
        let directory = tempdir().unwrap();
        let store = SqliteOrchestratorStore::open(directory.path().join("db.sqlite")).unwrap();
        let digest = format!("sha256:{}", "a".repeat(64));
        store
            .begin_idempotent_request("scope", "request-1", &digest, 1)
            .unwrap();
        assert_eq!(
            store
                .begin_idempotent_request(
                    "scope",
                    "request-1",
                    &digest,
                    STARTED_STALE_AFTER_MS + 1,
                )
                .unwrap(),
            IdempotencyBegin::NeedsAttention
        );
    }

    #[test]
    fn aborted_predispatch_reservation_can_be_retried_immediately() {
        let directory = tempdir().unwrap();
        let store = SqliteOrchestratorStore::open(directory.path().join("db.sqlite")).unwrap();
        let digest = format!("sha256:{}", "a".repeat(64));
        assert_eq!(
            store
                .begin_idempotent_request("scope", "request-1", &digest, 1)
                .unwrap(),
            IdempotencyBegin::Started
        );
        store
            .abort_idempotent_request("scope", "request-1", &digest)
            .unwrap();
        assert_eq!(
            store
                .begin_idempotent_request("scope", "request-1", &digest, 2)
                .unwrap(),
            IdempotencyBegin::Started
        );
    }
}
