ALTER TABLE problems
    ADD COLUMN IF NOT EXISTS aggregate_version BIGINT NOT NULL DEFAULT 0,
    ADD COLUMN IF NOT EXISTS package_revision BIGINT NOT NULL DEFAULT 0,
    ADD COLUMN IF NOT EXISTS package_artifact_uri TEXT NOT NULL DEFAULT '',
    ADD COLUMN IF NOT EXISTS package_artifact_sha256 TEXT NOT NULL DEFAULT '',
    ADD COLUMN IF NOT EXISTS package_artifact_size_bytes BIGINT NOT NULL DEFAULT 0;

CREATE INDEX IF NOT EXISTS idx_problems_package_artifact_uri
    ON problems(package_artifact_uri)
    WHERE package_artifact_uri <> '';

CREATE TABLE IF NOT EXISTS problem_package_revisions (
    problem_id BIGINT NOT NULL REFERENCES problems(id) ON DELETE CASCADE,
    package_revision BIGINT NOT NULL,
    aggregate_version BIGINT NOT NULL,
    artifact_uri TEXT NOT NULL,
    artifact_sha256 TEXT NOT NULL,
    artifact_size_bytes BIGINT NOT NULL,
    manifest_sha256 TEXT NOT NULL DEFAULT '',
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    PRIMARY KEY(problem_id, package_revision),
    CONSTRAINT chk_problem_package_revision_positive CHECK (package_revision > 0),
    CONSTRAINT chk_problem_package_aggregate_positive CHECK (aggregate_version > 0),
    CONSTRAINT chk_problem_package_artifact_size CHECK (artifact_size_bytes > 0)
);

DROP INDEX IF EXISTS uq_problem_package_revisions_digest;
CREATE INDEX IF NOT EXISTS idx_problem_package_revisions_digest
    ON problem_package_revisions(problem_id, artifact_sha256);
CREATE INDEX IF NOT EXISTS idx_problem_package_revisions_artifact_uri
    ON problem_package_revisions(artifact_uri);

CREATE TABLE IF NOT EXISTS integration_outbox (
    sequence BIGSERIAL PRIMARY KEY,
    event_id TEXT NOT NULL UNIQUE,
    aggregate_type TEXT NOT NULL,
    aggregate_id TEXT NOT NULL,
    aggregate_version BIGINT NOT NULL,
    event_type TEXT NOT NULL,
    payload JSONB NOT NULL,
    occurred_at TIMESTAMPTZ NOT NULL,
    available_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    attempt_count INT NOT NULL DEFAULT 0,
    lease_owner TEXT,
    lease_until TIMESTAMPTZ,
    published_at TIMESTAMPTZ,
    last_error TEXT NOT NULL DEFAULT '',
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    CONSTRAINT chk_integration_outbox_version CHECK (aggregate_version > 0),
    UNIQUE(aggregate_type, aggregate_id, aggregate_version, event_type)
);

CREATE INDEX IF NOT EXISTS idx_integration_outbox_pending
    ON integration_outbox(available_at, sequence)
    WHERE published_at IS NULL;

CREATE INDEX IF NOT EXISTS idx_integration_outbox_lease
    ON integration_outbox(lease_until)
    WHERE published_at IS NULL;
