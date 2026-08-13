package svc

import (
	"os"
	"path/filepath"
	"strings"
	"testing"

	"ojos-judge-api/internal/config"
)

func TestManagedEnvironmentUsesAgentSubmissionsResourceOutputAndDropsLegacySecrets(t *testing.T) {
	path := filepath.Join(t.TempDir(), "dsn")
	if err := os.WriteFile(path, []byte(`{"dsn":"postgres://judge:secret@database:5432/submissions?sslmode=require"}`), 0o600); err != nil {
		t.Fatal(err)
	}
	t.Setenv("OJOS_MANAGED_WORKLOAD", "true")
	t.Setenv("OJOS_RESOURCE_SUBMISSIONS_OUTPUT_FILE", path)
	t.Setenv("DATABASE_URL", "postgres://legacy:ignored@legacy:5432/legacy")
	t.Setenv("REDIS_URL", "redis://legacy:ignored@legacy:6379/0")
	t.Setenv("AUTH_SERVICE_ENDPOINT", "http://legacy-auth:8080")
	t.Setenv("AUTH_SERVICE_ADMIN_TOKEN", "legacy-auth-secret")
	t.Setenv("OJOS_STORAGE_SERVICE_ENDPOINT", "http://legacy-storage:8080")
	t.Setenv("OJOS_INTERNAL_GATEWAY_ENDPOINT", "http://legacy-gateway:8080")
	t.Setenv("OJOS_CALLER_NODE_ID", "legacy-node")
	t.Setenv("OJOS_AUTH_PERMISSION_CALLER_NODE_ID", "legacy-permission-node")
	t.Setenv("OJOS_SERVICE_TOKEN", "legacy-service-secret")
	t.Setenv("OJOS_ALLOW_LEGACY_WORKER_TOKEN", "true")
	value := config.Config{
		Redis: config.RedisConfig{Url: "redis://config-legacy:6379/0"},
		AuthService: config.AuthServiceConfig{
			Endpoint: "http://config-auth:8080", AdminToken: "config-secret",
			InternalGatewayEndpoint: "http://config-gateway:8080", ServiceToken: "config-service-secret",
		},
		Storage: config.StorageConfig{
			ServiceEndpoint: "http://config-storage:8080", InternalGatewayEndpoint: "http://config-gateway:8080",
			ServiceToken: "config-service-secret",
		},
	}
	if err := applyEnvOverrides(&value); err != nil {
		t.Fatal(err)
	}
	if value.Database.Url != "postgres://judge:secret@database:5432/submissions?sslmode=require" {
		t.Fatalf("managed DSN did not come from resource output")
	}
	if value.Redis.Url != "" || value.AuthService.Endpoint != "" || value.AuthService.AdminToken != "" ||
		value.AuthService.InternalGatewayEndpoint != "" || value.AuthService.CallerNodeID != "" || value.AuthService.ServiceToken != "" ||
		value.Storage.ServiceEndpoint != "" || value.Storage.InternalGatewayEndpoint != "" ||
		value.Storage.CallerNodeID != "" || value.Storage.ServiceToken != "" {
		t.Fatal("managed configuration retained a legacy URL or token")
	}
	if value.WorkloadIdentity.AllowLegacyWorkerToken {
		t.Fatal("managed configuration enabled the legacy Worker token")
	}
}

func TestManagedEnvironmentRejectsMissingResourceOutputWithoutLeakingDSN(t *testing.T) {
	t.Setenv("OJOS_MANAGED_WORKLOAD", "1")
	t.Setenv("OJOS_RESOURCE_SUBMISSIONS_OUTPUT_FILE", filepath.Join(t.TempDir(), "missing"))
	var value config.Config
	err := applyEnvOverrides(&value)
	if err == nil {
		t.Fatal("expected missing resource output to fail closed")
	}
	if strings.Contains(err.Error(), "postgres://") {
		t.Fatalf("resource error leaked credential material: %v", err)
	}
}

func TestUnmanagedEnvironmentRetainsLegacyDevelopmentAliases(t *testing.T) {
	t.Setenv("OJOS_MANAGED_WORKLOAD", "")
	t.Setenv("OJOS_ENVIRONMENT", "development")
	t.Setenv("DATABASE_URL", "postgres://local:secret@database:5432/judge")
	t.Setenv("REDIS_URL", "redis://local:6379/0")
	var value config.Config
	if err := applyEnvOverrides(&value); err != nil {
		t.Fatal(err)
	}
	if value.Database.Url != "postgres://local:secret@database:5432/judge" || value.Redis.Url != "redis://local:6379/0" {
		t.Fatal("development aliases were not retained")
	}
}

func TestUnmanagedPermissionCallerNodeFallsBackToTheWorkloadNode(t *testing.T) {
	t.Setenv("OJOS_MANAGED_WORKLOAD", "")
	t.Setenv("OJOS_ENVIRONMENT", "development")
	t.Setenv("OJOS_CALLER_NODE_ID", "child-node")

	var value config.Config
	if err := applyEnvOverrides(&value); err != nil {
		t.Fatal(err)
	}
	if value.AuthService.CallerNodeID != "child-node" {
		t.Fatalf("permission caller node = %q, want child-node", value.AuthService.CallerNodeID)
	}
	if value.Storage.CallerNodeID != "child-node" {
		t.Fatalf("storage caller node = %q, want child-node", value.Storage.CallerNodeID)
	}
}

func TestUnmanagedPermissionCallerNodePrefersItsDedicatedOverride(t *testing.T) {
	t.Setenv("OJOS_MANAGED_WORKLOAD", "")
	t.Setenv("OJOS_ENVIRONMENT", "development")
	t.Setenv("OJOS_AUTH_PERMISSION_CALLER_NODE_ID", "permission-node")
	t.Setenv("OJOS_CALLER_NODE_ID", "storage-node")

	var value config.Config
	if err := applyEnvOverrides(&value); err != nil {
		t.Fatal(err)
	}
	if value.AuthService.CallerNodeID != "permission-node" {
		t.Fatalf("permission caller node = %q, want permission-node", value.AuthService.CallerNodeID)
	}
	if value.Storage.CallerNodeID != "storage-node" {
		t.Fatalf("storage caller node = %q, want storage-node", value.Storage.CallerNodeID)
	}
}

func TestRequiredBindingNamesUseV3ContractRequirementIDs(t *testing.T) {
	want := map[string]string{
		"permission": "auth.user.permission.check",
		"get":        "storage.object.get",
		"put":        "storage.object.put",
		"head":       "storage.object.head",
	}
	got := map[string]string{
		"permission": permissionBindingName,
		"get":        storageGetBinding,
		"put":        storagePutBinding,
		"head":       storageHeadBinding,
	}
	for name := range want {
		if got[name] != want[name] {
			t.Fatalf("%s binding = %q, want %q", name, got[name], want[name])
		}
	}
}

func TestCompiledConfigurationAliasesReachRuntimeControls(t *testing.T) {
	t.Setenv("OJOS_MANAGED_WORKLOAD", "")
	t.Setenv("OJOS_ENVIRONMENT", "development")
	t.Setenv("OJOS_CONFIG_SUBMISSION_MAXCODEBYTES", "524288")
	t.Setenv("OJOS_CONFIG_WORKER_LEASETTLSECONDS", "90")
	var value config.Config
	if err := applyEnvOverrides(&value); err != nil {
		t.Fatal(err)
	}
	if value.Submission.MaxCodeBytes != 524288 || value.WorkerAuth.LeaseTTLSeconds != 90 {
		t.Fatalf("compiled configuration did not reach runtime controls: %#v", value)
	}
}
