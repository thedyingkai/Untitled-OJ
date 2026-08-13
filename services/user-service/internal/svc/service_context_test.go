package svc

import (
	"os"
	"path/filepath"
	"strings"
	"testing"

	"ojos-user-service/internal/config"
)

func TestManagedEnvironmentUsesAgentProfilesResourceOutput(t *testing.T) {
	path := filepath.Join(t.TempDir(), "dsn")
	if err := os.WriteFile(path, []byte(`{"dsn":"postgres://profile:secret@database:5432/profile?sslmode=require"}`), 0o600); err != nil {
		t.Fatal(err)
	}
	t.Setenv("OJOS_MANAGED_WORKLOAD", "true")
	t.Setenv("OJOS_RESOURCE_PROFILES_OUTPUT_FILE", path)
	t.Setenv("DATABASE_URL", "postgres://legacy:ignored@legacy:5432/legacy")
	var value config.Config
	if err := applyEnvOverrides(&value); err != nil {
		t.Fatal(err)
	}
	if value.Database.Url != "postgres://profile:secret@database:5432/profile?sslmode=require" {
		t.Fatalf("managed DSN did not come from resource output")
	}
}

func TestManagedEnvironmentRejectsMissingResourceOutputWithoutLeakingDSN(t *testing.T) {
	t.Setenv("OJOS_MANAGED_WORKLOAD", "1")
	t.Setenv("OJOS_RESOURCE_PROFILES_OUTPUT_FILE", filepath.Join(t.TempDir(), "missing"))
	var value config.Config
	err := applyEnvOverrides(&value)
	if err == nil {
		t.Fatal("expected missing resource output to fail closed")
	}
	if strings.Contains(err.Error(), "postgres://") {
		t.Fatalf("resource error leaked credential material: %v", err)
	}
}

func TestUnmanagedEnvironmentRetainsLegacyDevelopmentDatabaseAlias(t *testing.T) {
	t.Setenv("OJOS_MANAGED_WORKLOAD", "")
	t.Setenv("OJOS_ENVIRONMENT", "development")
	t.Setenv("DATABASE_URL", "postgres://local:secret@database:5432/local")
	var value config.Config
	if err := applyEnvOverrides(&value); err != nil {
		t.Fatal(err)
	}
	if value.Database.Url != "postgres://local:secret@database:5432/local" {
		t.Fatalf("unexpected development database URL")
	}
}

func TestPermissionBindingUsesContractRequirementID(t *testing.T) {
	if permissionBindingName != "auth.user.permission.check" {
		t.Fatalf("binding name %q does not match Service Contract requirement", permissionBindingName)
	}
}
