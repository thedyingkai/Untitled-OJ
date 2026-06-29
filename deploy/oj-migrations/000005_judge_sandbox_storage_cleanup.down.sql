DROP INDEX IF EXISTS idx_submissions_cancelled_at;
DROP INDEX IF EXISTS idx_submissions_judged_at;
DROP INDEX IF EXISTS idx_submissions_code_sha256;
DROP INDEX IF EXISTS idx_submissions_user_status;
DROP INDEX IF EXISTS idx_submissions_problem_status;

ALTER TABLE submissions
    DROP COLUMN IF EXISTS cancel_reason,
    DROP COLUMN IF EXISTS cancelled_by,
    DROP COLUMN IF EXISTS cancelled_at,
    DROP COLUMN IF EXISTS judged_at,
    DROP COLUMN IF EXISTS result_path,
    DROP COLUMN IF EXISTS code_sha256,
    DROP COLUMN IF EXISTS code_path;

ALTER TABLE submissions
    ADD COLUMN IF NOT EXISTS code TEXT NOT NULL DEFAULT '';

CREATE TABLE IF NOT EXISTS test_cases (
                                          id BIGSERIAL PRIMARY KEY,
                                          problem_id BIGINT NOT NULL REFERENCES problems(id) ON DELETE CASCADE,
                                          input TEXT NOT NULL,
                                          output TEXT NOT NULL,
                                          score INT NOT NULL DEFAULT 100,
                                          created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_test_cases_problem_id
    ON test_cases(problem_id);

CREATE TABLE IF NOT EXISTS submission_cases (
                                                id BIGSERIAL PRIMARY KEY,
                                                submission_id BIGINT NOT NULL REFERENCES submissions(id) ON DELETE CASCADE,
                                                test_case_id BIGINT NOT NULL REFERENCES test_cases(id),
                                                status TEXT NOT NULL,
                                                time_ms INT NOT NULL DEFAULT 0,
                                                memory_kb INT NOT NULL DEFAULT 0,
                                                message TEXT,
                                                created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_submission_cases_submission_id
    ON submission_cases(submission_id);