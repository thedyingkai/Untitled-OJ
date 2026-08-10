DO $ojos$
BEGIN
    IF EXISTS (
        SELECT 1 FROM problem_artifact_gc_operator_actions WHERE action_schema_version = 2
    ) THEN
        RAISE EXCEPTION 'cannot remove artifact GC operator API migration while v2 audit rows exist';
    END IF;
END
$ojos$;

DROP INDEX IF EXISTS uq_problem_artifact_gc_operator_actions_idempotency;

ALTER TABLE problem_artifact_gc_operator_actions
    DROP CONSTRAINT chk_problem_artifact_gc_action_v2,
    DROP CONSTRAINT chk_problem_artifact_gc_action_previous_status_v2,
    DROP CONSTRAINT chk_problem_artifact_gc_action_previous_failures_v2,
    DROP CONSTRAINT chk_problem_artifact_gc_action_transition_v2,
    DROP CONSTRAINT chk_problem_artifact_gc_action_idempotency_v2,
    DROP CONSTRAINT chk_problem_artifact_gc_action_request_hash_v2,
    DROP CONSTRAINT chk_problem_artifact_gc_action_identity_v2,
    DROP CONSTRAINT chk_problem_artifact_gc_action_failure_http_status_v2,
    DROP CONSTRAINT chk_problem_artifact_gc_action_failure_kind_v2,
    DROP CONSTRAINT chk_problem_artifact_gc_action_attention_snapshot_v2,
    DROP COLUMN action_schema_version,
    DROP COLUMN idempotency_key,
    DROP COLUMN request_hash,
    DROP COLUMN artifact_sha256,
    DROP COLUMN artifact_size_bytes,
    DROP COLUMN from_status,
    DROP COLUMN to_status,
    DROP COLUMN previous_last_failure_stage,
    DROP COLUMN previous_last_failure_kind,
    DROP COLUMN previous_last_failure_http_status,
    DROP COLUMN previous_last_failure_provider_result,
    DROP COLUMN previous_last_failure_deterministic;

ALTER TABLE problem_artifact_gc_operator_actions
    ALTER COLUMN previous_needs_attention_at SET NOT NULL,
    ADD CONSTRAINT chk_problem_artifact_gc_action
        CHECK (action = 'RETRY'),
    ADD CONSTRAINT chk_problem_artifact_gc_action_previous_status
        CHECK (previous_status = 'NEEDS_ATTENTION'),
    ADD CONSTRAINT chk_problem_artifact_gc_action_previous_failures
        CHECK (previous_failure_count >= 1);

ALTER TABLE problem_artifact_upload_intents
    DROP CONSTRAINT chk_problem_artifact_intent_failure_http_status,
    DROP CONSTRAINT chk_problem_artifact_intent_failure_kind,
    DROP CONSTRAINT chk_problem_artifact_intent_manual_reconcile,
    DROP COLUMN upload_completed_at,
    DROP COLUMN manual_reconcile_requested_at,
    DROP COLUMN last_failure_stage,
    DROP COLUMN last_failure_kind,
    DROP COLUMN last_failure_http_status,
    DROP COLUMN last_failure_provider_result,
    DROP COLUMN last_failure_deterministic;
