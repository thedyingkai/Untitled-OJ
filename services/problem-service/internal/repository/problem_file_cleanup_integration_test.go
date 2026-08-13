package repository

import (
	"context"
	"errors"
	"fmt"
	"os"
	"strings"
	"testing"
	"time"

	"ojos-problem-events/problemv1"
	"ojos-problem-service/internal/artifactgc"
	"ojos-problem-service/internal/packagefs"

	"github.com/jackc/pgx/v5"
	"github.com/jackc/pgx/v5/pgxpool"
)

func TestProblemFileCleanupCandidatesShareTheBusinessTransaction(t *testing.T) {
	ctx, repo, pool := setupProblemFileCleanupPostgres(t)

	replacedProblemID := int64(101)
	insertProblemLifecycleRow(t, ctx, pool, replacedProblemID)
	replaced := problemContentRef(replacedProblemID, "1", 11)
	replacement := problemContentRef(replacedProblemID, "2", 12)
	if err := repo.UpsertProblemFiles(ctx, replacedProblemID, []packagefs.IndexedFile{indexedArtifact("problem.yaml", replaced)}); err != nil {
		t.Fatal(err)
	}
	if err := repo.UpsertProblemFiles(ctx, replacedProblemID, []packagefs.IndexedFile{indexedArtifact("problem.yaml", replacement)}); err != nil {
		t.Fatal(err)
	}
	assertCleanupCandidate(t, ctx, pool, replaced, true)
	assertCleanupCandidate(t, ctx, pool, replacement, false)

	deletedFileProblemID := int64(102)
	insertProblemLifecycleRow(t, ctx, pool, deletedFileProblemID)
	deletedFile := problemContentRef(deletedFileProblemID, "3", 13)
	if err := repo.UpsertProblemFiles(ctx, deletedFileProblemID, []packagefs.IndexedFile{indexedArtifact("tests/1.in", deletedFile)}); err != nil {
		t.Fatal(err)
	}
	if err := repo.DeleteProblemFiles(ctx, deletedFileProblemID, []string{"tests/1.in"}); err != nil {
		t.Fatal(err)
	}
	assertCleanupCandidate(t, ctx, pool, deletedFile, true)
	assertProblemFileIdentity(t, ctx, pool, deletedFileProblemID, "tests/1.in", problemv1.ArtifactRef{}, false)

	deletedProblemID := int64(103)
	insertProblemLifecycleRow(t, ctx, pool, deletedProblemID)
	deletedWithProblem := problemContentRef(deletedProblemID, "4", 14)
	retainedPackage := packageArtifactRef("5", 105)
	if _, err := pool.Exec(ctx, `
UPDATE problems
SET aggregate_version=1, package_revision=1,
    package_artifact_uri=$2, package_artifact_sha256=$3,
    package_artifact_size_bytes=$4
WHERE id=$1
`, deletedProblemID, retainedPackage.URI, retainedPackage.SHA256, retainedPackage.SizeBytes); err != nil {
		t.Fatal(err)
	}
	if _, err := pool.Exec(ctx, `
INSERT INTO problem_package_revisions(
    problem_id, package_revision, aggregate_version, artifact_uri,
    artifact_sha256, artifact_size_bytes, manifest_sha256
) VALUES($1,1,1,$2,$3,$4,$5)
`, deletedProblemID, retainedPackage.URI, retainedPackage.SHA256, retainedPackage.SizeBytes, strings.Repeat("6", 64)); err != nil {
		t.Fatal(err)
	}
	if err := repo.UpsertProblemFiles(ctx, deletedProblemID, []packagefs.IndexedFile{indexedArtifact("problem.yaml", deletedWithProblem)}); err != nil {
		t.Fatal(err)
	}
	if err := repo.DeleteProblem(ctx, deletedProblemID); err != nil {
		t.Fatal(err)
	}
	assertCleanupCandidate(t, ctx, pool, deletedWithProblem, true)
	assertCleanupCandidate(t, ctx, pool, retainedPackage, false)
	var remainingProblem int
	if err := pool.QueryRow(ctx, `SELECT COUNT(*) FROM problems WHERE id=$1`, deletedProblemID).Scan(&remainingProblem); err != nil {
		t.Fatal(err)
	}
	if remainingProblem != 0 {
		t.Fatal("deleted Problem row survived")
	}

	rollbackProblemID := int64(104)
	insertProblemLifecycleRow(t, ctx, pool, rollbackProblemID)
	rollbackOld := problemContentRef(rollbackProblemID, "7", 17)
	rollbackNew := problemContentRef(rollbackProblemID, "8", 18)
	if err := repo.UpsertProblemFiles(ctx, rollbackProblemID, []packagefs.IndexedFile{indexedArtifact("problem.yaml", rollbackOld)}); err != nil {
		t.Fatal(err)
	}
	injected := errors.New("rollback file replacement and cleanup candidate")
	err := repo.InTransaction(ctx, func(txRepo *Repository) error {
		if err := txRepo.UpsertProblemFiles(ctx, rollbackProblemID, []packagefs.IndexedFile{indexedArtifact("problem.yaml", rollbackNew)}); err != nil {
			return err
		}
		return injected
	})
	if !errors.Is(err, injected) {
		t.Fatalf("expected rollback failpoint, got %v", err)
	}
	assertCleanupCandidate(t, ctx, pool, rollbackOld, false)
	assertProblemFileIdentity(t, ctx, pool, rollbackProblemID, "problem.yaml", rollbackOld, true)
}

func TestProblemFileCleanupCandidateStillHonorsFinalReferences(t *testing.T) {
	ctx, repo, pool := setupProblemFileCleanupPostgres(t)

	ownerID := int64(201)
	consumerID := int64(202)
	insertProblemLifecycleRow(t, ctx, pool, ownerID)
	insertProblemLifecycleRow(t, ctx, pool, consumerID)
	shared := problemContentRef(ownerID, "a", 21)
	replacement := problemContentRef(ownerID, "b", 22)
	if err := repo.UpsertProblemFiles(ctx, ownerID, []packagefs.IndexedFile{indexedArtifact("problem.yaml", shared)}); err != nil {
		t.Fatal(err)
	}
	if err := repo.UpsertProblemFiles(ctx, consumerID, []packagefs.IndexedFile{indexedArtifact("shared.yaml", shared)}); err != nil {
		t.Fatal(err)
	}
	if err := repo.UpsertProblemFiles(ctx, ownerID, []packagefs.IndexedFile{indexedArtifact("problem.yaml", replacement)}); err != nil {
		t.Fatal(err)
	}
	old := time.Now().Add(-48 * time.Hour)
	if _, err := pool.Exec(ctx, `
UPDATE problem_artifact_upload_intents
SET updated_at=$2, retry_after=$2
WHERE artifact_uri=$1
`, shared.URI, old); err != nil {
		t.Fatal(err)
	}

	store := &recordingCleanupObjectStore{}
	report, err := (artifactgc.Collector{
		Ledger:     artifactgc.PostgresLedger{DB: pool},
		Store:      store,
		Retention:  24 * time.Hour,
		ClaimLease: 3 * time.Minute,
		Delete:     true,
		BatchSize:  10,
	}).Run(ctx)
	if err != nil {
		t.Fatal(err)
	}
	if report.Referenced != 1 || len(report.Deleted) != 0 || store.deleteCalls != 0 {
		t.Fatalf("referenced cleanup candidate reached object deletion: report=%#v delete_calls=%d", report, store.deleteCalls)
	}
	assertProblemFileIdentity(t, ctx, pool, consumerID, "shared.yaml", shared, true)
	assertCleanupCandidate(t, ctx, pool, shared, false)
}

func TestProductionResolveRequiresExactPendingIntent(t *testing.T) {
	ctx, repo, pool := setupProblemFileCleanupPostgres(t)
	problemID := int64(301)
	insertProblemLifecycleRow(t, ctx, pool, problemID)
	missing := problemContentRef(problemID, "c", 31)
	if err := repo.UpsertProblemFiles(ctx, problemID, []packagefs.IndexedFile{indexedArtifact("problem.yaml", missing)}); err != nil {
		t.Fatal(err)
	}
	if err := repo.InTransaction(ctx, func(txRepo *Repository) error {
		return txRepo.ResolveArtifactUploadIntent(ctx, missing)
	}); !errors.Is(err, ErrArtifactIntentMissing) {
		t.Fatalf("production resolver accepted a missing PENDING intent: %v", err)
	}
	if err := repo.ResolveLegacyArtifactUploadIntent(ctx, missing); err != nil {
		t.Fatalf("explicit legacy/backfill resolver rejected a pre-ledger reference: %v", err)
	}
	assertProblemFileIdentity(t, ctx, pool, problemID, "problem.yaml", missing, true)

	// A single content-addressed object may back several logical files. The
	// production path still requires one exact PENDING intent for that object,
	// then deterministically deduplicates its in-transaction resolution.
	sharedProblemID := int64(302)
	insertProblemLifecycleRow(t, ctx, pool, sharedProblemID)
	shared := problemContentRef(sharedProblemID, "d", 32)
	if err := repo.RegisterArtifactUploadIntent(ctx, shared); err != nil {
		t.Fatal(err)
	}
	if err := repo.ResolveArtifactUploadIntent(ctx, shared); !errors.Is(err, ErrArtifactUploadIncomplete) {
		t.Fatalf("production resolver accepted an upload before identity verification: %v", err)
	}
	if err := repo.MarkArtifactUploadCompleted(ctx, shared); err != nil {
		t.Fatal(err)
	}
	sharedFiles := []packagefs.IndexedFile{
		indexedArtifact("tests/1.in", shared),
		indexedArtifact("tests/2.in", shared),
	}
	if err := repo.InTransaction(ctx, func(txRepo *Repository) error {
		if err := txRepo.UpsertProblemFiles(ctx, sharedProblemID, sharedFiles); err != nil {
			return err
		}
		return txRepo.ResolveProblemFileUploadIntents(ctx, sharedFiles)
	}); err != nil {
		t.Fatalf("resolve shared immutable object: %v", err)
	}
	assertCleanupCandidate(t, ctx, pool, shared, false)
	assertProblemFileIdentity(t, ctx, pool, sharedProblemID, "tests/1.in", shared, true)
	assertProblemFileIdentity(t, ctx, pool, sharedProblemID, "tests/2.in", shared, true)
}

func TestOperatorReconcileAndPublisherRegistrationAreMutuallyExclusive(t *testing.T) {
	ctx, repo, pool := setupProblemFileCleanupPostgres(t)
	artifact := problemContentRef(401, "e", 41)
	ledger := artifactgc.PostgresLedger{DB: pool}

	if err := repo.RegisterArtifactUploadIntent(ctx, artifact); err != nil {
		t.Fatal(err)
	}
	if _, err := ledger.RequestReconcile(ctx, artifact.URI, artifact.SHA256, artifact.SizeBytes, "user:41", "before upload", "before-upload"); !errors.Is(err, artifactgc.ErrOperatorStateConflict) {
		t.Fatalf("operator claimed an upload before exact verification: %v", err)
	}
	if err := repo.MarkArtifactUploadCompleted(ctx, artifact); err != nil {
		t.Fatal(err)
	}
	if _, err := ledger.RequestReconcile(ctx, artifact.URI, artifact.SHA256, artifact.SizeBytes, "user:41", "first reconcile", "first-reconcile"); err != nil {
		t.Fatal(err)
	}

	// A publisher replay atomically withdraws the operator marker and verified
	// bit before issuing a new PUT. Collector Claim therefore cannot delete the
	// URI in the Register -> PUT window.
	if err := repo.RegisterArtifactUploadIntent(ctx, artifact); err != nil {
		t.Fatal(err)
	}
	var completedAt, manualAt *time.Time
	if err := pool.QueryRow(ctx, `
SELECT upload_completed_at, manual_reconcile_requested_at
FROM problem_artifact_upload_intents WHERE artifact_uri=$1
`, artifact.URI).Scan(&completedAt, &manualAt); err != nil {
		t.Fatal(err)
	}
	if completedAt != nil || manualAt != nil {
		t.Fatalf("publisher replay retained operator/verification marker: completed=%v manual=%v", completedAt, manualAt)
	}
	if _, err := ledger.RequestReconcile(ctx, artifact.URI, artifact.SHA256, artifact.SizeBytes, "user:41", "raced", "raced-reconcile"); !errors.Is(err, artifactgc.ErrOperatorStateConflict) {
		t.Fatalf("operator reconciled during Register -> PUT: %v", err)
	}

	if err := repo.MarkArtifactUploadCompleted(ctx, artifact); err != nil {
		t.Fatal(err)
	}
	if _, err := ledger.RequestReconcile(ctx, artifact.URI, artifact.SHA256, artifact.SizeBytes, "user:41", "verified", "verified-reconcile"); err != nil {
		t.Fatal(err)
	}
	claim, err := ledger.Claim(ctx, time.Now().Add(-7*24*time.Hour), time.Minute)
	if err != nil || claim == nil || claim.URI != artifact.URI {
		t.Fatalf("targeted reconcile did not bypass retention: claim=%#v err=%v", claim, err)
	}
	if err := repo.RegisterArtifactUploadIntent(ctx, artifact); !errors.Is(err, ErrArtifactGCInProgress) {
		t.Fatalf("publisher stole an operator-owned claim: %v", err)
	}
}

type recordingCleanupObjectStore struct {
	deleteCalls int
}

func (s *recordingCleanupObjectStore) Inspect(_ context.Context, intent artifactgc.Intent) (artifactgc.Object, bool, error) {
	return artifactgc.Object{Key: intent.Key, SHA256: intent.SHA256, SizeBytes: intent.SizeBytes}, true, nil
}

func (s *recordingCleanupObjectStore) DeleteIfMatches(_ context.Context, _ artifactgc.Intent) error {
	s.deleteCalls++
	return nil
}

func setupProblemFileCleanupPostgres(t *testing.T) (context.Context, *Repository, *pgxpool.Pool) {
	t.Helper()
	databaseURL := strings.TrimSpace(os.Getenv("OJOS_EVENTING_TEST_POSTGRES_URL"))
	if databaseURL == "" {
		t.Skip("set OJOS_EVENTING_TEST_POSTGRES_URL")
	}
	ctx, cancel := context.WithTimeout(context.Background(), 60*time.Second)
	t.Cleanup(cancel)
	admin, err := pgxpool.New(ctx, databaseURL)
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(admin.Close)
	schema := fmt.Sprintf("ojos_problem_file_cleanup_%d", time.Now().UTC().UnixNano())
	identifier := pgx.Identifier{schema}.Sanitize()
	if _, err := admin.Exec(ctx, "CREATE SCHEMA "+identifier); err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { _, _ = admin.Exec(context.Background(), "DROP SCHEMA IF EXISTS "+identifier+" CASCADE") })

	cfg, err := pgxpool.ParseConfig(databaseURL)
	if err != nil {
		t.Fatal(err)
	}
	if cfg.ConnConfig.RuntimeParams == nil {
		cfg.ConnConfig.RuntimeParams = map[string]string{}
	}
	cfg.ConnConfig.RuntimeParams["search_path"] = schema
	pool, err := pgxpool.NewWithConfig(ctx, cfg)
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(pool.Close)
	applyProblemMigrations(t, ctx, pool)
	return ctx, New(pool), pool
}

func insertProblemLifecycleRow(t *testing.T, ctx context.Context, pool *pgxpool.Pool, problemID int64) {
	t.Helper()
	if _, err := pool.Exec(ctx, `INSERT INTO problems(id,title) VALUES($1,$2)`, problemID, fmt.Sprintf("lifecycle-%d", problemID)); err != nil {
		t.Fatal(err)
	}
}

func problemContentRef(problemID int64, digestCharacter string, size int64) problemv1.ArtifactRef {
	digest := strings.Repeat(digestCharacter, 64)
	return problemv1.ArtifactRef{
		URI:         fmt.Sprintf("storage://problems/problem-%d-objects-sha256-%s", problemID, digest),
		SHA256:      digest,
		SizeBytes:   size,
		ContentType: "application/octet-stream",
	}
}

func packageArtifactRef(digestCharacter string, size int64) problemv1.ArtifactRef {
	digest := strings.Repeat(digestCharacter, 64)
	return problemv1.ArtifactRef{
		URI:         "storage://problems/package-sha256-" + digest + ".zip",
		SHA256:      digest,
		SizeBytes:   size,
		ContentType: "application/zip",
	}
}

func indexedArtifact(logicalPath string, artifact problemv1.ArtifactRef) packagefs.IndexedFile {
	return packagefs.IndexedFile{
		LogicalPath: logicalPath,
		FileKind:    "authoring",
		StoragePath: artifact.URI,
		Sha256:      artifact.SHA256,
		SizeBytes:   artifact.SizeBytes,
		MimeType:    artifact.ContentType,
	}
}

func assertCleanupCandidate(t *testing.T, ctx context.Context, pool *pgxpool.Pool, artifact problemv1.ArtifactRef, want bool) {
	t.Helper()
	var digest string
	var size int64
	err := pool.QueryRow(ctx, `
SELECT artifact_sha256, artifact_size_bytes
FROM problem_artifact_upload_intents
WHERE artifact_uri=$1 AND status='PENDING'
`, artifact.URI).Scan(&digest, &size)
	if !want {
		if !errors.Is(err, pgx.ErrNoRows) {
			t.Fatalf("unexpected cleanup candidate %s: digest=%s size=%d err=%v", artifact.URI, digest, size, err)
		}
		return
	}
	if err != nil {
		t.Fatalf("missing cleanup candidate %s: %v", artifact.URI, err)
	}
	if digest != strings.ToLower(artifact.SHA256) || size != artifact.SizeBytes {
		t.Fatalf("cleanup candidate identity mismatch for %s: %s/%d", artifact.URI, digest, size)
	}
}

func assertProblemFileIdentity(t *testing.T, ctx context.Context, pool *pgxpool.Pool, problemID int64, logicalPath string, artifact problemv1.ArtifactRef, want bool) {
	t.Helper()
	var uri, digest string
	var size int64
	err := pool.QueryRow(ctx, `
SELECT storage_path, sha256, size_bytes
FROM problem_files
WHERE problem_id=$1 AND logical_path=$2
`, problemID, logicalPath).Scan(&uri, &digest, &size)
	if !want {
		if !errors.Is(err, pgx.ErrNoRows) {
			t.Fatalf("unexpected problem_files row %d/%s: %s/%s/%d err=%v", problemID, logicalPath, uri, digest, size, err)
		}
		return
	}
	if err != nil {
		t.Fatalf("missing problem_files row %d/%s: %v", problemID, logicalPath, err)
	}
	if uri != artifact.URI || !strings.EqualFold(digest, artifact.SHA256) || size != artifact.SizeBytes {
		t.Fatalf("problem_files identity mismatch %d/%s: %s/%s/%d", problemID, logicalPath, uri, digest, size)
	}
}
