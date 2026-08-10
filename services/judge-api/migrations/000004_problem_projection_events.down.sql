DROP TABLE IF EXISTS integration_dead_letters;
DROP TABLE IF EXISTS integration_inbox;

DROP INDEX IF EXISTS idx_submission_problem_package_revision;
DROP INDEX IF EXISTS idx_submission_problem_artifact_uri;
ALTER TABLE submissions
    DROP COLUMN IF EXISTS problem_artifact_size_bytes,
    DROP COLUMN IF EXISTS problem_artifact_sha256,
    DROP COLUMN IF EXISTS problem_artifact_uri,
    DROP COLUMN IF EXISTS problem_package_revision,
    DROP COLUMN IF EXISTS problem_aggregate_version;

DROP INDEX IF EXISTS idx_judge_problem_projection_deleted;
DROP INDEX IF EXISTS idx_judge_problem_projection_version;
ALTER TABLE problems
    DROP COLUMN IF EXISTS memory_limit_mb,
    DROP COLUMN IF EXISTS time_limit_ms,
    DROP COLUMN IF EXISTS problem_type,
    DROP COLUMN IF EXISTS title,
    DROP COLUMN IF EXISTS problem_no,
    DROP COLUMN IF EXISTS deleted,
    DROP COLUMN IF EXISTS source_updated_at,
    DROP COLUMN IF EXISTS projected_event_id,
    DROP COLUMN IF EXISTS manifest_sha256,
    DROP COLUMN IF EXISTS package_artifact_size_bytes,
    DROP COLUMN IF EXISTS package_artifact_sha256,
    DROP COLUMN IF EXISTS package_artifact_uri,
    DROP COLUMN IF EXISTS package_revision,
    DROP COLUMN IF EXISTS aggregate_version;
