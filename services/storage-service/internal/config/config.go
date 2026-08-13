// Code scaffolded by goctl. Safe to edit.
// goctl 1.10.1

package config

import (
	"fmt"
	"os"
	"strings"

	"github.com/zeromicro/go-zero/rest"
)

const minimumObjectStreamNetworkTimeoutMS int64 = 600000

type Config struct {
	rest.RestConf

	Storage          StorageConfig
	Jaeger           JaegerConfig
	WorkloadIdentity WorkloadIdentityConfig `json:",optional"`
}

type JaegerConfig struct {
	Endpoint string
}

type StorageConfig struct {
	Backend string
	Root    string
	Buckets []string
	MinIO   MinIOConfig
}

type MinIOConfig struct {
	Endpoint  string
	AccessKey string
	SecretKey string
	UseSSL    bool
}

type WorkloadIdentityConfig struct {
	PublicKeyPEM  string `json:",optional"`
	PublicKeyFile string `json:",optional"`
	KeyID         string `json:",optional"`
	Issuer        string `json:",optional"`
	Audience      string `json:",optional"`
}

// ManagedEnvironment means the process is owned by an OJOS Agent. It is
// intentionally narrower than OJOS_ENVIRONMENT=production: Compose remains an
// unmanaged development/compatibility path, while Agent-managed workloads must
// consume only compiler-generated OJOS_CONFIG_* and OJOS_SECRET_* inputs.
func ManagedEnvironment() bool {
	value := strings.TrimSpace(os.Getenv("OJOS_MANAGED_WORKLOAD"))
	return value == "1" || strings.EqualFold(value, "true")
}

func ProductionEnvironment() bool {
	return ManagedEnvironment() || strings.EqualFold(env("OJOS_ENVIRONMENT"), "production")
}

// ApplyEnvironment applies the compiler/Agent materialization contract. In a
// managed workload, legacy endpoint and credential aliases are treated as
// contamination instead of silently winning an environment precedence race.
func ApplyEnvironment(c *Config) error {
	if c == nil {
		return fmt.Errorf("storage config is nil")
	}
	if ManagedEnvironment() {
		return applyManagedEnvironment(c)
	}
	applyDevelopmentEnvironment(c)
	return validateRuntimeMode(c, ProductionEnvironment())
}

func applyManagedEnvironment(c *Config) error {
	for _, name := range []string{
		"STORAGE_BACKEND", "OJOS_STORAGE_BACKEND", "OJOS_STORAGE_ROOT", "OJOS_STORAGE_BUCKETS",
		"MINIO_ENDPOINT", "MINIO_ACCESS_KEY", "MINIO_SECRET_KEY", "MINIO_USE_SSL",
	} {
		if strings.TrimSpace(os.Getenv(name)) != "" {
			return fmt.Errorf("managed storage rejects legacy configuration variable %s", name)
		}
	}

	// Discard every value loaded from the image-owned development YAML before
	// reading Agent materialization. This prevents image configuration from
	// becoming a second production source of truth.
	c.Storage = StorageConfig{}
	c.WorkloadIdentity = WorkloadIdentityConfig{}
	c.Storage.Backend = env("OJOS_CONFIG_BACKEND")
	c.Storage.Buckets = splitCSV(env("OJOS_CONFIG_BUCKETS"))
	c.Storage.Root = env("OJOS_CONFIG_LOCALROOT")
	c.Storage.MinIO.Endpoint = env("OJOS_CONFIG_MINIOENDPOINT")
	if value := env("OJOS_CONFIG_MINIOUSESSL"); value != "" {
		parsed, err := parseStrictBool(value)
		if err != nil {
			return fmt.Errorf("OJOS_CONFIG_MINIOUSESSL: %w", err)
		}
		c.Storage.MinIO.UseSSL = parsed
	}
	c.Storage.MinIO.AccessKey = env("OJOS_SECRET_MINIOACCESSKEY")
	c.Storage.MinIO.SecretKey = env("OJOS_SECRET_MINIOSECRETKEY")
	c.WorkloadIdentity.PublicKeyFile = env("OJOS_WORKLOAD_PUBLIC_KEY_FILE")
	c.WorkloadIdentity.KeyID = env("OJOS_WORKLOAD_KEY_ID")
	c.WorkloadIdentity.Issuer = env("OJOS_WORKLOAD_ISSUER")
	c.WorkloadIdentity.Audience = env("OJOS_WORKLOAD_AUDIENCE")
	if !strings.EqualFold(env("OJOS_CONFIG_MODE"), "production") {
		return fmt.Errorf("managed storage requires OJOS_CONFIG_MODE=production")
	}
	return validateRuntimeMode(c, true)
}

func applyDevelopmentEnvironment(c *Config) {
	if value := firstEnv("STORAGE_BACKEND", "OJOS_STORAGE_BACKEND"); value != "" {
		c.Storage.Backend = value
	}
	if value := env("OJOS_STORAGE_ROOT"); value != "" {
		c.Storage.Root = value
	}
	if value := env("OJOS_STORAGE_BUCKETS"); value != "" {
		c.Storage.Buckets = splitCSV(value)
	}
	if value := env("MINIO_ENDPOINT"); value != "" {
		c.Storage.MinIO.Endpoint = value
	}
	if value := env("MINIO_ACCESS_KEY"); value != "" {
		c.Storage.MinIO.AccessKey = value
	}
	if value := env("MINIO_SECRET_KEY"); value != "" {
		c.Storage.MinIO.SecretKey = value
	}
	if value := env("MINIO_USE_SSL"); value != "" {
		if parsed, err := parseStrictBool(value); err == nil {
			c.Storage.MinIO.UseSSL = parsed
		}
	}
	if value := env("JAEGER_ENDPOINT"); value != "" {
		c.Jaeger.Endpoint = value
	}
	if value := env("OJOS_WORKLOAD_PUBLIC_KEY_PEM"); value != "" {
		c.WorkloadIdentity.PublicKeyPEM = value
	}
	if value := env("OJOS_WORKLOAD_PUBLIC_KEY_FILE"); value != "" {
		c.WorkloadIdentity.PublicKeyFile = value
	}
	if value := env("OJOS_WORKLOAD_KEY_ID"); value != "" {
		c.WorkloadIdentity.KeyID = value
	}
	if value := env("OJOS_WORKLOAD_ISSUER"); value != "" {
		c.WorkloadIdentity.Issuer = value
	}
	if value := env("OJOS_WORKLOAD_AUDIENCE"); value != "" {
		c.WorkloadIdentity.Audience = value
	}
}

func validateRuntimeMode(c *Config, managed bool) error {
	backend := strings.ToLower(strings.TrimSpace(c.Storage.Backend))
	if backend == "" {
		backend = "local"
		c.Storage.Backend = backend
	}
	if len(c.Storage.Buckets) == 0 {
		return fmt.Errorf("at least one storage bucket is required")
	}
	if managed && backend != "minio" {
		return fmt.Errorf("managed production storage requires the minio backend")
	}
	if managed && (strings.TrimSpace(c.WorkloadIdentity.PublicKeyFile) == "" || strings.TrimSpace(c.WorkloadIdentity.KeyID) == "" || strings.TrimSpace(c.WorkloadIdentity.Issuer) == "" || strings.TrimSpace(c.WorkloadIdentity.Audience) == "") {
		return fmt.Errorf("managed production storage requires the platform workload verifier file and trust tuple")
	}
	if backend == "minio" && (strings.TrimSpace(c.Storage.MinIO.Endpoint) == "" || strings.TrimSpace(c.Storage.MinIO.AccessKey) == "" || strings.TrimSpace(c.Storage.MinIO.SecretKey) == "") {
		return fmt.Errorf("minio endpoint, access key, and secret key are required")
	}
	return nil
}

func env(name string) string { return strings.TrimSpace(os.Getenv(name)) }

func firstEnv(names ...string) string {
	for _, name := range names {
		if value := env(name); value != "" {
			return value
		}
	}
	return ""
}

func splitCSV(value string) []string {
	parts := strings.Split(value, ",")
	result := make([]string, 0, len(parts))
	for _, part := range parts {
		if part = strings.TrimSpace(part); part != "" {
			result = append(result, part)
		}
	}
	return result
}

func parseStrictBool(value string) (bool, error) {
	switch strings.ToLower(strings.TrimSpace(value)) {
	case "1", "true", "yes", "on":
		return true, nil
	case "0", "false", "no", "off":
		return false, nil
	default:
		return false, fmt.Errorf("must be a boolean")
	}
}

// PrepareObjectStreaming prevents the framework timeout middleware from
// buffering complete object bodies. Gateway/client contexts retain the
// operation-specific deadline and this value remains the socket-level bound.
func (c *Config) PrepareObjectStreaming() {
	if c.Timeout < minimumObjectStreamNetworkTimeoutMS {
		c.Timeout = minimumObjectStreamNetworkTimeoutMS
	}
	c.Middlewares.Timeout = false
	c.Middlewares.Recover = false
}
