CREATE TABLE IF NOT EXISTS problems (
    id BIGINT PRIMARY KEY,
    package_dir TEXT NOT NULL DEFAULT '',
    status TEXT NOT NULL DEFAULT 'ready',
    visibility TEXT NOT NULL DEFAULT 'public',
    created_by BIGINT NOT NULL DEFAULT 0,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_judge_problem_meta_status
    ON problems(status);
CREATE INDEX IF NOT EXISTS idx_judge_problem_meta_visibility
    ON problems(visibility);
