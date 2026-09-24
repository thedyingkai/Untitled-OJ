// Runtime configuration normalization and validation.
package svc

import (
	"fmt"
	"os"
	"strings"
	"time"

	"ojos-problem-service/internal/config"
	"ojos-shared/resourceoutput"
	sharedperm "ojos-shared/security/permission"
)

func envBool(key string) bool {
	value := strings.TrimSpace(os.Getenv(key))
	return value == "1" || strings.EqualFold(value, "true")
}

func envBoolDefault(key string, fallback bool) bool {
	value := firstEnv(contractConfigEnv(key), key)
	if value == "" {
		return fallback
	}
	return value == "1" || strings.EqualFold(value, "true")
}

func envDuration(key string, fallback time.Duration) (time.Duration, error) {
	value := firstEnv(contractConfigEnv(key), key)
	if value == "" {
		return fallback, nil
	}
	return time.ParseDuration(value)
}

// permissionCheckerConfig keeps the routing decision in one place: gateway +
// api_id first, direct auth-service address only as a fallback.
func permissionCheckerConfig(c config.Config) sharedperm.RemoteCheckerConfig {
	return sharedperm.RemoteCheckerConfig{
		InternalGatewayEndpoint: c.AuthService.InternalGatewayEndpoint,
		ApiID:                   c.AuthService.PermissionCheckApiID,
		CallerService:           c.AuthService.CallerService,
		CallerNodeID:            c.AuthService.CallerNodeID,
		ServiceToken:            c.AuthService.ServiceToken,
		AuthServiceEndpoint:     c.AuthService.Endpoint,
		AuthServiceAdminToken:   c.AuthService.AdminToken,
	}
}

func applyEnvOverrides(c *config.Config) error {
	managed := managedEnvironment()
	if managed {
		path := firstEnv("OJOS_RESOURCE_PROBLEMS_OUTPUT_FILE", "OJOS_RESOURCE_OUTPUT_FILE")
		if path == "" {
			path = defaultProblemsOutputFile
		}
		dsn, err := resourceoutput.ReadPostgreSQLDSN(path)
		if err != nil {
			return fmt.Errorf("load problems resource output: %w", err)
		}
		c.Database.Url = dsn
		// The signed runtime volume contract owns this target. Managed workloads
		// must never let inherited configuration redirect mutation journals or
		// authoring trees onto the container root filesystem.
		c.Storage.ProblemsRoot = managedProblemsRoot
	} else if value := firstEnv("PROBLEM_DATABASE_URL", "DATABASE_URL", "POSTGRES_DSN"); value != "" {
		c.Database.Url = value
	}
	if value := strings.TrimSpace(os.Getenv("JAEGER_ENDPOINT")); value != "" {
		c.Jaeger.Endpoint = value
	}
	if !managed {
		if value := strings.TrimSpace(os.Getenv("OJOS_PROBLEMS_ROOT")); value != "" {
			c.Storage.ProblemsRoot = value
		}
	}
	if value := firstEnv("OJOS_CONFIG_STORAGE_BUCKET", "OJOS_PROBLEM_STORAGE_BUCKET"); value != "" {
		c.Storage.Bucket = value
	}
	if managed {
		// Agent materialization is the only production trust path. In
		// particular, legacy direct service URLs and bearer tokens must not be
		// copied into the effective managed configuration even when inherited
		// from an old Compose environment.
		return nil
	}
	if value := strings.TrimSpace(os.Getenv("REDIS_URL")); value != "" {
		c.Redis.Url = value
	}
	if value := strings.TrimSpace(os.Getenv("AUTH_SERVICE_ENDPOINT")); value != "" {
		c.AuthService.Endpoint = value
	}
	if value := firstEnv("AUTH_SERVICE_ADMIN_TOKEN", "AUTH_INTERNAL_TOKEN"); value != "" {
		c.AuthService.AdminToken = value
	}
	if value := strings.TrimSpace(os.Getenv("OJOS_STORAGE_SERVICE_URL")); value != "" {
		c.Storage.ServiceEndpoint = value
	}
	if value := firstEnv("OJOS_INTERNAL_GATEWAY_ENDPOINT", "OJOS_INTERNAL_GATEWAY_URL"); value != "" {
		c.Storage.InternalGatewayEndpoint = value
	}
	if value := strings.TrimSpace(os.Getenv("OJOS_STORAGE_OBJECT_PUT_API_ID")); value != "" {
		c.Storage.PutApiID = value
	}
	if value := strings.TrimSpace(os.Getenv("OJOS_STORAGE_OBJECT_HEAD_API_ID")); value != "" {
		c.Storage.HeadApiID = value
	}
	if value := firstEnv("OJOS_PROBLEM_SERVICE_TOKEN", "OJOS_SERVICE_TOKEN"); value != "" {
		c.Storage.ServiceToken = value
	}
	if value := strings.TrimSpace(os.Getenv("OJOS_CALLER_SERVICE")); value != "" {
		c.Storage.CallerService = value
	}
	if value := firstEnv("OJOS_CALLER_NODE_ID", "OJOS_NODE_ID"); value != "" {
		c.Storage.CallerNodeID = value
	}
	// Deliberately a dedicated variable rather than reusing
	// OJOS_INTERNAL_GATEWAY_ENDPOINT / OJOS_INTERNAL_GATEWAY_URL (which already
	// drive the storage client): switching the permission check onto the gateway
	// also requires a service credential and a service permission grant, so it
	// must be an explicit opt-in per deployment.
	if value := strings.TrimSpace(os.Getenv("OJOS_AUTH_PERMISSION_GATEWAY_ENDPOINT")); value != "" {
		c.AuthService.InternalGatewayEndpoint = value
	}
	if value := strings.TrimSpace(os.Getenv("OJOS_AUTH_PERMISSION_CHECK_API_ID")); value != "" {
		c.AuthService.PermissionCheckApiID = value
	}
	if value := strings.TrimSpace(os.Getenv("OJOS_CALLER_SERVICE")); value != "" {
		c.AuthService.CallerService = value
	}
	if value := firstEnv("OJOS_CALLER_NODE_ID", "OJOS_NODE_ID"); value != "" {
		c.AuthService.CallerNodeID = value
	}
	if value := firstEnv("OJOS_PROBLEM_SERVICE_TOKEN", "OJOS_SERVICE_TOKEN"); value != "" {
		c.AuthService.ServiceToken = value
	}
	return nil
}

func contractConfigEnv(legacy string) string {
	switch legacy {
	case "OJOS_PROBLEM_ARTIFACT_GC_ENABLED":
		return "OJOS_CONFIG_ARTIFACTGC_ENABLED"
	case "OJOS_PROBLEM_ARTIFACT_GC_DELETE":
		return "OJOS_CONFIG_ARTIFACTGC_DELETE"
	case "OJOS_PROBLEM_ARTIFACT_GC_RETENTION":
		return "OJOS_CONFIG_ARTIFACTGC_RETENTION"
	case "OJOS_PROBLEM_ARTIFACT_GC_INTERVAL":
		return "OJOS_CONFIG_ARTIFACTGC_INTERVAL"
	case "OJOS_PROBLEM_ARTIFACT_GC_CLAIM_LEASE":
		return "OJOS_CONFIG_ARTIFACTGC_CLAIMLEASE"
	default:
		return ""
	}
}

func managedEnvironment() bool {
	value := strings.TrimSpace(os.Getenv("OJOS_MANAGED_WORKLOAD"))
	return value == "1" || strings.EqualFold(value, "true") ||
		strings.EqualFold(strings.TrimSpace(os.Getenv("OJOS_ENVIRONMENT")), "production")
}

func firstEnv(keys ...string) string {
	for _, key := range keys {
		if value := strings.TrimSpace(os.Getenv(key)); value != "" {
			return value
		}
	}
	return ""
}
