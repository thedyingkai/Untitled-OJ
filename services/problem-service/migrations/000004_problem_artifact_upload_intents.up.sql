CREATE TABLE IF NOT EXISTS problem_artifact_upload_intents (
    artifact_uri TEXT PRIMARY KEY,
    artifact_sha256 TEXT NOT NULL,
    artifact_size_bytes BIGINT NOT NULL,
    status TEXT NOT NULL DEFAULT 'PENDING',
    retry_after TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    claim_token TEXT,
    claim_until TIMESTAMPTZ,
    attempt_count INT NOT NULL DEFAULT 0,
    last_error TEXT NOT NULL DEFAULT '',
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    CONSTRAINT chk_problem_artifact_intent_uri
        CHECK (artifact_uri ~ '^storage://[a-z0-9][a-z0-9.-]{1,62}/package-sha256-[a-f0-9]{64}\.zip$'),
    CONSTRAINT chk_problem_artifact_intent_sha256
        CHECK (artifact_sha256 ~ '^[a-f0-9]{64}$'),
    CONSTRAINT chk_problem_artifact_intent_size CHECK (artifact_size_bytes > 0),
    CONSTRAINT chk_problem_artifact_intent_status CHECK (status IN ('PENDING', 'DELETING')),
    CONSTRAINT chk_problem_artifact_intent_claim CHECK (
        (status = 'PENDING' AND claim_token IS NULL AND claim_until IS NULL)
        OR
        (status = 'DELETING' AND claim_token IS NOT NULL AND claim_until IS NOT NULL)
    )
);

CREATE INDEX IF NOT EXISTS idx_problem_artifact_intents_gc
    ON problem_artifact_upload_intents(retry_after, updated_at, artifact_uri);

CREATE INDEX IF NOT EXISTS idx_problem_artifact_intents_claim
    ON problem_artifact_upload_intents(claim_until)
    WHERE status = 'DELETING';
