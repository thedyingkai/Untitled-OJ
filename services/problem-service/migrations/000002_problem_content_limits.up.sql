ALTER TABLE problems
    ADD COLUMN IF NOT EXISTS problem_no TEXT NOT NULL DEFAULT '',
    ADD COLUMN IF NOT EXISTS statement_format TEXT NOT NULL DEFAULT 'markdown+latex',
    ADD COLUMN IF NOT EXISTS solution TEXT NOT NULL DEFAULT '',
    ADD COLUMN IF NOT EXISTS solution_format TEXT NOT NULL DEFAULT 'markdown+latex';

CREATE UNIQUE INDEX IF NOT EXISTS uq_problems_problem_no
    ON problems(problem_no)
    WHERE problem_no <> '';

CREATE TABLE IF NOT EXISTS problem_language_limits (
    problem_id BIGINT NOT NULL REFERENCES problems(id) ON DELETE CASCADE,
    language TEXT NOT NULL,
    time_limit_ms INT NOT NULL,
    memory_limit_mb INT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    PRIMARY KEY(problem_id, language),
    CONSTRAINT chk_problem_language_limits_language CHECK (language <> ''),
    CONSTRAINT chk_problem_language_limits_time CHECK (time_limit_ms BETWEEN 1 AND 600000),
    CONSTRAINT chk_problem_language_limits_memory CHECK (memory_limit_mb BETWEEN 1 AND 65536)
);

CREATE INDEX IF NOT EXISTS idx_problem_language_limits_problem_id
    ON problem_language_limits(problem_id);
