package resourceoutput

import (
	"os"
	"path/filepath"
	"strings"
	"testing"
)

func writeOutput(t *testing.T, contents string) string {
	t.Helper()
	path := filepath.Join(t.TempDir(), "output")
	if err := os.WriteFile(path, []byte(contents), 0o600); err != nil {
		t.Fatal(err)
	}
	return path
}

func TestReadPostgreSQLDSNAcceptsTypedAndLegacyOutputs(t *testing.T) {
	const dsn = "postgresql://service:secret@postgres/service?sslmode=require"
	for name, contents := range map[string]string{
		"typed":  `{"dsn":"` + dsn + `"}`,
		"legacy": dsn + "\n",
	} {
		t.Run(name, func(t *testing.T) {
			got, err := ReadPostgreSQLDSN(writeOutput(t, contents))
			if err != nil || got != dsn {
				t.Fatalf("dsn=%q err=%v", got, err)
			}
		})
	}
}

func TestReadPostgreSQLDSNRejectsUnknownFieldsAndTrailingJSON(t *testing.T) {
	for name, contents := range map[string]string{
		"unknown":  `{"dsn":"postgresql://service:secret@postgres/service","password":"secret"}`,
		"trailing": `{"dsn":"postgresql://service:secret@postgres/service"}{}`,
	} {
		t.Run(name, func(t *testing.T) {
			if _, err := ReadPostgreSQLDSN(writeOutput(t, contents)); err == nil {
				t.Fatal("invalid resource output accepted")
			}
		})
	}
}

func TestReadPostgreSQLDSNErrorsNeverContainCredential(t *testing.T) {
	secret := "do-not-log-this-password"
	_, err := ReadPostgreSQLDSN(writeOutput(t, `{"dsn":"postgresql://service:`+secret+`@"}`))
	if err == nil || strings.Contains(err.Error(), secret) {
		t.Fatalf("credential leaked through error: %v", err)
	}
}

func TestReadPostgreSQLDSNRequiresPostgresHostUserAndPassword(t *testing.T) {
	for name, contents := range map[string]string{
		"scheme":   "https://service:secret@example.test/database",
		"host":     "postgresql://service:secret@/database",
		"user":     "postgresql://postgres/database",
		"password": "postgresql://service@postgres/database",
	} {
		t.Run(name, func(t *testing.T) {
			if _, err := ReadPostgreSQLDSN(writeOutput(t, contents)); err == nil {
				t.Fatal("invalid PostgreSQL DSN accepted")
			}
		})
	}
}
