package packagemutation

import (
	"context"
	"fmt"
	"os"
	"path/filepath"
	"sort"
	"strings"
	"testing"
	"time"

	"ojos-problem-service/internal/config"
	"ojos-problem-service/internal/packagefs"
	"ojos-problem-service/internal/repository"

	"github.com/jackc/pgx/v5"
	"github.com/jackc/pgx/v5/pgxpool"
)

func TestPostgresMutationLockSerializesConcurrentWriters(t *testing.T) {
	ctx, repo, pool, storage := setupMutationPostgres(t)
	problemID, liveDir := createIntegrationProblem(t, ctx, repo, storage, "concurrent")

	firstEntered := make(chan struct{})
	releaseFirst := make(chan struct{})
	secondStarted := make(chan struct{})
	secondEntered := make(chan struct{})
	errorsCh := make(chan error, 2)

	run := func(first bool) {
		if !first {
			close(secondStarted)
		}
		_, err := RunExisting(
			ctx,
			repo,
			storage,
			problemID,
			func(_ *repository.Problem, stagingDir string) (Change, error) {
				if first {
					close(firstEntered)
					select {
					case <-releaseFirst:
					case <-ctx.Done():
						return Change{}, ctx.Err()
					}
				} else {
					close(secondEntered)
				}
				result, err := packagefs.AddCase(packagefs.AddCaseArgs{
					PackageDir: stagingDir,
					Input:      fmt.Sprintf("%t input\n", first),
					Answer:     fmt.Sprintf("%t answer\n", first),
					Score:      50,
				})
				if err != nil {
					return Change{}, err
				}
				return Change{Files: result.Files}, nil
			},
			func(txRepo *repository.Repository, _ *repository.Problem, change Change) error {
				return txRepo.UpsertProblemFiles(ctx, problemID, change.Files)
			},
		)
		errorsCh <- err
	}

	go run(true)
	select {
	case <-firstEntered:
	case <-ctx.Done():
		t.Fatal(ctx.Err())
	}
	go run(false)
	select {
	case <-secondStarted:
	case <-ctx.Done():
		t.Fatal(ctx.Err())
	}
	select {
	case <-secondEntered:
		t.Fatal("second writer entered staging while first held the PostgreSQL advisory lock")
	case <-time.After(250 * time.Millisecond):
	}
	close(releaseFirst)
	for range 2 {
		if err := <-errorsCh; err != nil {
			t.Fatal(err)
		}
	}

	cases, err := packagefs.ListCases(liveDir)
	if err != nil {
		t.Fatal(err)
	}
	if len(cases) != 2 || cases[0].No != 1 || cases[1].No != 2 {
		t.Fatalf("serialized writers lost a testcase: %#v", cases)
	}
	state, err := repo.ProblemProjectionState(ctx, problemID)
	if err != nil {
		t.Fatal(err)
	}
	if state.AggregateVersion != 3 {
		t.Fatalf("aggregate version is %d, want initial+2 mutations = 3", state.AggregateVersion)
	}
	var outboxCount int
	if err := pool.QueryRow(ctx, `SELECT COUNT(*) FROM integration_outbox WHERE aggregate_id = $1`, fmt.Sprintf("problem/%d", problemID)).Scan(&outboxCount); err != nil {
		t.Fatal(err)
	}
	if outboxCount != 3 {
		t.Fatalf("outbox count is %d, want 3", outboxCount)
	}
}

func TestPostgresDeferredCommitFailureRestoresLiveTree(t *testing.T) {
	ctx, repo, pool, storage := setupMutationPostgres(t)
	problemID, liveDir := createIntegrationProblem(t, ctx, repo, storage, "commit-failure")
	before := snapshotTestTree(t, liveDir)
	stateBefore, err := repo.ProblemProjectionState(ctx, problemID)
	if err != nil {
		t.Fatal(err)
	}
	if _, err := pool.Exec(ctx, `ALTER TABLE problems ADD CONSTRAINT mutation_title_unique UNIQUE(title) DEFERRABLE INITIALLY DEFERRED`); err != nil {
		t.Fatal(err)
	}
	conflictingID, err := repo.ReserveProblemID(ctx)
	if err != nil {
		t.Fatal(err)
	}

	_, err = RunExisting(
		ctx,
		repo,
		storage,
		problemID,
		func(_ *repository.Problem, stagingDir string) (Change, error) {
			result, err := packagefs.AddCase(packagefs.AddCaseArgs{
				PackageDir: stagingDir,
				Input:      "must rollback\n",
				Answer:     "must rollback\n",
				Score:      100,
			})
			if err != nil {
				return Change{}, err
			}
			return Change{Files: result.Files}, nil
		},
		func(txRepo *repository.Repository, problem *repository.Problem, change Change) error {
			if err := txRepo.UpsertProblemFiles(ctx, problemID, change.Files); err != nil {
				return err
			}
			// The duplicate title is accepted until outer COMMIT because the
			// constraint is deferred. This injects a failure after journal+swap.
			return txRepo.InsertProblemWithID(ctx, conflictingID, repository.CreateProblemArg{
				ProblemNo:     fmt.Sprintf("P%d", conflictingID),
				Title:         problem.Title,
				ProblemType:   "traditional",
				Visibility:    "private",
				Difficulty:    "medium",
				TimeLimitMs:   1000,
				MemoryLimitMb: 256,
				CreatedBy:     1,
			})
		},
	)
	if err == nil {
		t.Fatal("deferred commit failure was not surfaced")
	}
	after := snapshotTestTree(t, liveDir)
	if !reflectStringMapEqual(before, after) {
		t.Fatalf("commit failure changed live bytes: before=%v after=%v", before, after)
	}
	stateAfter, stateErr := repo.ProblemProjectionState(ctx, problemID)
	if stateErr != nil {
		t.Fatal(stateErr)
	}
	if stateAfter.AggregateVersion != stateBefore.AggregateVersion || stateAfter.PackageArtifactSHA256 != stateBefore.PackageArtifactSHA256 {
		t.Fatalf("failed commit advanced projection: before=%+v after=%+v", stateBefore, stateAfter)
	}
	var conflictingRows int
	if err := pool.QueryRow(ctx, `SELECT COUNT(*) FROM problems WHERE id = $1`, conflictingID).Scan(&conflictingRows); err != nil {
		t.Fatal(err)
	}
	if conflictingRows != 0 {
		t.Fatal("deferred-constraint failpoint row survived rollback")
	}
	if _, err := os.Stat(journalPath(storage.ProblemsRoot, problemID)); !os.IsNotExist(err) {
		t.Fatalf("recovered transaction left a mutation journal: %v", err)
	}
}

func setupMutationPostgres(t *testing.T) (context.Context, *repository.Repository, *pgxpool.Pool, config.StorageConfig) {
	t.Helper()
	databaseURL := strings.TrimSpace(os.Getenv("OJOS_EVENTING_TEST_POSTGRES_URL"))
	if databaseURL == "" {
		t.Skip("set OJOS_EVENTING_TEST_POSTGRES_URL")
	}
	ctx, cancel := context.WithTimeout(context.Background(), 45*time.Second)
	t.Cleanup(cancel)
	admin, err := pgxpool.New(ctx, databaseURL)
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(admin.Close)
	schema := fmt.Sprintf("ojos_package_mutation_%d", time.Now().UTC().UnixNano())
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
	// One connection makes the test prove that the advisory lock and final
	// transaction use the same physical session. An implementation that tried
	// to begin the transaction through the pool would deadlock here.
	cfg.MaxConns = 1
	pool, err := pgxpool.NewWithConfig(ctx, cfg)
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(pool.Close)
	applyMutationMigrations(t, ctx, pool)
	storage := config.StorageConfig{ProblemsRoot: t.TempDir()}
	return ctx, repository.New(pool), pool, storage
}

func createIntegrationProblem(t *testing.T, ctx context.Context, repo *repository.Repository, storage config.StorageConfig, suffix string) (int64, string) {
	t.Helper()
	id, err := repo.ReserveProblemID(ctx)
	if err != nil {
		t.Fatal(err)
	}
	title := fmt.Sprintf("mutation-%s-%d", suffix, id)
	result, err := RunCreate(
		ctx,
		repo,
		storage,
		packagefs.CreateProblemArgs{
			ID:            id,
			ProblemNo:     fmt.Sprintf("P%d", id),
			Slug:          suffix,
			Title:         title,
			ProblemType:   "traditional",
			Visibility:    "private",
			TimeLimitMs:   1000,
			MemoryLimitMb: 256,
		},
		func(txRepo *repository.Repository, result *packagefs.CreateProblemResult) error {
			if err := txRepo.InsertProblemWithID(ctx, id, repository.CreateProblemArg{
				ProblemNo:     fmt.Sprintf("P%d", id),
				Title:         title,
				ProblemType:   "traditional",
				Visibility:    "private",
				Difficulty:    "medium",
				TimeLimitMs:   1000,
				MemoryLimitMb: 256,
				CreatedBy:     1,
			}); err != nil {
				return err
			}
			if err := txRepo.UpdateProblemPackage(ctx, id, filepath.Base(result.PackageDir), result.PackageDir, result.ManifestPath, result.ManifestSha256); err != nil {
				return err
			}
			return txRepo.UpsertProblemFiles(ctx, id, result.Files)
		},
	)
	if err != nil {
		t.Fatal(err)
	}
	problem, err := repo.GetProblem(ctx, id)
	if err != nil {
		t.Fatal(err)
	}
	if problem.PackageDir != result.PackageDir || problem.ManifestPath != result.ManifestPath {
		t.Fatalf("create persisted stale staging paths: package=%q manifest=%q result=%q", problem.PackageDir, problem.ManifestPath, result.PackageDir)
	}
	if _, err := os.Stat(filepath.Join(problem.PackageDir, filepath.FromSlash(problem.ManifestPath))); err != nil {
		t.Fatalf("persisted manifest path is not live: %v", err)
	}
	return id, result.PackageDir
}

func applyMutationMigrations(t *testing.T, ctx context.Context, pool *pgxpool.Pool) {
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

func reflectStringMapEqual(left map[string]string, right map[string]string) bool {
	if len(left) != len(right) {
		return false
	}
	for key, value := range left {
		if right[key] != value {
			return false
		}
	}
	return true
}
