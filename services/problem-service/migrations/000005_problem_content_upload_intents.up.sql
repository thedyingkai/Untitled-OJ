-- Expand the durable upload-intent ledger to cover immutable authoring-file
-- objects as well as deterministic package archives. Add and validate the
-- broader constraint before removing the v1 constraint so existing writers
-- remain protected throughout an online migration.
ALTER TABLE problem_artifact_upload_intents
    ADD CONSTRAINT chk_problem_artifact_intent_uri_v2
    CHECK (
        artifact_uri ~ '^storage://[a-z0-9][a-z0-9.-]{1,62}/(package-sha256-[a-f0-9]{64}\.zip|problem-[1-9][0-9]*-objects-sha256-[a-f0-9]{64})$'
    ) NOT VALID;

ALTER TABLE problem_artifact_upload_intents
    VALIDATE CONSTRAINT chk_problem_artifact_intent_uri_v2;

ALTER TABLE problem_artifact_upload_intents
    DROP CONSTRAINT chk_problem_artifact_intent_uri;

ALTER TABLE problem_artifact_upload_intents
    RENAME CONSTRAINT chk_problem_artifact_intent_uri_v2
    TO chk_problem_artifact_intent_uri;

ALTER TABLE problem_artifact_upload_intents
    ADD CONSTRAINT chk_problem_artifact_intent_size_v2
    CHECK (
        (artifact_uri ~ '/package-sha256-[a-f0-9]{64}\.zip$' AND artifact_size_bytes > 0)
        OR
        (artifact_uri ~ '/problem-[1-9][0-9]*-objects-sha256-[a-f0-9]{64}$' AND artifact_size_bytes >= 0)
    ) NOT VALID;

ALTER TABLE problem_artifact_upload_intents
    VALIDATE CONSTRAINT chk_problem_artifact_intent_size_v2;

ALTER TABLE problem_artifact_upload_intents
    DROP CONSTRAINT chk_problem_artifact_intent_size;

ALTER TABLE problem_artifact_upload_intents
    RENAME CONSTRAINT chk_problem_artifact_intent_size_v2
    TO chk_problem_artifact_intent_size;
