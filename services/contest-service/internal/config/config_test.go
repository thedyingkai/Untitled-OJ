package config

import (
	"os"
	"path/filepath"
	"strings"
	"testing"
)

func TestReadDatabaseDSNAcceptsResourceOutputJSON(t *testing.T) {
	path := filepath.Join(t.TempDir(), "dsn")
	if err := os.WriteFile(path, []byte(`{"dsn":"postgresql://contest:secret@postgres/contest?sslmode=require"}`), 0o600); err != nil {
		t.Fatal(err)
	}
	dsn, err := ReadDatabaseDSN(path)
	if err != nil || dsn != "postgresql://contest:secret@postgres/contest?sslmode=require" {
		t.Fatalf("dsn=%q err=%v", dsn, err)
	}
}

func TestReadDatabaseDSNDoesNotLeakCredentialInErrors(t *testing.T) {
	path := filepath.Join(t.TempDir(), "dsn")
	secret := "do-not-log-this-password"
	if err := os.WriteFile(path, []byte(`{"dsn":"postgresql://contest:`+secret+`@"}`), 0o600); err != nil {
		t.Fatal(err)
	}
	_, err := ReadDatabaseDSN(path)
	if err == nil || strings.Contains(err.Error(), secret) {
		t.Fatalf("credential leaked in error: %v", err)
	}
}

func TestReadDatabaseDSNRejectsNonPostgres(t *testing.T) {
	path := filepath.Join(t.TempDir(), "dsn")
	if err := os.WriteFile(path, []byte("https://example.com"), 0o600); err != nil {
		t.Fatal(err)
	}
	if _, err := ReadDatabaseDSN(path); err == nil {
		t.Fatal("non-PostgreSQL DSN accepted")
	}
}

func TestReadDatabaseDSNRejectsUnknownResourceOutputFields(t *testing.T) {
	path := filepath.Join(t.TempDir(), "dsn")
	if err := os.WriteFile(path, []byte(`{"dsn":"postgresql://contest:secret@postgres/contest","password":"leak"}`), 0o600); err != nil {
		t.Fatal(err)
	}
	if _, err := ReadDatabaseDSN(path); err == nil {
		t.Fatal("resource output with unknown credential field accepted")
	}
}

func TestLoadValidatesAgentMaterializedConditionalSecret(t *testing.T) {
	t.Setenv("OJOS_CONFIG_REGISTRATION_MODE", "invite-only")
	t.Setenv("OJOS_SECRET_REGISTRATION_INVITESIGNINGKEY", "")
	if _, err := Load(); err == nil {
		t.Fatal("invite-only mode without its conditional secret accepted")
	}
	t.Setenv("OJOS_SECRET_REGISTRATION_INVITESIGNINGKEY", strings.Repeat("k", 32))
	loaded, err := Load()
	if err != nil || loaded.RegistrationMode != "invite-only" {
		t.Fatalf("config=%#v err=%v", loaded, err)
	}
}

func TestLoadRejectsConditionalSecretWhenBranchIsInactive(t *testing.T) {
	t.Setenv("OJOS_CONFIG_REGISTRATION_MODE", "open")
	t.Setenv("OJOS_SECRET_REGISTRATION_INVITESIGNINGKEY", strings.Repeat("k", 32))
	if _, err := Load(); err == nil {
		t.Fatal("inactive conditional secret accepted")
	}
}

func TestManagedLoadRejectsLegacyRuntimePathAndListenPoisoning(t *testing.T) {
	t.Setenv("OJOS_MANAGED_WORKLOAD", "true")
	t.Setenv("CONTEST_DATABASE_SECRET_FILE", "/legacy/database-secret")
	t.Setenv("CONTEST_LISTEN_ADDRESS", "127.0.0.1:9999")
	loaded, err := Load()
	if err != nil {
		t.Fatal(err)
	}
	if loaded.DatabaseSecretFile != defaultDatabaseSecret || loaded.ListenAddress != ":8080" {
		t.Fatalf("managed config retained legacy runtime input: %#v", loaded)
	}
}

func TestReadDatabaseDSNRejectsMissingCredentialAndTrailingDocument(t *testing.T) {
	for name, contents := range map[string]string{
		"no-password": "postgresql://contest@postgres/contest",
		"trailing":    `{"dsn":"postgresql://contest:secret@postgres/contest"}{}`,
	} {
		t.Run(name, func(t *testing.T) {
			path := filepath.Join(t.TempDir(), "dsn")
			if err := os.WriteFile(path, []byte(contents), 0o600); err != nil {
				t.Fatal(err)
			}
			if _, err := ReadDatabaseDSN(path); err == nil {
				t.Fatalf("invalid Agent resource output %q accepted", contents)
			}
		})
	}
}
