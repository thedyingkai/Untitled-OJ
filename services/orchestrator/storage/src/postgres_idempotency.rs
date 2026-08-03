use crate::{
    IdempotencyBegin, PostgresError, PostgresOrchestratorStore, PostgresResult,
    StoredIdempotentResponse,
};
use r2d2_postgres::postgres::{GenericClient, Row};

const STARTED_STALE_AFTER_MS: i64 = 5 * 60 * 1_000;
const RETENTION_MS: i64 = 24 * 60 * 60 * 1_000;

impl PostgresOrchestratorStore {
    pub fn begin_idempotent_request(
        &self,
        scope: &str,
        key: &str,
        request_sha256: &str,
        now_ms: i64,
    ) -> PostgresResult<IdempotencyBegin> {
        validate_input(scope, key, request_sha256)?;
        let mut connection = self.pool().connection()?;
        let mut transaction = connection.transaction()?;
        let existing = load_record(&mut transaction, scope, key)?;
        let outcome = if let Some(row) = existing {
            evaluate_record(&row, request_sha256, now_ms)?
        } else {
            let expires_at_ms = now_ms.saturating_add(RETENTION_MS);
            let inserted = transaction.execute(
                "INSERT INTO orchestrator_idempotency(scope, idempotency_key, request_sha256, state, started_at_ms, expires_at_ms) VALUES ($1, $2, $3, 'STARTED', $4, $5) ON CONFLICT(scope, idempotency_key) DO NOTHING",
                &[&scope, &key, &request_sha256, &now_ms, &expires_at_ms],
            )?;
            if inserted == 1 {
                IdempotencyBegin::Started
            } else {
                let row = load_record(&mut transaction, scope, key)?.ok_or_else(|| {
                    PostgresError::Invariant(
                        "idempotency row disappeared after uniqueness conflict".to_string(),
                    )
                })?;
                evaluate_record(&row, request_sha256, now_ms)?
            }
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
    ) -> PostgresResult<()> {
        validate_input(scope, key, request_sha256)?;
        if response.content_type.trim().is_empty() {
            return Err(PostgresError::Invariant(
                "idempotent response content type must not be empty".to_string(),
            ));
        }
        let response_status = i32::from(response.status);
        let response_headers = serde_json::to_string(&response.headers)?;
        let response_body = serde_json::to_string(&response.body)?;
        let expires_at_ms = now_ms.saturating_add(RETENTION_MS);
        let changed = self.pool().with_client(|client| {
            Ok(client.execute(
                "UPDATE orchestrator_idempotency SET state = 'COMPLETED', response_status = $4, response_content_type = $5, response_headers = $6::text::jsonb, response_body = $7::text::jsonb, completed_at_ms = $8, expires_at_ms = $9 WHERE scope = $1 AND idempotency_key = $2 AND request_sha256 = $3 AND state = 'STARTED'",
                &[
                    &scope,
                    &key,
                    &request_sha256,
                    &response_status,
                    &response.content_type,
                    &response_headers,
                    &response_body,
                    &now_ms,
                    &expires_at_ms,
                ],
            )?)
        })?;
        if changed != 1 {
            return Err(PostgresError::Conflict(
                "idempotency reservation is missing or already completed".to_string(),
            ));
        }
        Ok(())
    }

    /// Releases only the caller's still-uncommitted pre-dispatch reservation.
    /// Completed/changed rows are never removed.
    pub fn abort_idempotent_request(
        &self,
        scope: &str,
        key: &str,
        request_sha256: &str,
    ) -> PostgresResult<()> {
        validate_input(scope, key, request_sha256)?;
        let changed = self.pool().with_client(|client| {
            Ok(client.execute(
                "DELETE FROM orchestrator_idempotency WHERE scope = $1 AND idempotency_key = $2 AND request_sha256 = $3 AND state = 'STARTED'",
                &[&scope, &key, &request_sha256],
            )?)
        })?;
        if changed != 1 {
            return Err(PostgresError::Conflict(
                "idempotency reservation is missing, completed, or owned by another request"
                    .to_string(),
            ));
        }
        Ok(())
    }

    pub fn purge_expired_idempotency(&self, now_ms: i64) -> PostgresResult<u64> {
        self.pool().with_client(|client| {
            client
                .execute(
                    "DELETE FROM orchestrator_idempotency WHERE expires_at_ms < $1 AND state = 'COMPLETED'",
                    &[&now_ms],
                )
                .map_err(Into::into)
        })
    }
}

fn load_record(
    client: &mut impl GenericClient,
    scope: &str,
    key: &str,
) -> PostgresResult<Option<Row>> {
    client
        .query_opt(
            "SELECT request_sha256, state, response_status, response_content_type, response_headers::text, response_body::text, started_at_ms FROM orchestrator_idempotency WHERE scope = $1 AND idempotency_key = $2 FOR UPDATE",
            &[&scope, &key],
        )
        .map_err(Into::into)
}

fn evaluate_record(
    row: &Row,
    request_sha256: &str,
    now_ms: i64,
) -> PostgresResult<IdempotencyBegin> {
    let existing_sha: String = row.get(0);
    if existing_sha != request_sha256 {
        return Err(PostgresError::Conflict(
            "Idempotency-Key was already used with a different request".to_string(),
        ));
    }
    let state: String = row.get(1);
    let started_at_ms: i64 = row.get(6);
    match state.as_str() {
        "STARTED" if now_ms.saturating_sub(started_at_ms) >= STARTED_STALE_AFTER_MS => {
            Ok(IdempotencyBegin::NeedsAttention)
        }
        "STARTED" => Ok(IdempotencyBegin::InProgress),
        "COMPLETED" => {
            let response_status: i32 = row.get::<_, Option<i32>>(2).ok_or_else(|| {
                PostgresError::Invariant(
                    "completed idempotency record has no response status".to_string(),
                )
            })?;
            let status = u16::try_from(response_status).map_err(|_| {
                PostgresError::Invariant(
                    "completed idempotency response status is outside u16 range".to_string(),
                )
            })?;
            let content_type = row.get::<_, Option<String>>(3).ok_or_else(|| {
                PostgresError::Invariant(
                    "completed idempotency record has no content type".to_string(),
                )
            })?;
            let headers =
                serde_json::from_str(&row.get::<_, Option<String>>(4).ok_or_else(|| {
                    PostgresError::Invariant(
                        "completed idempotency record has no headers".to_string(),
                    )
                })?)?;
            let body =
                serde_json::from_str(&row.get::<_, Option<String>>(5).ok_or_else(|| {
                    PostgresError::Invariant("completed idempotency record has no body".to_string())
                })?)?;
            Ok(IdempotencyBegin::Replay(StoredIdempotentResponse {
                status,
                content_type,
                headers,
                body,
            }))
        }
        other => Err(PostgresError::Invariant(format!(
            "unknown idempotency state {other}"
        ))),
    }
}

fn validate_input(scope: &str, key: &str, request_sha256: &str) -> PostgresResult<()> {
    if scope.trim().is_empty() || scope.len() > 512 {
        return Err(PostgresError::Invariant(
            "idempotency scope must contain 1..=512 characters".to_string(),
        ));
    }
    if key.trim().len() < 8 || key.len() > 200 {
        return Err(PostgresError::Invariant(
            "Idempotency-Key must contain 8..=200 characters".to_string(),
        ));
    }
    let digest = request_sha256.strip_prefix("sha256:").unwrap_or_default();
    if digest.len() != 64
        || !digest
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(PostgresError::Invariant(
            "idempotency request digest must be sha256 lowercase hex".to_string(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn input_validation_matches_sqlite_contract() {
        let digest = format!("sha256:{}", "a".repeat(64));
        assert!(validate_input("scope", "request-1", &digest).is_ok());
        assert!(validate_input("", "request-1", &digest).is_err());
        assert!(validate_input("scope", "short", &digest).is_err());
        assert!(validate_input("scope", "request-1", "sha256:ABC").is_err());
    }
}
