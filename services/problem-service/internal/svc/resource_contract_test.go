package svc

import (
	"encoding/json"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"

	"ojos-problem-service/internal/config"
)

func TestGeneratedContractPinsRetainedProblemPackageVolume(t *testing.T) {
	bytes, err := os.ReadFile(filepath.Join("..", "..", "gen", "service.contract.json"))
	if err != nil {
		t.Fatal(err)
	}
	var contract struct {
		Runtime struct {
			Volumes []struct {
				Name      string `json:"name"`
				Kind      string `json:"kind"`
				Target    string `json:"target"`
				Access    string `json:"access"`
				Lifecycle string `json:"lifecycle"`
			} `json:"volumes"`
		} `json:"runtime"`
	}
	if err := json.Unmarshal(bytes, &contract); err != nil {
		t.Fatal(err)
	}
	if len(contract.Runtime.Volumes) != 1 {
		t.Fatalf("signed runtime volumes = %d, want exactly one", len(contract.Runtime.Volumes))
	}
	volume := contract.Runtime.Volumes[0]
	if volume.Name != "problem-packages" || volume.Kind != "managed-volume" ||
		volume.Target != managedProblemsRoot || volume.Access != "rw" || volume.Lifecycle != "retain" {
		t.Fatalf("unexpected signed problem package volume: %#v", volume)
	}
}

func TestRuntimeImageSeedsRetainedVolumeTargetForNonRootUser(t *testing.T) {
	bytes, err := os.ReadFile(filepath.Join("..", "..", "Dockerfile"))
	if err != nil {
		t.Fatal(err)
	}
	dockerfile := string(bytes)
	for _, required := range []string{
		"RUN mkdir -p /data/ojos/problems && chown -R 65532:65532 /data/ojos",
		"USER 65532:65532",
	} {
		if !strings.Contains(dockerfile, required) {
			t.Fatalf("runtime image no longer seeds RETAIN volume ownership: missing %q", required)
		}
	}
}

func TestManagedEnvironmentUsesAgentProblemsResourceOutput(t *testing.T) {
	path := filepath.Join(t.TempDir(), "dsn")
	if err := os.WriteFile(path, []byte(`{"dsn":"postgres://problem:secret@database:5432/problems?sslmode=require"}`), 0o600); err != nil {
		t.Fatal(err)
	}
	t.Setenv("OJOS_MANAGED_WORKLOAD", "true")
	t.Setenv("OJOS_RESOURCE_PROBLEMS_OUTPUT_FILE", path)
	t.Setenv("DATABASE_URL", "postgres://legacy:ignored@legacy:5432/legacy")
	t.Setenv("REDIS_URL", "redis://legacy:ignored@legacy:6379/0")
	t.Setenv("AUTH_SERVICE_ENDPOINT", "http://legacy-auth:8080")
	t.Setenv("AUTH_SERVICE_ADMIN_TOKEN", "legacy-auth-secret")
	t.Setenv("OJOS_STORAGE_SERVICE_URL", "http://legacy-storage:8080")
	t.Setenv("OJOS_INTERNAL_GATEWAY_ENDPOINT", "http://legacy-gateway:8080")
	t.Setenv("OJOS_PROBLEM_SERVICE_TOKEN", "legacy-service-secret")
	t.Setenv("OJOS_PROBLEMS_ROOT", filepath.Join(t.TempDir(), "legacy-root"))
	var value config.Config
	if err := applyEnvOverrides(&value); err != nil {
		t.Fatal(err)
	}
	if value.Database.Url != "postgres://problem:secret@database:5432/problems?sslmode=require" {
		t.Fatalf("managed DSN did not come from resource output")
	}
	if value.Redis.Url != "" || value.AuthService.Endpoint != "" || value.AuthService.AdminToken != "" ||
		value.AuthService.InternalGatewayEndpoint != "" || value.AuthService.ServiceToken != "" ||
		value.Storage.ServiceEndpoint != "" || value.Storage.InternalGatewayEndpoint != "" || value.Storage.ServiceToken != "" {
		t.Fatal("managed configuration retained a legacy URL or token")
	}
	if value.Storage.ProblemsRoot != managedProblemsRoot {
		t.Fatalf("managed problems root = %q, want signed volume target %q", value.Storage.ProblemsRoot, managedProblemsRoot)
	}
}

func TestProbeProblemsRootExercisesRenameAndCleansUp(t *testing.T) {
	root, err := filepath.Abs(t.TempDir())
	if err != nil {
		t.Fatal(err)
	}
	if err := probeProblemsRoot(root); err != nil {
		t.Fatal(err)
	}
	entries, err := os.ReadDir(root)
	if err != nil {
		t.Fatal(err)
	}
	if len(entries) != 0 {
		t.Fatalf("readiness probe left files behind: %v", entries)
	}
}

func TestProbeProblemsRootRejectsNonDirectory(t *testing.T) {
	path := filepath.Join(t.TempDir(), "not-a-volume")
	if err := os.WriteFile(path, []byte("not a directory"), 0o600); err != nil {
		t.Fatal(err)
	}
	if err := probeProblemsRoot(path); err == nil {
		t.Fatal("readiness accepted a non-directory problems root")
	}
}

func TestManagedEnvironmentRejectsMissingResourceOutputWithoutLeakingDSN(t *testing.T) {
	t.Setenv("OJOS_MANAGED_WORKLOAD", "1")
	t.Setenv("OJOS_RESOURCE_PROBLEMS_OUTPUT_FILE", filepath.Join(t.TempDir(), "missing"))
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
	t.Setenv("DATABASE_URL", "postgres://local:secret@database:5432/problems")
	var value config.Config
	if err := applyEnvOverrides(&value); err != nil {
		t.Fatal(err)
	}
	if value.Database.Url != "postgres://local:secret@database:5432/problems" {
		t.Fatal("development database alias was not retained")
	}
}

func TestRequiredBindingNamesUseContractRequirementIDs(t *testing.T) {
	want := map[string]string{
		"permission": "auth.user.permission.check",
		"put":        "storage.object.put",
		"head":       "storage.object.head",
		"delete":     "storage.object.delete",
	}
	got := map[string]string{
		"permission": permissionBindingName,
		"put":        storagePutBinding,
		"head":       storageHeadBinding,
		"delete":     storageDeleteBinding,
	}
	for name := range want {
		if got[name] != want[name] {
			t.Fatalf("%s binding = %q, want %q", name, got[name], want[name])
		}
	}
}

func TestCompiledConfigurationAliasesReachExistingRuntimeControls(t *testing.T) {
	t.Setenv("OJOS_MANAGED_WORKLOAD", "")
	t.Setenv("OJOS_ENVIRONMENT", "development")
	t.Setenv("OJOS_CONFIG_STORAGE_BUCKET", "problem-packages")
	t.Setenv("OJOS_CONFIG_ARTIFACTGC_RETENTION", "240h")
	t.Setenv("OJOS_PROBLEM_ARTIFACT_GC_RETENTION", "legacy-ignored")
	var value config.Config
	if err := applyEnvOverrides(&value); err != nil {
		t.Fatal(err)
	}
	if value.Storage.Bucket != "problem-packages" {
		t.Fatalf("compiled storage bucket was not applied: %q", value.Storage.Bucket)
	}
	duration, err := envDuration("OJOS_PROBLEM_ARTIFACT_GC_RETENTION", 0)
	if err != nil || duration != 240*time.Hour {
		t.Fatalf("compiled artifact GC retention did not override legacy alias: duration=%s err=%v", duration, err)
	}
}
