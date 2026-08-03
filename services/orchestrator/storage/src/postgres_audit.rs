use crate::{
    AuditOutcome, AuditRecord, NewAuditRecord, PostgresError, PostgresOrchestratorStore,
    PostgresResult, audit::validate_read_window,
};
use r2d2_postgres::postgres::Row;

impl PostgresOrchestratorStore {
    pub fn append_audit_record(&self, record: NewAuditRecord) -> PostgresResult<AuditRecord> {
        record.validate().map_err(PostgresError::Invariant)?;
        let row = self.pool().with_client(|client| {
            client
                .query_one(
                    "INSERT INTO orchestrator_audit_log(request_id, actor, action, resource, idempotency_key, request_digest, outcome, response_status, operation_id, timestamp_ms) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10) RETURNING sequence",
                    &[
                        &record.request_id,
                        &record.actor,
                        &record.action,
                        &record.resource,
                        &record.idempotency_key,
                        &record.request_digest,
                        &record.outcome.as_str(),
                        &record.response_status.map(i32::from),
                        &record.operation_id,
                        &record.timestamp_ms,
                    ],
                )
                .map_err(PostgresError::from)
        })?;
        let sequence = u64::try_from(row.get::<_, i64>(0)).map_err(|_| {
            PostgresError::Invariant("audit sequence was outside the u64 range".to_string())
        })?;
        Ok(record.into_stored(sequence))
    }

    pub fn audit_records(
        &self,
        request_id: Option<&str>,
        after_sequence: u64,
        limit: u32,
    ) -> PostgresResult<Vec<AuditRecord>> {
        validate_read_window(after_sequence, limit).map_err(PostgresError::Invariant)?;
        let after_sequence = i64::try_from(after_sequence).map_err(|_| {
            PostgresError::Invariant("after_sequence was outside the i64 range".to_string())
        })?;
        self.pool().with_client(|client| {
            client
                .query(
                    "SELECT sequence, request_id, actor, action, resource, idempotency_key, request_digest, outcome, response_status, operation_id, timestamp_ms FROM orchestrator_audit_log WHERE sequence > $1 AND ($2::text IS NULL OR request_id = $2) ORDER BY sequence LIMIT $3",
                    &[&after_sequence, &request_id, &i64::from(limit)],
                )?
                .iter()
                .map(audit_record_from_row)
                .collect()
        })
    }

    pub fn latest_audit_record_for_request(
        &self,
        request_id: &str,
    ) -> PostgresResult<Option<AuditRecord>> {
        if request_id.trim().is_empty() || request_id.len() > 256 {
            return Err(PostgresError::Invariant(
                "audit request_id must contain 1 to 256 bytes".to_string(),
            ));
        }
        self.pool().with_client(|client| {
            client
                .query_opt(
                    "SELECT sequence, request_id, actor, action, resource, idempotency_key, request_digest, outcome, response_status, operation_id, timestamp_ms FROM orchestrator_audit_log WHERE request_id = $1 ORDER BY sequence DESC LIMIT 1",
                    &[&request_id],
                )?
                .as_ref()
                .map(audit_record_from_row)
                .transpose()
        })
    }
}

fn audit_record_from_row(row: &Row) -> PostgresResult<AuditRecord> {
    let sequence = u64::try_from(row.get::<_, i64>(0)).map_err(|_| {
        PostgresError::Invariant("audit sequence was outside the u64 range".to_string())
    })?;
    let outcome =
        AuditOutcome::parse(row.get::<_, String>(7).as_str()).map_err(PostgresError::Invariant)?;
    Ok(AuditRecord {
        sequence,
        request_id: row.get(1),
        actor: row.get(2),
        action: row.get(3),
        resource: row.get(4),
        idempotency_key: row.get(5),
        request_digest: row.get(6),
        outcome,
        response_status: row.get::<_, Option<i32>>(8).map(|value| value as u16),
        operation_id: row.get(9),
        timestamp_ms: row.get(10),
    })
}
