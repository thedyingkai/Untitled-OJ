package repository

import (
	"context"
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"sort"
	"strings"
	"testing"
	"time"

	"ojos-problem-events/problemv1"

	"github.com/jackc/pgx/v5"
	"github.com/jackc/pgx/v5/pgxpool"
)

func TestRealProblemMutationAndOutboxShareTransaction(t *testing.T) {
	databaseURL := strings.TrimSpace(os.Getenv("OJOS_EVENTING_TEST_POSTGRES_URL"))
	if databaseURL == "" {
		t.Skip("set OJOS_EVENTING_TEST_POSTGRES_URL")
	}
	ctx, cancel := context.WithTimeout(context.Background(), 30*time.Second)
	defer cancel()
	admin, err := pgxpool.New(ctx, databaseURL)
	if err != nil {
		t.Fatal(err)
	}
	defer admin.Close()
	schema := fmt.Sprintf("ojos_problem_outbox_%d", time.Now().UTC().UnixNano())
	identifier := pgx.Identifier{schema}.Sanitize()
	if _, err := admin.Exec(ctx, "CREATE SCHEMA "+identifier); err != nil {
		t.Fatal(err)
	}
	defer admin.Exec(context.Background(), "DROP SCHEMA IF EXISTS "+identifier+" CASCADE")

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
	defer pool.Close()
	applyProblemMigrations(t, ctx, pool)

	repo := New(pool)
	emptyDigest := "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
	emptyURI := "storage://problems/problem-1-objects-sha256-" + emptyDigest
	if _, err := pool.Exec(ctx, `
INSERT INTO problem_artifact_upload_intents(
 artifact_uri, artifact_sha256, artifact_size_bytes, status
) VALUES($1,$2,0,'PENDING')
`, emptyURI, emptyDigest); err != nil {
		t.Fatalf("v2 migration rejected a zero-byte content intent: %v", err)
	}
	if _, err := pool.Exec(ctx, `DELETE FROM problem_artifact_upload_intents WHERE artifact_uri=$1`, emptyURI); err != nil {
		t.Fatal(err)
	}
	zeroPackageURI := "storage://problems/package-sha256-" + emptyDigest + ".zip"
	if _, err := pool.Exec(ctx, `
INSERT INTO problem_artifact_upload_intents(
 artifact_uri, artifact_sha256, artifact_size_bytes, status
) VALUES($1,$2,0,'PENDING')
`, zeroPackageURI, emptyDigest); err == nil {
		t.Fatal("v2 migration allowed a zero-byte package archive intent")
	}
	id, err := repo.ReserveProblemID(ctx)
	if err != nil {
		t.Fatal(err)
	}
	artifact := problemv1.ArtifactRef{
		SHA256:      strings.Repeat("b", 64),
		SizeBytes:   456,
		ContentType: "application/zip",
	}
	artifact.URI = "storage://problems/package-sha256-" + artifact.SHA256 + ".zip"
	if err := repo.RegisterArtifactUploadIntent(ctx, artifact); err != nil {
		t.Fatal(err)
	}
	if err := repo.MarkArtifactUploadCompleted(ctx, artifact); err != nil {
		t.Fatal(err)
	}
	err = repo.InTransaction(ctx, func(txRepo *Repository) error {
		if err := txRepo.InsertProblemWithID(ctx, id, CreateProblemArg{
			ProblemNo:      fmt.Sprintf("P%d", id),
			Title:          "outbox integration",
			ProblemType:    "traditional",
			Visibility:     "public",
			Difficulty:     "medium",
			TimeLimitMs:    1000,
			MemoryLimitMb:  256,
			CreatedBy:      9,
			LanguageLimits: []ProblemLanguageLimit{},
		}); err != nil {
			return err
		}
		if err := txRepo.UpdateProblemPackage(ctx, id, "outbox-integration", "/tmp/problem", "problem.yaml", strings.Repeat("c", 64)); err != nil {
			return err
		}
		_, err := txRepo.PublishProblemSnapshot(ctx, id, artifact)
		return err
	})
	if err != nil {
		t.Fatal(err)
	}
	var aggregateVersion, packageRevision, outboxCount int64
	if err := pool.QueryRow(ctx, `SELECT aggregate_version, package_revision FROM problems WHERE id = $1`, id).Scan(&aggregateVersion, &packageRevision); err != nil {
		t.Fatal(err)
	}
	if err := pool.QueryRow(ctx, `SELECT COUNT(*) FROM integration_outbox WHERE aggregate_id = $1`, fmt.Sprintf("problem/%d", id)).Scan(&outboxCount); err != nil {
		t.Fatal(err)
	}
	if aggregateVersion != 1 || packageRevision != 1 || outboxCount != 1 {
		t.Fatalf("mutation/outbox transaction is incomplete: aggregate=%d package=%d outbox=%d", aggregateVersion, packageRevision, outboxCount)
	}

	// Returning to an earlier digest still creates a new immutable revision;
	// digest addressing deduplicates bytes, not revision history.
	for _, digest := range []string{strings.Repeat("d", 64), artifact.SHA256} {
		next := artifact
		next.SHA256 = digest
		next.URI = "storage://problems/package-sha256-" + digest + ".zip"
		if err := repo.RegisterArtifactUploadIntent(ctx, next); err != nil {
			t.Fatal(err)
		}
		if err := repo.MarkArtifactUploadCompleted(ctx, next); err != nil {
			t.Fatal(err)
		}
		if err := repo.InTransaction(ctx, func(txRepo *Repository) error {
			_, err := txRepo.PublishProblemSnapshot(ctx, id, next)
			return err
		}); err != nil {
			t.Fatal(err)
		}
	}
	var revisionCount int64
	if err := pool.QueryRow(ctx, `SELECT package_revision FROM problems WHERE id = $1`, id).Scan(&packageRevision); err != nil {
		t.Fatal(err)
	}
	if err := pool.QueryRow(ctx, `SELECT COUNT(*) FROM problem_package_revisions WHERE problem_id = $1`, id).Scan(&revisionCount); err != nil {
		t.Fatal(err)
	}
	if packageRevision != 3 || revisionCount != 3 {
		t.Fatalf("digest reuse lost immutable history: current=%d rows=%d", packageRevision, revisionCount)
	}

	rolledBackID, err := repo.ReserveProblemID(ctx)
	if err != nil {
		t.Fatal(err)
	}
	expected := errors.New("failpoint after domain insert")
	err = repo.InTransaction(ctx, func(txRepo *Repository) error {
		if err := txRepo.InsertProblemWithID(ctx, rolledBackID, CreateProblemArg{
			ProblemNo:     fmt.Sprintf("P%d", rolledBackID),
			Title:         "must rollback",
			ProblemType:   "traditional",
			Visibility:    "private",
			Difficulty:    "medium",
			TimeLimitMs:   1000,
			MemoryLimitMb: 256,
			CreatedBy:     9,
		}); err != nil {
			return err
		}
		return expected
	})
	if !errors.Is(err, expected) {
		t.Fatalf("expected failpoint error, got %v", err)
	}
	var rolledBackCount int
	if err := pool.QueryRow(ctx, `SELECT COUNT(*) FROM problems WHERE id = $1`, rolledBackID).Scan(&rolledBackCount); err != nil {
		t.Fatal(err)
	}
	if rolledBackCount != 0 {
		t.Fatal("domain row survived rolled back outbox transaction")
	}
}

func applyProblemMigrations(t *testing.T, ctx context.Context, pool *pgxpool.Pool) {
	t.Helper()
	files, err := filepath.Glob(filepath.Join("..", "..", "migrations", "*.up.sql"))
	if err != nil {
		t.Fatal(err)
	}
	sort.Strings(files)
	if len(files) == 0 {
		t.Fatal("problem migrations were not found")
	}
	for _, file := range files {
		sql, err := os.ReadFile(file)
		if err != nil {
			t.Fatal(err)
		}
		if _, err := pool.Exec(ctx, string(sql)); err != nil {
			t.Fatalf("apply %s: %v", filepath.Base(file), err)
		}
	}
}
