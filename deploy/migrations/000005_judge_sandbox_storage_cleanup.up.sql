DROP INDEX IF EXISTS idx_submission_cases_submission_id;
DROP TABLE IF EXISTS submission_cases;

DROP INDEX IF EXISTS idx_test_cases_problem_id;
DROP TABLE IF EXISTS test_cases;

ALTER TABLE submissions
    DROP COLUMN IF EXISTS code;

ALTER TABLE submissions
    ADD COLUMN IF NOT EXISTS code_path TEXT NOT NULL DEFAULT '',
    ADD COLUMN IF NOT EXISTS code_sha256 TEXT NOT NULL DEFAULT '',
    ADD COLUMN IF NOT EXISTS result_path TEXT NOT NULL DEFAULT '',
    ADD COLUMN IF NOT EXISTS judged_at TIMESTAMPTZ,
    ADD COLUMN IF NOT EXISTS cancelled_at TIMESTAMPTZ,
    ADD COLUMN IF NOT EXISTS cancelled_by BIGINT REFERENCES users(id) ON DELETE SET NULL,
    ADD COLUMN IF NOT EXISTS cancel_reason TEXT NOT NULL DEFAULT '';

CREATE INDEX IF NOT EXISTS idx_submissions_problem_status
    ON submissions(problem_id, status);

CREATE INDEX IF NOT EXISTS idx_submissions_user_status
    ON submissions(user_id, status);

CREATE INDEX IF NOT EXISTS idx_submissions_code_sha256
    ON submissions(code_sha256);

CREATE INDEX IF NOT EXISTS idx_submissions_judged_at
    ON submissions(judged_at);

CREATE INDEX IF NOT EXISTS idx_submissions_cancelled_at
    ON submissions(cancelled_at);