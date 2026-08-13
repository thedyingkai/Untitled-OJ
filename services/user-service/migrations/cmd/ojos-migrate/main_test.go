package main

import (
	"os"
	"path/filepath"
	"reflect"
	"testing"
)

func TestDatabaseDSNReadsAgentResourceOutput(t *testing.T) {
	path := filepath.Join(t.TempDir(), "dsn")
	if err := os.WriteFile(path, []byte(`{"dsn":"postgres://user:secret@db:5432/users?sslmode=require"}`), 0o600); err != nil {
		t.Fatal(err)
	}
	t.Setenv("OJOS_RESOURCE_OUTPUT_FILE", path)
	dsn, err := databaseDSN()
	if err != nil {
		t.Fatal(err)
	}
	if dsn != "postgres://user:secret@db:5432/users?sslmode=require" {
		t.Fatalf("unexpected DSN: %q", dsn)
	}
}

func TestDatabaseDSNPrefersNamedClaimOverGenericAlias(t *testing.T) {
	directory := t.TempDir()
	named := filepath.Join(directory, "named")
	generic := filepath.Join(directory, "generic")
	if err := os.WriteFile(named, []byte(`{"dsn":"postgres://user:named@db:5432/users"}`), 0o600); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(generic, []byte(`{"dsn":"postgres://user:generic@db:5432/wrong"}`), 0o600); err != nil {
		t.Fatal(err)
	}
	t.Setenv("OJOS_RESOURCE_PROFILES_OUTPUT_FILE", named)
	t.Setenv("OJOS_RESOURCE_OUTPUT_FILE", generic)
	dsn, err := databaseDSN()
	if err != nil || dsn != "postgres://user:named@db:5432/users" {
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
	want := []string{
		filepath.Join(directory, "000001_a.up.sql"),
		filepath.Join(directory, "000002_b.up.sql"),
	}
	if !reflect.DeepEqual(files, want) {
		t.Fatalf("migration files = %v, want %v", files, want)
	}
}

func TestMigrationFilesRejectsEmptyDirectory(t *testing.T) {
	if _, err := migrationFiles(t.TempDir()); err == nil {
		t.Fatal("expected empty migration directory to fail closed")
	}
}
