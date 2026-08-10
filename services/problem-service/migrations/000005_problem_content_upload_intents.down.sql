ALTER TABLE problem_artifact_upload_intents
    ADD CONSTRAINT chk_problem_artifact_intent_uri_v1
    CHECK (
        artifact_uri ~ '^storage://[a-z0-9][a-z0-9.-]{1,62}/package-sha256-[a-f0-9]{64}\.zip$'
    ) NOT VALID;

-- A downgrade is intentionally refused while v2 content-object intents
-- remain; validating the narrower constraint makes that failure explicit.
ALTER TABLE problem_artifact_upload_intents
    VALIDATE CONSTRAINT chk_problem_artifact_intent_uri_v1;

ALTER TABLE problem_artifact_upload_intents
    DROP CONSTRAINT chk_problem_artifact_intent_uri;

ALTER TABLE problem_artifact_upload_intents
    RENAME CONSTRAINT chk_problem_artifact_intent_uri_v1
    TO chk_problem_artifact_intent_uri;

ALTER TABLE problem_artifact_upload_intents
    ADD CONSTRAINT chk_problem_artifact_intent_size_v1
    CHECK (artifact_size_bytes > 0) NOT VALID;

ALTER TABLE problem_artifact_upload_intents
    VALIDATE CONSTRAINT chk_problem_artifact_intent_size_v1;

ALTER TABLE problem_artifact_upload_intents
    DROP CONSTRAINT chk_problem_artifact_intent_size;

ALTER TABLE problem_artifact_upload_intents
    RENAME CONSTRAINT chk_problem_artifact_intent_size_v1
    TO chk_problem_artifact_intent_size;
