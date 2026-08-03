use crate::{SqliteOrchestratorStore, StorageError, StorageResult};
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};

const MAX_REQUEST_ID_LEN: usize = 256;
const MAX_ACTOR_LEN: usize = 256;
const MAX_ACTION_LEN: usize = 512;
const MAX_RESOURCE_LEN: usize = 2_048;
const MAX_IDEMPOTENCY_KEY_LEN: usize = 512;
const MAX_REQUEST_DIGEST_LEN: usize = 128;
const MAX_OPERATION_ID_LEN: usize = 256;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum AuditOutcome {
    Intent,
    Succeeded,
    Rejected,
}

impl AuditOutcome {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Intent => "INTENT",
            Self::Succeeded => "SUCCEEDED",
            Self::Rejected => "REJECTED",
        }
    }

    pub(crate) fn parse(value: &str) -> Result<Self, String> {
        match value {
            "INTENT" => Ok(Self::Intent),
            "SUCCEEDED" => Ok(Self::Succeeded),
            "REJECTED" => Ok(Self::Rejected),
            other => Err(format!("unknown audit outcome {other}")),
        }
    }
}

/// A single immutable row in the control-plane audit ledger.
///
/// A mutation writes an `Intent` row before dispatch and a terminal row after
/// dispatch. Rows are never updated in place, so an interrupted mutation is
/// visible as an intent without a corresponding terminal outcome.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AuditRecord {
    pub sequence: u64,
    pub request_id: String,
    pub actor: String,
    pub action: String,
    pub resource: String,
    pub idempotency_key: String,
    pub request_digest: String,
    pub outcome: AuditOutcome,
    pub response_status: Option<u16>,
    pub operation_id: Option<String>,
    pub timestamp_ms: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct NewAuditRecord {
    pub request_id: String,
    pub actor: String,
    pub action: String,
    pub resource: String,
    pub idempotency_key: String,
    pub request_digest: String,
    pub outcome: AuditOutcome,
    pub response_status: Option<u16>,
    pub operation_id: Option<String>,
    pub timestamp_ms: i64,
}

impl NewAuditRecord {
    pub(crate) fn validate(&self) -> Result<(), String> {
        required("request_id", &self.request_id, MAX_REQUEST_ID_LEN)?;
        required("actor", &self.actor, MAX_ACTOR_LEN)?;
        required("action", &self.action, MAX_ACTION_LEN)?;
        required("resource", &self.resource, MAX_RESOURCE_LEN)?;
        required(
            "idempotency_key",
            &self.idempotency_key,
            MAX_IDEMPOTENCY_KEY_LEN,
        )?;
        required(
            "request_digest",
            &self.request_digest,
            MAX_REQUEST_DIGEST_LEN,
        )?;
        if !self.request_digest.starts_with("sha256:") {
            return Err("request_digest must use the sha256: prefix".to_string());
        }
        if self.timestamp_ms < 0 {
            return Err("timestamp_ms must be non-negative".to_string());
        }
        match (self.outcome, self.response_status) {
            (AuditOutcome::Intent, None)
            | (AuditOutcome::Succeeded | AuditOutcome::Rejected, Some(_)) => {}
            (AuditOutcome::Intent, Some(_)) => {
                return Err("audit intent must not have a response_status".to_string());
            }
            (_, None) => {
                return Err("terminal audit outcome requires response_status".to_string());
            }
        }
        if let Some(operation_id) = &self.operation_id {
            required("operation_id", operation_id, MAX_OPERATION_ID_LEN)?;
        }
        Ok(())
    }

    pub(crate) fn into_stored(self, sequence: u64) -> AuditRecord {
        AuditRecord {
            sequence,
            request_id: self.request_id,
            actor: self.actor,
            action: self.action,
            resource: self.resource,
            idempotency_key: self.idempotency_key,
            request_digest: self.request_digest,
            outcome: self.outcome,
            response_status: self.response_status,
            operation_id: self.operation_id,
            timestamp_ms: self.timestamp_ms,
        }
    }
}

impl SqliteOrchestratorStore {
    pub fn append_audit_record(&self, record: NewAuditRecord) -> StorageResult<AuditRecord> {
        record.validate().map_err(StorageError::Invariant)?;
        let connection = self.connection()?;
        connection.execute(
            "INSERT INTO orchestrator_audit_log(request_id, actor, action, resource, idempotency_key, request_digest, outcome, response_status, operation_id, timestamp_ms) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                &record.request_id,
                &record.actor,
                &record.action,
                &record.resource,
                &record.idempotency_key,
                &record.request_digest,
                record.outcome.as_str(),
                record.response_status,
                &record.operation_id,
                record.timestamp_ms,
            ],
        )?;
        let sequence = u64::try_from(connection.last_insert_rowid()).map_err(|_| {
            StorageError::Invariant("audit sequence was outside the u64 range".to_string())
        })?;
        Ok(record.into_stored(sequence))
    }

    /// Reads the immutable ledger in sequence order. The bounded limit keeps
    /// diagnostics and future export endpoints from loading the entire table.
    pub fn audit_records(
        &self,
        request_id: Option<&str>,
        after_sequence: u64,
        limit: u32,
    ) -> StorageResult<Vec<AuditRecord>> {
        validate_read_window(after_sequence, limit).map_err(StorageError::Invariant)?;
        let after_sequence = i64::try_from(after_sequence).map_err(|_| {
            StorageError::Invariant("after_sequence was outside the i64 range".to_string())
        })?;
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT sequence, request_id, actor, action, resource, idempotency_key, request_digest, outcome, response_status, operation_id, timestamp_ms FROM orchestrator_audit_log WHERE sequence > ?1 AND (?2 IS NULL OR request_id = ?2) ORDER BY sequence LIMIT ?3",
        )?;
        let rows = statement.query_map(
            params![after_sequence, request_id, i64::from(limit)],
            audit_record_from_row,
        )?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(StorageError::from)
    }

    pub fn latest_audit_record_for_request(
        &self,
        request_id: &str,
    ) -> StorageResult<Option<AuditRecord>> {
        required("request_id", request_id, MAX_REQUEST_ID_LEN).map_err(StorageError::Invariant)?;
        self.connection()?
            .query_row(
                "SELECT sequence, request_id, actor, action, resource, idempotency_key, request_digest, outcome, response_status, operation_id, timestamp_ms FROM orchestrator_audit_log WHERE request_id = ?1 ORDER BY sequence DESC LIMIT 1",
                [request_id],
                audit_record_from_row,
            )
            .optional()
            .map_err(StorageError::from)
    }
}

fn audit_record_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<AuditRecord> {
    let sequence = row.get::<_, u64>(0)?;
    let outcome = row.get::<_, String>(7)?;
    let outcome = AuditOutcome::parse(&outcome).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(
            7,
            rusqlite::types::Type::Text,
            Box::new(std::io::Error::new(std::io::ErrorKind::InvalidData, error)),
        )
    })?;
    Ok(AuditRecord {
        sequence,
        request_id: row.get(1)?,
        actor: row.get(2)?,
        action: row.get(3)?,
        resource: row.get(4)?,
        idempotency_key: row.get(5)?,
        request_digest: row.get(6)?,
        outcome,
        response_status: row.get(8)?,
        operation_id: row.get(9)?,
        timestamp_ms: row.get(10)?,
    })
}

pub(crate) fn validate_read_window(after_sequence: u64, limit: u32) -> Result<(), String> {
    if after_sequence > i64::MAX as u64 {
        return Err("after_sequence must fit in a signed 64-bit integer".to_string());
    }
    if !(1..=1_000).contains(&limit) {
        return Err("audit read limit must be between 1 and 1000".to_string());
    }
    Ok(())
}

fn required(name: &str, value: &str, max_len: usize) -> Result<(), String> {
    if value.trim().is_empty() {
        return Err(format!("audit {name} must not be empty"));
    }
    if value.len() > max_len {
        return Err(format!("audit {name} exceeds {max_len} bytes"));
    }
    Ok(())
}
