package main

import (
	"os"
	"path/filepath"
	"reflect"
	"testing"
)

func TestDatabaseDSNReadsAgentResourceOutput(t *testing.T) {
	path := filepath.Join(t.TempDir(), "dsn")
	if err := os.WriteFile(path, []byte(`{"dsn":"postgres://judge:secret@db:5432/submissions?sslmode=require"}`), 0o600); err != nil {
		t.Fatal(err)
	}
	t.Setenv("OJOS_RESOURCE_OUTPUT_FILE", path)
	dsn, err := databaseDSN()
	if err != nil {
		t.Fatal(err)
	}
	if dsn != "postgres://judge:secret@db:5432/submissions?sslmode=require" {
		t.Fatalf("unexpected DSN: %q", dsn)
	}
}

func TestDatabaseDSNPrefersNamedClaimOverGenericAlias(t *testing.T) {
	directory := t.TempDir()
	named := filepath.Join(directory, "named")
	generic := filepath.Join(directory, "generic")
	if err := os.WriteFile(named, []byte(`{"dsn":"postgres://judge:named@db:5432/submissions"}`), 0o600); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(generic, []byte(`{"dsn":"postgres://judge:generic@db:5432/wrong"}`), 0o600); err != nil {
		t.Fatal(err)
	}
	t.Setenv("OJOS_RESOURCE_SUBMISSIONS_OUTPUT_FILE", named)
	t.Setenv("OJOS_RESOURCE_OUTPUT_FILE", generic)
	dsn, err := databaseDSN()
	if err != nil || dsn != "postgres://judge:named@db:5432/submissions" {
		t.Fatalf("named claim was not authoritative: dsn=%q err=%v", dsn, err)
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

func TestCheckedInMigrationSetIsComplete(t *testing.T) {
	files, err := migrationFiles(filepath.Join("..", ".."))
	if err != nil {
		t.Fatal(err)
	}
	want := []string{
		"000001_submission_schema.up.sql",
		"000002_worker_link.up.sql",
		"000003_problem_meta_cache.up.sql",
		"000004_problem_projection_events.up.sql",
		"000005_judge_result_outbox.up.sql",
		"000006_task_retry_schedule.up.sql",
	}
	got := make([]string, len(files))
	for index := range files {
		got[index] = filepath.Base(files[index])
	}
	if !reflect.DeepEqual(got, want) {
		t.Fatalf("checked-in migrations = %v, want %v", got, want)
	}
}

func TestMigrationFilesRejectsEmptyDirectory(t *testing.T) {
	if _, err := migrationFiles(t.TempDir()); err == nil {
		t.Fatal("expected empty migration directory to fail closed")
	}
}
