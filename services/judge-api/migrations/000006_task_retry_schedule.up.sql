ALTER TABLE judge_tasks
    ADD COLUMN IF NOT EXISTS available_at TIMESTAMPTZ NOT NULL DEFAULT NOW();

UPDATE judge_tasks
SET available_at = NOW()
WHERE available_at IS NULL;

CREATE INDEX IF NOT EXISTS idx_judge_tasks_pending_available
    ON judge_tasks(available_at, id)
    WHERE status = 'PENDING';
