package main

import (
	"os"
	"path/filepath"
	"reflect"
	"testing"
)

func TestDatabaseDSNReadsAgentResourceOutput(t *testing.T) {
	path := filepath.Join(t.TempDir(), "dsn")
	if err := os.WriteFile(path, []byte(`{"dsn":"postgres://problem:secret@db:5432/problems?sslmode=require"}`), 0o600); err != nil {
		t.Fatal(err)
	}
	t.Setenv("OJOS_RESOURCE_OUTPUT_FILE", path)
	dsn, err := databaseDSN()
	if err != nil {
		t.Fatal(err)
	}
	if dsn != "postgres://problem:secret@db:5432/problems?sslmode=require" {
		t.Fatalf("unexpected DSN: %q", dsn)
	}
}

func TestDatabaseDSNPrefersProblemsClaimOutput(t *testing.T) {
	directory := t.TempDir()
	claimPath := filepath.Join(directory, "problems-dsn")
	genericPath := filepath.Join(directory, "generic-dsn")
	claimDSN := "postgres://problem:claim@db:5432/problems?sslmode=require"
	if err := os.WriteFile(claimPath, []byte(`{"dsn":"`+claimDSN+`"}`), 0o600); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(genericPath, []byte(`{"dsn":"postgres://wrong:generic@db:5432/other"}`), 0o600); err != nil {
		t.Fatal(err)
	}
	t.Setenv("OJOS_RESOURCE_PROBLEMS_OUTPUT_FILE", claimPath)
	t.Setenv("OJOS_RESOURCE_OUTPUT_FILE", genericPath)
	dsn, err := databaseDSN()
	if err != nil {
		t.Fatal(err)
	}
	if dsn != claimDSN {
		t.Fatalf("migration runner selected %q instead of the problems claim DSN", dsn)
	}
}

func TestMigrationFilesAreDeterministicAndUpOnly(t *testing.T) {
	directory := t.TempDir()
	for _, name := range []string{"000002_b.up.sql", "000001_a.up.sql", "000001_a.down.sql", "README.md"} {
		if err := os.WriteFile(filepath.Join(directory, name), []byte("SELECT 1;"), 0o600); err != nil {
			t.Fatal(err)
		}
	}
	files, err := migrationFiles(directory)
	if err != nil {
		t.Fatal(err)
	}
	want := []string{filepath.Join(directory, "000001_a.up.sql"), filepath.Join(directory, "000002_b.up.sql")}
	if !reflect.DeepEqual(files, want) {
		t.Fatalf("migration files = %v, want %v", files, want)
	}
}

func TestMigrationFilesRejectsEmptyDirectory(t *testing.T) {
	if _, err := migrationFiles(t.TempDir()); err == nil {
		t.Fatal("expected empty migration directory to fail closed")
	}
}

func TestOnlyPinnedConcurrentIndexMigrationRunsOutsideTransaction(t *testing.T) {
	path := filepath.Join("..", "..", "000006_problem_files_storage_path_index.up.sql")
	sql, err := os.ReadFile(path)
	if err != nil {
		t.Fatal(err)
	}
	const id = "000006_problem_files_storage_path_index.up.sql"
	if !migrationRunsOutsideTransaction(id, sql) {
		t.Fatal("pinned concurrent-index migration must run outside a transaction")
	}
	if migrationRunsOutsideTransaction("000009_untrusted.up.sql", sql) {
		t.Fatal("an unlisted migration must not escape the transaction boundary")
	}
	mutated := append([]byte(nil), sql...)
	mutated = append(mutated, '\n')
	if migrationRunsOutsideTransaction(id, mutated) {
		t.Fatal("a modified pinned migration must not escape the transaction boundary")
	}
}
