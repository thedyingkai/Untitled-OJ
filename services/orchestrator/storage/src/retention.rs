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
