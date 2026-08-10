DROP TABLE IF EXISTS integration_outbox;
DROP TABLE IF EXISTS problem_package_revisions;

DROP INDEX IF EXISTS idx_problems_package_artifact_uri;

ALTER TABLE problems
    DROP COLUMN IF EXISTS package_artifact_size_bytes,
    DROP COLUMN IF EXISTS package_artifact_sha256,
    DROP COLUMN IF EXISTS package_artifact_uri,
    DROP COLUMN IF EXISTS package_revision,
    DROP COLUMN IF EXISTS aggregate_version;
