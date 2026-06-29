DROP INDEX IF EXISTS idx_judge_tasks_lease_expires_at;
DROP INDEX IF EXISTS idx_judge_tasks_worker_status;
DROP INDEX IF EXISTS idx_judge_tasks_submission_id;
DROP INDEX IF EXISTS idx_judge_tasks_status_id;
DROP INDEX IF EXISTS idx_judge_workers_status_last_seen;

DROP TABLE IF EXISTS judge_tasks;
DROP TABLE IF EXISTS judge_workers;
