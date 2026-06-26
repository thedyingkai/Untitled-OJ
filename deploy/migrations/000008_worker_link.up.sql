CREATE TABLE IF NOT EXISTS judge_workers (
    worker_id TEXT PRIMARY KEY,
    worker_name TEXT NOT NULL DEFAULT '',
    hostname TEXT NOT NULL DEFAULT '',
    version TEXT NOT NULL DEFAULT '',
    capabilities JSONB NOT NULL DEFAULT '{}'::jsonb,
    supported_languages TEXT[] NOT NULL DEFAULT '{}'::text[],
    max_concurrency INT NOT NULL DEFAULT 1,
    running_count INT NOT NULL DEFAULT 0,
    status TEXT NOT NULL DEFAULT 'ONLINE',
    drain BOOLEAN NOT NULL DEFAULT FALSE,
    last_seen TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    registered_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    CONSTRAINT chk_judge_workers_status
        CHECK (status IN ('ONLINE', 'OFFLINE', 'DRAINING'))
);

CREATE TABLE IF NOT EXISTS judge_tasks (
    id BIGSERIAL PRIMARY KEY,
    task_id TEXT NOT NULL UNIQUE,
    submission_id BIGINT NOT NULL UNIQUE REFERENCES submissions(id) ON DELETE CASCADE,
    problem_id BIGINT NOT NULL REFERENCES problems(id) ON DELETE CASCADE,
    language TEXT NOT NULL,
    worker_id TEXT REFERENCES judge_workers(worker_id) ON DELETE SET NULL,
    lease_version INT NOT NULL DEFAULT 0,
    lease_expires_at TIMESTAMPTZ,
    heartbeat_at TIMESTAMPTZ,
    attempt INT NOT NULL DEFAULT 0,
    status TEXT NOT NULL DEFAULT 'PENDING',
    error_message TEXT NOT NULL DEFAULT '',
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    CONSTRAINT chk_judge_tasks_status
        CHECK (status IN ('PENDING', 'RUNNING', 'SUCCEEDED', 'FAILED', 'CANCELLED'))
);

CREATE INDEX IF NOT EXISTS idx_judge_workers_status_last_seen
    ON judge_workers(status, last_seen);

CREATE INDEX IF NOT EXISTS idx_judge_tasks_status_id
    ON judge_tasks(status, id);

CREATE INDEX IF NOT EXISTS idx_judge_tasks_submission_id
    ON judge_tasks(submission_id);

CREATE INDEX IF NOT EXISTS idx_judge_tasks_worker_status
    ON judge_tasks(worker_id, status);

CREATE INDEX IF NOT EXISTS idx_judge_tasks_lease_expires_at
    ON judge_tasks(lease_expires_at)
    WHERE status = 'RUNNING';
