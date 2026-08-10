DROP TABLE IF EXISTS judge_result_outbox;
DROP TABLE IF EXISTS judge_task_report_receipts;

ALTER TABLE judge_tasks
    DROP COLUMN IF EXISTS result_payload_sha256;
