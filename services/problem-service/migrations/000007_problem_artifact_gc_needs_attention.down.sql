DO $ojos$
BEGIN
    IF EXISTS (
        SELECT 1
        FROM problem_artifact_upload_intents
        WHERE status = 'NEEDS_ATTENTION'
    ) THEN
        RAISE EXCEPTION 'cannot remove artifact GC NEEDS_ATTENTION state while terminal intents exist';
    END IF;
END
$ojos$;

DROP TABLE problem_artifact_gc_operator_actions;
DROP FUNCTION reject_problem_artifact_gc_operator_action_mutation();

ALTER TABLE problem_artifact_upload_intents
    DROP CONSTRAINT chk_problem_artifact_intent_status,
    DROP CONSTRAINT chk_problem_artifact_intent_claim,
    DROP CONSTRAINT chk_problem_artifact_intent_attention_time,
    DROP CONSTRAINT chk_problem_artifact_intent_failure_count;

ALTER TABLE problem_artifact_upload_intents
    ADD CONSTRAINT chk_problem_artifact_intent_status
    CHECK (status IN ('PENDING', 'DELETING')),
    ADD CONSTRAINT chk_problem_artifact_intent_claim CHECK (
        (status = 'PENDING' AND claim_token IS NULL AND claim_until IS NULL)
        OR
        (status = 'DELETING' AND claim_token IS NOT NULL AND claim_until IS NOT NULL)
    );

ALTER TABLE problem_artifact_upload_intents
    DROP COLUMN last_operator_retry_at,
    DROP COLUMN last_operator_retry_reason,
    DROP COLUMN needs_attention_at,
    DROP COLUMN failure_count;
