package repository

import (
	"context"
	"fmt"
	"os"
	"path/filepath"
	"sort"
	"strings"
	"testing"
	"time"

	"github.com/jackc/pgx/v5"
	"github.com/jackc/pgx/v5/pgxpool"
)

const problemFilesStoragePathIndex = "idx_problem_files_storage_path"

func TestProblemFilesStoragePathIndexMigrationsStayStandalone(t *testing.T) {
	t.Parallel()
	tests := []struct {
		file       string
		statement  string
		concurrent string
	}{
		{
			file:       "000006_problem_files_storage_path_index.up.sql",
			statement:  "CREATE INDEX CONCURRENTLY IF NOT EXISTS " + problemFilesStoragePathIndex,
			concurrent: "CREATE INDEX CONCURRENTLY",
		},
		{
			file:       "000006_problem_files_storage_path_index.down.sql",
			statement:  "DROP INDEX CONCURRENTLY IF EXISTS " + problemFilesStoragePathIndex,
			concurrent: "DROP INDEX CONCURRENTLY",
		},
	}
	for _, test := range tests {
		test := test
		t.Run(test.file, func(t *testing.T) {
			t.Parallel()
			contents, err := os.ReadFile(filepath.Join("..", "..", "migrations", test.file))
			if err != nil {
				t.Fatal(err)
			}
			sql := string(contents)
			if strings.Count(sql, ";") != 1 {
				t.Fatalf("concurrent migration must contain exactly one SQL statement: %q", sql)
			}
			upper := strings.ToUpper(sql)
			if strings.Contains(upper, "BEGIN;") || strings.Contains(upper, "COMMIT;") {
				t.Fatalf("concurrent migration must not open a transaction block: %q", sql)
			}
			if !strings.Contains(upper, strings.ToUpper(test.statement)) || !strings.Contains(upper, test.concurrent) {
				t.Fatalf("migration lost the required concurrent statement: %q", sql)
			}
		})
	}
}

func TestProblemFilesStoragePathConcurrentIndexMigrationPostgres(t *testing.T) {
	databaseURL := strings.TrimSpace(os.Getenv("OJOS_EVENTING_TEST_POSTGRES_URL"))
	if databaseURL == "" {
		t.Skip("set OJOS_EVENTING_TEST_POSTGRES_URL")
	}
	ctx, cancel := context.WithTimeout(context.Background(), 90*time.Second)
	t.Cleanup(cancel)

	admin, err := pgxpool.New(ctx, databaseURL)
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(admin.Close)
	schema := fmt.Sprintf("ojos_problem_online_index_%d", time.Now().UTC().UnixNano())
	identifier := pgx.Identifier{schema}.Sanitize()
	if _, err := admin.Exec(ctx, "CREATE SCHEMA "+identifier); err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() {
		cleanupCtx, cleanupCancel := context.WithTimeout(context.Background(), 15*time.Second)
		defer cleanupCancel()
		_, _ = admin.Exec(cleanupCtx, "DROP SCHEMA IF EXISTS "+identifier+" CASCADE")
	})

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

	files, err := filepath.Glob(filepath.Join("..", "..", "migrations", "*.up.sql"))
	if err != nil {
		t.Fatal(err)
	}
	sort.Strings(files)
	for _, file := range files {
		if filepath.Base(file) >= "000006_problem_files_storage_path_index.up.sql" {
			continue
		}
		sql, err := os.ReadFile(file)
		if err != nil {
			t.Fatal(err)
		}
		if _, err := pool.Exec(ctx, string(sql)); err != nil {
			t.Fatalf("apply prerequisite %s: %v", filepath.Base(file), err)
		}
	}

	if _, err := pool.Exec(ctx, `
INSERT INTO problems(id, title) VALUES (1, 'online index migration');
INSERT INTO problem_files(problem_id, logical_path, file_kind, storage_path)
VALUES
    (1, 'problem.yaml', 'authoring', 'storage://problems/problem-1-objects-sha256-' || repeat('a', 64)),
    (1, 'legacy.txt', 'authoring', '/legacy/problem/legacy.txt');
`); err != nil {
		t.Fatal(err)
	}

	up := readProblemMigration(t, "000006_problem_files_storage_path_index.up.sql")
	if _, err := pool.Exec(ctx, up); err != nil {
		t.Fatalf("apply concurrent index migration: %v", err)
	}
	// IF NOT EXISTS must make a replay harmless; migration runners can retry
	// after losing their own bookkeeping response.
	if _, err := pool.Exec(ctx, up); err != nil {
		t.Fatalf("replay concurrent index migration: %v", err)
	}

	var valid, ready bool
	var predicate, definition string
	if err := pool.QueryRow(ctx, `
SELECT i.indisvalid, i.indisready, pg_get_expr(i.indpred, i.indrelid), pg_get_indexdef(i.indexrelid)
FROM pg_index i
JOIN pg_class c ON c.oid = i.indexrelid
JOIN pg_namespace n ON n.oid = c.relnamespace
WHERE n.nspname = current_schema() AND c.relname = $1
`, problemFilesStoragePathIndex).Scan(&valid, &ready, &predicate, &definition); err != nil {
		t.Fatal(err)
	}
	if !valid || !ready {
		t.Fatalf("concurrent index was not promoted as valid and ready: valid=%t ready=%t", valid, ready)
	}
	if !strings.Contains(predicate, "storage://%") || !strings.Contains(definition, "(storage_path)") {
		t.Fatalf("unexpected partial index definition: predicate=%q definition=%q", predicate, definition)
	}

	down := readProblemMigration(t, "000006_problem_files_storage_path_index.down.sql")
	if _, err := pool.Exec(ctx, down); err != nil {
		t.Fatalf("drop concurrent index: %v", err)
	}
	if _, err := pool.Exec(ctx, down); err != nil {
		t.Fatalf("replay concurrent index rollback: %v", err)
	}
	var remaining int
	if err := pool.QueryRow(ctx, `
SELECT COUNT(*)
FROM pg_class c
JOIN pg_namespace n ON n.oid = c.relnamespace
WHERE n.nspname = current_schema() AND c.relname = $1
`, problemFilesStoragePathIndex).Scan(&remaining); err != nil {
		t.Fatal(err)
	}
	if remaining != 0 {
		t.Fatal("concurrent index survived its down migration")
	}
}

func readProblemMigration(t *testing.T, name string) string {
	t.Helper()
	contents, err := os.ReadFile(filepath.Join("..", "..", "migrations", name))
	if err != nil {
		t.Fatal(err)
	}
	return string(contents)
}
