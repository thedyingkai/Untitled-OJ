CREATE TABLE IF NOT EXISTS submissions (
    id BIGSERIAL PRIMARY KEY,
    problem_id BIGINT NOT NULL,
    user_id BIGINT NOT NULL,
    language TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'PENDING',
    score INT NOT NULL DEFAULT 0,
    time_ms INT NOT NULL DEFAULT 0,
    memory_kb INT NOT NULL DEFAULT 0,
    message TEXT,
    code_path TEXT NOT NULL DEFAULT '',
    code_sha256 TEXT NOT NULL DEFAULT '',
    result_path TEXT NOT NULL DEFAULT '',
    judged_at TIMESTAMPTZ,
    cancelled_at TIMESTAMPTZ,
    cancelled_by BIGINT,
    cancel_reason TEXT NOT NULL DEFAULT '',
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_submissions_problem_id ON submissions(problem_id);
CREATE INDEX IF NOT EXISTS idx_submissions_user_id ON submissions(user_id);
CREATE INDEX IF NOT EXISTS idx_submissions_status ON submissions(status);
CREATE INDEX IF NOT EXISTS idx_submissions_problem_status ON submissions(problem_id, status);
CREATE INDEX IF NOT EXISTS idx_submissions_user_status ON submissions(user_id, status);
CREATE INDEX IF NOT EXISTS idx_submissions_code_sha256 ON submissions(code_sha256);
CREATE INDEX IF NOT EXISTS idx_submissions_judged_at ON submissions(judged_at);
CREATE INDEX IF NOT EXISTS idx_submissions_cancelled_at ON submissions(cancelled_at);
