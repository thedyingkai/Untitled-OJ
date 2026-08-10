-- Expand the upload-intent ledger with an operator-owned terminal state. The
-- old constraints remain active while the compatible v2 constraints validate,
-- so existing PENDING/DELETING writers stay protected throughout migration.
ALTER TABLE problem_artifact_upload_intents
    ADD COLUMN failure_count INT NOT NULL DEFAULT 0,
    ADD COLUMN needs_attention_at TIMESTAMPTZ,
    ADD COLUMN last_operator_retry_reason TEXT NOT NULL DEFAULT '',
    ADD COLUMN last_operator_retry_at TIMESTAMPTZ;

ALTER TABLE problem_artifact_upload_intents
    ADD CONSTRAINT chk_problem_artifact_intent_failure_count
    CHECK (failure_count >= 0) NOT VALID;

ALTER TABLE problem_artifact_upload_intents
    VALIDATE CONSTRAINT chk_problem_artifact_intent_failure_count;

ALTER TABLE problem_artifact_upload_intents
    ADD CONSTRAINT chk_problem_artifact_intent_status_v2
    CHECK (status IN ('PENDING', 'DELETING', 'NEEDS_ATTENTION')) NOT VALID;

ALTER TABLE problem_artifact_upload_intents
    VALIDATE CONSTRAINT chk_problem_artifact_intent_status_v2;

ALTER TABLE problem_artifact_upload_intents
    ADD CONSTRAINT chk_problem_artifact_intent_claim_v2
    CHECK (
        (status = 'PENDING' AND claim_token IS NULL AND claim_until IS NULL)
        OR
        (status = 'DELETING' AND claim_token IS NOT NULL AND claim_until IS NOT NULL)
        OR
        (status = 'NEEDS_ATTENTION' AND claim_token IS NULL AND claim_until IS NULL)
    ) NOT VALID;

ALTER TABLE problem_artifact_upload_intents
    VALIDATE CONSTRAINT chk_problem_artifact_intent_claim_v2;

ALTER TABLE problem_artifact_upload_intents
    ADD CONSTRAINT chk_problem_artifact_intent_attention_time
    CHECK (status <> 'NEEDS_ATTENTION' OR needs_attention_at IS NOT NULL) NOT VALID;

ALTER TABLE problem_artifact_upload_intents
    VALIDATE CONSTRAINT chk_problem_artifact_intent_attention_time;

ALTER TABLE problem_artifact_upload_intents
    DROP CONSTRAINT chk_problem_artifact_intent_status,
    DROP CONSTRAINT chk_problem_artifact_intent_claim;

ALTER TABLE problem_artifact_upload_intents
    RENAME CONSTRAINT chk_problem_artifact_intent_status_v2
    TO chk_problem_artifact_intent_status;

ALTER TABLE problem_artifact_upload_intents
    RENAME CONSTRAINT chk_problem_artifact_intent_claim_v2
    TO chk_problem_artifact_intent_claim;

CREATE TABLE problem_artifact_gc_operator_actions (
    action_id BIGSERIAL PRIMARY KEY,
    artifact_uri TEXT NOT NULL,
    action TEXT NOT NULL,
    actor TEXT NOT NULL,
    reason TEXT NOT NULL,
    previous_status TEXT NOT NULL,
    previous_failure_count INT NOT NULL,
    previous_last_error TEXT NOT NULL,
    previous_needs_attention_at TIMESTAMPTZ NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    CONSTRAINT chk_problem_artifact_gc_action
        CHECK (action IN ('RETRY')),
    CONSTRAINT chk_problem_artifact_gc_action_actor
        CHECK (BTRIM(actor) <> '' AND LENGTH(actor) <= 255),
    CONSTRAINT chk_problem_artifact_gc_action_reason
        CHECK (BTRIM(reason) <> '' AND LENGTH(reason) <= 2000),
    CONSTRAINT chk_problem_artifact_gc_action_previous_status
        CHECK (previous_status = 'NEEDS_ATTENTION'),
    CONSTRAINT chk_problem_artifact_gc_action_previous_failures
        CHECK (previous_failure_count >= 1)
);

CREATE INDEX idx_problem_artifact_gc_operator_actions_uri
    ON problem_artifact_gc_operator_actions(artifact_uri, created_at, action_id);

CREATE FUNCTION reject_problem_artifact_gc_operator_action_mutation()
RETURNS trigger
LANGUAGE plpgsql
AS $ojos$
BEGIN
    RAISE EXCEPTION 'problem artifact GC operator audit is append-only';
END
$ojos$;

CREATE TRIGGER trg_problem_artifact_gc_operator_actions_append_only
BEFORE UPDATE OR DELETE ON problem_artifact_gc_operator_actions
FOR EACH ROW EXECUTE FUNCTION reject_problem_artifact_gc_operator_action_mutation();

CREATE TRIGGER trg_problem_artifact_gc_operator_actions_no_truncate
BEFORE TRUNCATE ON problem_artifact_gc_operator_actions
FOR EACH STATEMENT EXECUTE FUNCTION reject_problem_artifact_gc_operator_action_mutation();
