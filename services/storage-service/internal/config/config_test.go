package config

import (
	"strings"
	"testing"
)

func TestPrepareObjectStreamingOverridesGeneratedThreeSecondDefault(t *testing.T) {
	cfg := Config{}
	cfg.Timeout = 3000
	cfg.Middlewares.Timeout = true
	cfg.Middlewares.Recover = true

	cfg.PrepareObjectStreaming()

	if cfg.Timeout != minimumObjectStreamNetworkTimeoutMS {
		t.Fatalf("network timeout = %dms", cfg.Timeout)
	}
	if cfg.Middlewares.Timeout || cfg.Middlewares.Recover {
		t.Fatalf("buffering/native recovery remained enabled: %#v", cfg.Middlewares)
	}
}

func TestManagedEnvironmentUsesOnlyCompilerMaterializedInputs(t *testing.T) {
	t.Setenv("OJOS_MANAGED_WORKLOAD", "true")
	t.Setenv("OJOS_CONFIG_MODE", "production")
	t.Setenv("OJOS_CONFIG_BACKEND", "minio")
	t.Setenv("OJOS_CONFIG_BUCKETS", "problems,submissions,judge-artifacts,avatars")
	t.Setenv("OJOS_CONFIG_MINIOENDPOINT", "minio.example:9000")
	t.Setenv("OJOS_CONFIG_MINIOUSESSL", "true")
	t.Setenv("OJOS_SECRET_MINIOACCESSKEY", "materialized-access")
	t.Setenv("OJOS_SECRET_MINIOSECRETKEY", "materialized-secret")
	t.Setenv("OJOS_WORKLOAD_PUBLIC_KEY_FILE", "/run/ojos/service/workload-public-key.pem")
	t.Setenv("OJOS_WORKLOAD_KEY_ID", "workload-1")
	t.Setenv("OJOS_WORKLOAD_ISSUER", "ojos-auth/workload")
	t.Setenv("OJOS_WORKLOAD_AUDIENCE", "ojos-gateway")

	value := Config{Storage: StorageConfig{
		Backend: "local", Root: "/image-owned", Buckets: []string{"legacy"},
		MinIO: MinIOConfig{Endpoint: "legacy:9000", AccessKey: "legacy-access", SecretKey: "legacy-secret"},
	}}
	if err := ApplyEnvironment(&value); err != nil {
		t.Fatal(err)
	}
	if value.Storage.Backend != "minio" || value.Storage.Root != "" || value.Storage.MinIO.Endpoint != "minio.example:9000" || !value.Storage.MinIO.UseSSL {
		t.Fatalf("managed config retained image or legacy values: %#v", value.Storage)
	}
	if value.Storage.MinIO.AccessKey != "materialized-access" || value.Storage.MinIO.SecretKey != "materialized-secret" {
		t.Fatal("managed credentials did not come from Agent materialization")
	}
	if value.WorkloadIdentity.KeyID != "workload-1" || value.WorkloadIdentity.PublicKeyFile != "/run/ojos/service/workload-public-key.pem" {
		t.Fatal("managed workload verification key was not materialized")
	}
}

func TestManagedEnvironmentRejectsLegacyCredentialContamination(t *testing.T) {
	t.Setenv("OJOS_MANAGED_WORKLOAD", "1")
	t.Setenv("OJOS_CONFIG_MODE", "production")
	t.Setenv("OJOS_CONFIG_BACKEND", "minio")
	t.Setenv("OJOS_CONFIG_BUCKETS", "problems")
	t.Setenv("OJOS_CONFIG_MINIOENDPOINT", "minio.example:9000")
	t.Setenv("OJOS_CONFIG_MINIOUSESSL", "true")
	t.Setenv("OJOS_SECRET_MINIOACCESSKEY", "materialized-access")
	t.Setenv("OJOS_SECRET_MINIOSECRETKEY", "materialized-secret")
	t.Setenv("OJOS_WORKLOAD_PUBLIC_KEY_FILE", "/run/ojos/service/workload-public-key.pem")
	t.Setenv("OJOS_WORKLOAD_KEY_ID", "workload-1")
	t.Setenv("OJOS_WORKLOAD_ISSUER", "issuer")
	t.Setenv("OJOS_WORKLOAD_AUDIENCE", "audience")
	t.Setenv("MINIO_SECRET_KEY", "legacy-secret")

	err := ApplyEnvironment(&Config{})
	if err == nil || !strings.Contains(err.Error(), "MINIO_SECRET_KEY") {
		t.Fatalf("legacy credential contamination was not rejected: %v", err)
	}
	if strings.Contains(err.Error(), "legacy-secret") {
		t.Fatalf("configuration error leaked credential bytes: %v", err)
	}
}

func TestManagedEnvironmentRejectsLocalBackendAndMissingWorkloadKey(t *testing.T) {
	for name, configure := range map[string]func(){
		"local": func() {
			t.Setenv("OJOS_CONFIG_MODE", "production")
			t.Setenv("OJOS_CONFIG_BACKEND", "local")
			t.Setenv("OJOS_CONFIG_BUCKETS", "problems")
			t.Setenv("OJOS_CONFIG_LOCALROOT", "/data")
			t.Setenv("OJOS_WORKLOAD_PUBLIC_KEY_FILE", "/run/ojos/service/workload-public-key.pem")
			t.Setenv("OJOS_WORKLOAD_KEY_ID", "workload-1")
			t.Setenv("OJOS_WORKLOAD_ISSUER", "issuer")
			t.Setenv("OJOS_WORKLOAD_AUDIENCE", "audience")
		},
		"missing identity": func() {
			t.Setenv("OJOS_CONFIG_MODE", "production")
			t.Setenv("OJOS_CONFIG_BACKEND", "minio")
			t.Setenv("OJOS_CONFIG_BUCKETS", "problems")
			t.Setenv("OJOS_CONFIG_MINIOENDPOINT", "minio:9000")
			t.Setenv("OJOS_SECRET_MINIOACCESSKEY", "access")
			t.Setenv("OJOS_SECRET_MINIOSECRETKEY", "secret-secret")
			t.Setenv("OJOS_WORKLOAD_PUBLIC_KEY_FILE", "")
			t.Setenv("OJOS_WORKLOAD_KEY_ID", "")
		},
	} {
		t.Run(name, func(t *testing.T) {
			t.Setenv("OJOS_MANAGED_WORKLOAD", "true")
			configure()
			if err := ApplyEnvironment(&Config{}); err == nil {
				t.Fatal("invalid managed mode was accepted")
			}
		})
	}
}

func TestUnmanagedDevelopmentRetainsLocalAliases(t *testing.T) {
	t.Setenv("OJOS_MANAGED_WORKLOAD", "")
	t.Setenv("STORAGE_BACKEND", "local")
	t.Setenv("OJOS_STORAGE_ROOT", t.TempDir())
	t.Setenv("OJOS_STORAGE_BUCKETS", "problems,submissions")
	var value Config
	if err := ApplyEnvironment(&value); err != nil {
		t.Fatal(err)
	}
	if value.Storage.Backend != "local" || len(value.Storage.Buckets) != 2 {
		t.Fatalf("development aliases were not applied: %#v", value.Storage)
	}
}
