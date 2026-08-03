use crate::{PostgresOrchestratorStore, PostgresResult, SqliteOrchestratorStore, StorageResult};
use rusqlite::TransactionBehavior;

/// Counts from one bounded history-retention pass. Audit rows and domain
/// resources are deliberately absent: the audit ledger remains append-only,
/// while Operations and Jobs remain queryable after their verbose history is
/// removed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct HistoryRetentionReport {
    pub operation_logs_deleted: u64,
    pub job_events_deleted: u64,
    pub idempotency_records_deleted: u64,
}

impl SqliteOrchestratorStore {
    pub fn purge_terminal_history(
        &self,
        completed_before_ms: i64,
        now_ms: i64,
    ) -> StorageResult<HistoryRetentionReport> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let operation_logs_deleted = transaction.execute(
            "DELETE FROM orchestrator_operation_logs_v2
             WHERE operation_id IN (
               SELECT operation_id FROM orchestrator_durable_operations
               WHERE status IN ('SUCCEEDED', 'FAILED', 'CANCELLED', 'NEEDS_ATTENTION')
                 AND updated_at_ms < ?1
             )",
            [completed_before_ms],
        )? as u64;
        let job_events_deleted = transaction.execute(
            "DELETE FROM orchestrator_job_events
             WHERE job_id IN (
               SELECT job_id FROM orchestrator_jobs
               WHERE status IN ('SUCCEEDED', 'FAILED', 'CANCELLED', 'NEEDS_ATTENTION')
                 AND CAST(json_extract(payload, '$.completed_at_ms') AS INTEGER) < ?1
             )",
            [completed_before_ms],
        )? as u64;
        transaction.commit()?;
        let idempotency_records_deleted = self.purge_expired_idempotency(now_ms)?;
        Ok(HistoryRetentionReport {
            operation_logs_deleted,
            job_events_deleted,
            idempotency_records_deleted,
        })
    }
}

impl PostgresOrchestratorStore {
    pub fn purge_terminal_history(
        &self,
        completed_before_ms: i64,
        now_ms: i64,
    ) -> PostgresResult<HistoryRetentionReport> {
        let (operation_logs_deleted, job_events_deleted) =
            self.pool().with_transaction(|transaction| {
                let operation_logs_deleted = transaction.execute(
                    "DELETE FROM orchestrator_operation_logs_v2
                     WHERE operation_id IN (
                       SELECT operation_id FROM orchestrator_durable_operations
                       WHERE status IN ('SUCCEEDED', 'FAILED', 'CANCELLED', 'NEEDS_ATTENTION')
                         AND updated_at_ms < $1
                     )",
                    &[&completed_before_ms],
                )?;
                let job_events_deleted = transaction.execute(
                    "DELETE FROM orchestrator_job_events
                     WHERE job_id IN (
                       SELECT job_id FROM orchestrator_jobs
                       WHERE status IN ('SUCCEEDED', 'FAILED', 'CANCELLED', 'NEEDS_ATTENTION')
                         AND NULLIF(payload->>'completed_at_ms', '')::BIGINT < $1
                     )",
                    &[&completed_before_ms],
                )?;
                Ok((operation_logs_deleted, job_events_deleted))
            })?;
        let idempotency_records_deleted = self.purge_expired_idempotency(now_ms)?;
        Ok(HistoryRetentionReport {
            operation_logs_deleted,
            job_events_deleted,
            idempotency_records_deleted,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::params;

    #[test]
    fn sqlite_prunes_only_verbose_terminal_history_and_keeps_audit_and_resources() {
        let directory = tempfile::tempdir().unwrap();
        let store = SqliteOrchestratorStore::open(directory.path().join("retention.db")).unwrap();
        let connection = store.connection().unwrap();
        connection
            .execute(
                "INSERT INTO orchestrator_durable_operations(operation_id, revision, status, payload, created_at_ms, updated_at_ms) VALUES (?1, 1, ?2, ?3, 1, ?4)",
                params!["op-old", "SUCCEEDED", r#"{"operation_id":"op-old"}"#, 10],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO orchestrator_durable_operations(operation_id, revision, status, payload, created_at_ms, updated_at_ms) VALUES (?1, 1, ?2, ?3, 1, ?4)",
                params!["op-running", "RUNNING", r#"{"operation_id":"op-running"}"#, 10],
            )
            .unwrap();
        for operation_id in ["op-old", "op-running"] {
            connection
                .execute(
                    "INSERT INTO orchestrator_operation_logs_v2(operation_id, payload) VALUES (?1, '{}')",
                    [operation_id],
                )
                .unwrap();
        }
        for (job_id, status, completed_at_ms) in [
            ("job-old", "SUCCEEDED", Some(10_i64)),
            ("job-live", "LEASED", None),
        ] {
            let payload = serde_json::json!({
                "job_id": job_id,
                "completed_at_ms": completed_at_ms,
            })
            .to_string();
            connection
                .execute(
                    "INSERT INTO orchestrator_jobs(job_id, operation_id, node_id, idempotency_key, payload_sha256, status, available_at_ms, created_at_ms, payload) VALUES (?1, ?2, 'node-1', ?3, ?4, ?5, 0, 0, ?6)",
                    params![
                        job_id,
                        if job_id == "job-old" { "op-old" } else { "op-running" },
                        format!("idem-{job_id}"),
                        format!("sha256:{}", "0".repeat(64)),
                        status,
                        payload,
                    ],
                )
                .unwrap();
            connection
                .execute(
                    "INSERT INTO orchestrator_job_events(job_id, sequence, payload, created_at_ms) VALUES (?1, 1, '{}', 1)",
                    [job_id],
                )
                .unwrap();
        }
        connection
            .execute(
                "INSERT INTO orchestrator_idempotency(scope, idempotency_key, request_sha256, state, response_status, response_content_type, response_headers, response_body, started_at_ms, completed_at_ms, expires_at_ms) VALUES ('scope', 'idempotency-old', ?1, 'COMPLETED', 200, 'application/json', '{}', '{}', 1, 2, 3)",
                [format!("sha256:{}", "1".repeat(64))],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO orchestrator_audit_log(request_id, actor, action, resource, idempotency_key, request_digest, outcome, response_status, timestamp_ms) VALUES ('req-1', 'admin', 'operation.apply', 'op-old', 'idempotency-old', ?1, 'SUCCEEDED', 200, 2)",
                [format!("sha256:{}", "2".repeat(64))],
            )
            .unwrap();
        drop(connection);

        let report = store.purge_terminal_history(100, 100).unwrap();
        assert_eq!(report.operation_logs_deleted, 1);
        assert_eq!(report.job_events_deleted, 1);
        assert_eq!(report.idempotency_records_deleted, 1);

        let connection = store.connection().unwrap();
        let operation_count: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM orchestrator_durable_operations",
                [],
                |row| row.get(0),
            )
            .unwrap();
        let remaining_logs: Vec<String> = connection
            .prepare(
                "SELECT operation_id FROM orchestrator_operation_logs_v2 ORDER BY operation_id",
            )
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        let remaining_events: Vec<String> = connection
            .prepare("SELECT job_id FROM orchestrator_job_events ORDER BY job_id")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        let audit_count: i64 = connection
            .query_row("SELECT COUNT(*) FROM orchestrator_audit_log", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(
            operation_count, 2,
            "retention must keep Operation resources"
        );
        assert_eq!(remaining_logs, vec!["op-running"]);
        assert_eq!(remaining_events, vec!["job-live"]);
        assert_eq!(
            audit_count, 1,
            "retention must never prune append-only audit"
        );
    }
}
