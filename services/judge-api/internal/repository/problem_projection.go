package repository

import (
	"context"
	"fmt"
	"strings"

	"ojos-shared/eventing"

	"github.com/jackc/pgx/v5"
)

func ApplyProblemProjection(ctx context.Context, tx pgx.Tx, envelope eventing.Envelope) error {
	if err := envelope.Validate(); err != nil {
		return err
	}
	switch envelope.Type {
	case eventing.ProblemSnapshotV1:
		snapshot, err := eventing.DecodeProblemSnapshotData(envelope)
		if err != nil {
			return err
		}
		_, err = tx.Exec(ctx, `
INSERT INTO problems(
    id,
    package_dir,
    status,
    visibility,
    created_by,
    updated_at,
    aggregate_version,
    package_revision,
    package_artifact_uri,
    package_artifact_sha256,
    package_artifact_size_bytes,
    manifest_sha256,
    projected_event_id,
    source_updated_at,
    deleted,
    problem_no,
    title,
    problem_type,
    time_limit_ms,
    memory_limit_mb
)
VALUES($1, '', $2, $3, $4, NOW(), $5, $6, $7, $8, $9, $10, $11, $12, FALSE, $13, $14, $15, $16, $17)
ON CONFLICT(id)
DO UPDATE SET
    status = EXCLUDED.status,
    visibility = EXCLUDED.visibility,
    created_by = EXCLUDED.created_by,
    updated_at = NOW(),
    aggregate_version = EXCLUDED.aggregate_version,
    package_revision = EXCLUDED.package_revision,
    package_artifact_uri = EXCLUDED.package_artifact_uri,
    package_artifact_sha256 = EXCLUDED.package_artifact_sha256,
    package_artifact_size_bytes = EXCLUDED.package_artifact_size_bytes,
    manifest_sha256 = EXCLUDED.manifest_sha256,
    projected_event_id = EXCLUDED.projected_event_id,
    source_updated_at = EXCLUDED.source_updated_at,
    deleted = FALSE,
    problem_no = EXCLUDED.problem_no,
    title = EXCLUDED.title,
    problem_type = EXCLUDED.problem_type,
    time_limit_ms = EXCLUDED.time_limit_ms,
    memory_limit_mb = EXCLUDED.memory_limit_mb
WHERE problems.aggregate_version < EXCLUDED.aggregate_version
`, snapshot.ProblemID, snapshot.Status, snapshot.Visibility, snapshot.CreatedBy, snapshot.AggregateVersion, snapshot.PackageRevision, snapshot.PackageArtifact.URI, strings.ToLower(snapshot.PackageArtifact.SHA256), snapshot.PackageArtifact.SizeBytes, snapshot.ManifestSHA256, envelope.ID, snapshot.SourceUpdatedAtUTC, snapshot.ProblemNo, snapshot.Title, snapshot.ProblemType, snapshot.TimeLimitMS, snapshot.MemoryLimitMB)
		return err

	case eventing.ProblemDeletedV1:
		deleted, err := eventing.DecodeProblemDeletedData(envelope)
		if err != nil {
			return err
		}
		_, err = tx.Exec(ctx, `
INSERT INTO problems(
    id, package_dir, status, visibility, created_by, updated_at,
    aggregate_version, projected_event_id, deleted
)
VALUES($1, '', 'archived', 'private', 0, NOW(), $2, $3, TRUE)
ON CONFLICT(id)
DO UPDATE SET
    status = 'archived',
    visibility = 'private',
    updated_at = NOW(),
    aggregate_version = EXCLUDED.aggregate_version,
    projected_event_id = EXCLUDED.projected_event_id,
    deleted = TRUE
WHERE problems.aggregate_version < EXCLUDED.aggregate_version
`, deleted.ProblemID, deleted.AggregateVersion, envelope.ID)
		return err
	default:
		return fmt.Errorf("unsupported problem projection event type %q", envelope.Type)
	}
}
