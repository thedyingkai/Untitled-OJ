ALTER TABLE problems
    ADD COLUMN IF NOT EXISTS aggregate_version BIGINT NOT NULL DEFAULT 0,
    ADD COLUMN IF NOT EXISTS package_revision BIGINT NOT NULL DEFAULT 0,
    ADD COLUMN IF NOT EXISTS package_artifact_uri TEXT NOT NULL DEFAULT '',
    ADD COLUMN IF NOT EXISTS package_artifact_sha256 TEXT NOT NULL DEFAULT '',
    ADD COLUMN IF NOT EXISTS package_artifact_size_bytes BIGINT NOT NULL DEFAULT 0,
    ADD COLUMN IF NOT EXISTS manifest_sha256 TEXT NOT NULL DEFAULT '',
    ADD COLUMN IF NOT EXISTS projected_event_id TEXT NOT NULL DEFAULT '',
    ADD COLUMN IF NOT EXISTS source_updated_at TIMESTAMPTZ,
    ADD COLUMN IF NOT EXISTS deleted BOOLEAN NOT NULL DEFAULT FALSE,
    ADD COLUMN IF NOT EXISTS problem_no TEXT NOT NULL DEFAULT '',
    ADD COLUMN IF NOT EXISTS title TEXT NOT NULL DEFAULT '',
    ADD COLUMN IF NOT EXISTS problem_type TEXT NOT NULL DEFAULT 'traditional',
    ADD COLUMN IF NOT EXISTS time_limit_ms INT NOT NULL DEFAULT 1000,
    ADD COLUMN IF NOT EXISTS memory_limit_mb INT NOT NULL DEFAULT 256;

ALTER TABLE submissions
    ADD COLUMN IF NOT EXISTS problem_aggregate_version BIGINT NOT NULL DEFAULT 0,
    ADD COLUMN IF NOT EXISTS problem_package_revision BIGINT NOT NULL DEFAULT 0,
    ADD COLUMN IF NOT EXISTS problem_artifact_uri TEXT NOT NULL DEFAULT '',
    ADD COLUMN IF NOT EXISTS problem_artifact_sha256 TEXT NOT NULL DEFAULT '',
    ADD COLUMN IF NOT EXISTS problem_artifact_size_bytes BIGINT NOT NULL DEFAULT 0;

CREATE TABLE IF NOT EXISTS integration_inbox (
    consumer_name TEXT NOT NULL,
    event_id TEXT NOT NULL,
    event_type TEXT NOT NULL,
    received_at TIMESTAMPTZ NOT NULL,
    processed_at TIMESTAMPTZ,
    PRIMARY KEY(consumer_name, event_id)
);

CREATE TABLE IF NOT EXISTS integration_dead_letters (
    consumer_name TEXT NOT NULL,
    event_id TEXT NOT NULL,
    stream_entry_id TEXT NOT NULL,
    payload JSONB NOT NULL,
    attempts INT NOT NULL DEFAULT 1,
    last_error TEXT NOT NULL,
    first_failed_at TIMESTAMPTZ NOT NULL,
    last_failed_at TIMESTAMPTZ NOT NULL,
    PRIMARY KEY(consumer_name, event_id)
);

CREATE INDEX IF NOT EXISTS idx_judge_problem_projection_version
    ON problems(aggregate_version);
CREATE INDEX IF NOT EXISTS idx_judge_problem_projection_deleted
    ON problems(deleted);
CREATE INDEX IF NOT EXISTS idx_submission_problem_package_revision
    ON submissions(problem_id, problem_package_revision);
CREATE INDEX IF NOT EXISTS idx_submission_problem_artifact_uri
    ON submissions(problem_artifact_uri)
    WHERE problem_artifact_uri <> '';
CREATE INDEX IF NOT EXISTS idx_integration_dead_letters_last_failed
    ON integration_dead_letters(last_failed_at);
