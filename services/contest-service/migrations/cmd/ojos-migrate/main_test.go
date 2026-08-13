package main

import (
	"os"
	"path/filepath"
	"reflect"
	"testing"
)

func TestMigrationFilesAreStableAndOnlyUp(t *testing.T) {
	directory := t.TempDir()
	for name := range map[string]bool{"000002_b.up.sql": true, "000001_a.up.sql": true, "000001_a.down.sql": true} {
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
		t.Fatalf("files=%v want=%v", files, want)
	}
}

func TestDatabaseDSNReadsOnlyExplicitSecretFile(t *testing.T) {
	path := filepath.Join(t.TempDir(), "dsn")
	if err := os.WriteFile(path, []byte("postgresql://contest:secret@postgres/contest"), 0o600); err != nil {
		t.Fatal(err)
	}
	t.Setenv("OJOS_RESOURCE_OUTPUT_FILE", path)
	dsn, err := databaseDSN()
	if err != nil || dsn != "postgresql://contest:secret@postgres/contest" {
		t.Fatalf("dsn=%q err=%v", dsn, err)
	}
}

func TestDatabaseDSNSupportsAgentResourceOutputJSON(t *testing.T) {
	path := filepath.Join(t.TempDir(), "dsn")
	if err := os.WriteFile(path, []byte(`{"dsn":"postgresql://contest:secret@postgres/contest?sslmode=require"}`), 0o600); err != nil {
		t.Fatal(err)
	}
	t.Setenv("OJOS_RESOURCE_OUTPUT_FILE", path)
	dsn, err := databaseDSN()
	if err != nil || dsn != "postgresql://contest:secret@postgres/contest?sslmode=require" {
		t.Fatalf("dsn=%q err=%v", dsn, err)
	}
}

func TestDatabaseDSNPrefersNamedClaimOverGenericAlias(t *testing.T) {
	directory := t.TempDir()
	named := filepath.Join(directory, "named")
	generic := filepath.Join(directory, "generic")
	if err := os.WriteFile(named, []byte("postgresql://contest:named@postgres/contest"), 0o600); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(generic, []byte("postgresql://contest:generic@postgres/wrong"), 0o600); err != nil {
		t.Fatal(err)
	}
	t.Setenv("OJOS_RESOURCE_CONTESTS_OUTPUT_FILE", named)
	t.Setenv("OJOS_RESOURCE_OUTPUT_FILE", generic)
	dsn, err := databaseDSN()
	if err != nil || dsn != "postgresql://contest:named@postgres/contest" {
		t.Fatalf("named claim was not authoritative: dsn=%q err=%v", dsn, err)
	}
}
