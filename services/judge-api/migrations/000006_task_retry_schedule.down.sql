DROP INDEX IF EXISTS idx_judge_tasks_pending_available;

ALTER TABLE judge_tasks
    DROP COLUMN IF EXISTS available_at;
