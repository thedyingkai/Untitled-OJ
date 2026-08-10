-- Add the durable, idempotent operator request surface without rewriting the
-- v7 terminal-state migration or ever disabling/mutating its append-only audit.
ALTER TABLE problem_artifact_upload_intents
    ADD COLUMN upload_completed_at TIMESTAMPTZ,
    ADD COLUMN manual_reconcile_requested_at TIMESTAMPTZ,
    ADD COLUMN last_failure_stage TEXT NOT NULL DEFAULT '',
    ADD COLUMN last_failure_kind TEXT NOT NULL DEFAULT '',
    ADD COLUMN last_failure_http_status INT,
    ADD COLUMN last_failure_provider_result TEXT NOT NULL DEFAULT '',
    ADD COLUMN last_failure_deterministic BOOLEAN NOT NULL DEFAULT FALSE;

ALTER TABLE problem_artifact_upload_intents
    ADD CONSTRAINT chk_problem_artifact_intent_failure_http_status
        CHECK (last_failure_http_status IS NULL OR last_failure_http_status BETWEEN 100 AND 599) NOT VALID,
    ADD CONSTRAINT chk_problem_artifact_intent_failure_kind
        CHECK (last_failure_kind IN ('', 'TRANSIENT', 'PROVIDER_HTTP', 'OBJECT_IDENTITY_MISMATCH', 'REFERENCE_IDENTITY_MISMATCH', 'REFERENCED_OBJECT_MISSING', 'LEDGER', 'DETERMINISTIC')) NOT VALID,
    ADD CONSTRAINT chk_problem_artifact_intent_manual_reconcile
        CHECK (
            manual_reconcile_requested_at IS NULL
            OR (status = 'PENDING' AND upload_completed_at IS NOT NULL)
        ) NOT VALID;

ALTER TABLE problem_artifact_upload_intents
    VALIDATE CONSTRAINT chk_problem_artifact_intent_failure_http_status;
ALTER TABLE problem_artifact_upload_intents
    VALIDATE CONSTRAINT chk_problem_artifact_intent_failure_kind;
ALTER TABLE problem_artifact_upload_intents
    VALIDATE CONSTRAINT chk_problem_artifact_intent_manual_reconcile;

ALTER TABLE problem_artifact_gc_operator_actions
    ADD COLUMN action_schema_version SMALLINT NOT NULL DEFAULT 1,
    ADD COLUMN idempotency_key TEXT,
    ADD COLUMN request_hash TEXT,
    ADD COLUMN artifact_sha256 TEXT NOT NULL DEFAULT '',
    ADD COLUMN artifact_size_bytes BIGINT NOT NULL DEFAULT 0,
    ADD COLUMN from_status TEXT NOT NULL DEFAULT 'NEEDS_ATTENTION',
    ADD COLUMN to_status TEXT NOT NULL DEFAULT 'PENDING',
    ADD COLUMN previous_last_failure_stage TEXT NOT NULL DEFAULT '',
    ADD COLUMN previous_last_failure_kind TEXT NOT NULL DEFAULT '',
    ADD COLUMN previous_last_failure_http_status INT,
    ADD COLUMN previous_last_failure_provider_result TEXT NOT NULL DEFAULT '',
    ADD COLUMN previous_last_failure_deterministic BOOLEAN NOT NULL DEFAULT FALSE;

ALTER TABLE problem_artifact_gc_operator_actions
    ALTER COLUMN previous_needs_attention_at DROP NOT NULL,
    DROP CONSTRAINT chk_problem_artifact_gc_action,
    DROP CONSTRAINT chk_problem_artifact_gc_action_previous_status,
    DROP CONSTRAINT chk_problem_artifact_gc_action_previous_failures;

ALTER TABLE problem_artifact_gc_operator_actions
    ADD CONSTRAINT chk_problem_artifact_gc_action_v2
        CHECK (
            (action_schema_version = 1 AND action = 'RETRY')
            OR (action_schema_version = 2 AND action IN ('RETRY', 'RECONCILE'))
        ),
    ADD CONSTRAINT chk_problem_artifact_gc_action_previous_status_v2
        CHECK (
            (action_schema_version = 1 AND previous_status = 'NEEDS_ATTENTION')
            OR (action_schema_version = 2 AND previous_status IN ('PENDING', 'NEEDS_ATTENTION'))
        ),
    ADD CONSTRAINT chk_problem_artifact_gc_action_previous_failures_v2
        CHECK (
            (action_schema_version = 1 AND previous_failure_count >= 1)
            OR (action_schema_version = 2 AND action = 'RETRY' AND previous_failure_count >= 1)
            OR (action_schema_version = 2 AND action = 'RECONCILE' AND previous_failure_count >= 0)
        ),
    ADD CONSTRAINT chk_problem_artifact_gc_action_transition_v2
        CHECK (
            action_schema_version = 1
            OR
            (action_schema_version = 2 AND action = 'RETRY' AND from_status = previous_status AND from_status = 'NEEDS_ATTENTION' AND to_status = 'PENDING')
            OR
            (action_schema_version = 2 AND action = 'RECONCILE' AND from_status = previous_status AND from_status = 'PENDING' AND to_status = 'PENDING')
        ),
    ADD CONSTRAINT chk_problem_artifact_gc_action_idempotency_v2
        CHECK (
            (action_schema_version = 1 AND idempotency_key IS NULL)
            OR (action_schema_version = 2 AND BTRIM(idempotency_key) <> '' AND LENGTH(idempotency_key) <= 255)
        ),
    ADD CONSTRAINT chk_problem_artifact_gc_action_request_hash_v2
        CHECK (
            (action_schema_version = 1 AND request_hash IS NULL)
            OR (action_schema_version = 2 AND request_hash ~ '^[a-f0-9]{64}$')
        ),
    ADD CONSTRAINT chk_problem_artifact_gc_action_identity_v2
        CHECK (
            action_schema_version = 1
            OR (action_schema_version = 2 AND artifact_sha256 ~ '^[a-f0-9]{64}$' AND artifact_size_bytes >= 0)
        ),
    ADD CONSTRAINT chk_problem_artifact_gc_action_failure_http_status_v2
        CHECK (previous_last_failure_http_status IS NULL OR previous_last_failure_http_status BETWEEN 100 AND 599),
    ADD CONSTRAINT chk_problem_artifact_gc_action_failure_kind_v2
        CHECK (previous_last_failure_kind IN ('', 'TRANSIENT', 'PROVIDER_HTTP', 'OBJECT_IDENTITY_MISMATCH', 'REFERENCE_IDENTITY_MISMATCH', 'REFERENCED_OBJECT_MISSING', 'LEDGER', 'DETERMINISTIC')),
    ADD CONSTRAINT chk_problem_artifact_gc_action_attention_snapshot_v2
        CHECK (action = 'RECONCILE' OR previous_needs_attention_at IS NOT NULL);

CREATE UNIQUE INDEX uq_problem_artifact_gc_operator_actions_idempotency
    ON problem_artifact_gc_operator_actions(idempotency_key)
    WHERE idempotency_key IS NOT NULL;
