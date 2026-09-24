// Runtime configuration normalization and validation.
package svc

import (
	"errors"
	"fmt"
	"os"
	"strconv"
	"strings"

	"ojos-judge-api/internal/config"
	"ojos-shared/resourceoutput"
	sharedperm "ojos-shared/security/permission"
)

func validateWorkerIdentityMode(c config.Config, verifierConfigured bool, environment string) error {
	if !strings.EqualFold(strings.TrimSpace(environment), "production") {
		return nil
	}
	if !verifierConfigured {
		return fmt.Errorf("production requires OJOS_WORKLOAD_PUBLIC_KEY_FILE")
	}
	if c.WorkloadIdentity.AllowLegacyWorkerToken || strings.TrimSpace(c.WorkerAuth.Token) != "" {
		return fmt.Errorf("production forbids the legacy shared Worker token")
	}
	return nil
}

func validateProblemProjectionMode(c config.Config, environment string) error {
	if !c.ProblemProjection.AllowLegacyPackageDir {
		return nil
	}
	if !strings.EqualFold(strings.TrimSpace(environment), "development") {
		return fmt.Errorf("legacy package_dir submissions are allowed only when OJOS_ENVIRONMENT=development; complete Problem projection backfill/reconcile first")
	}
	return nil
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
		path := firstEnv("OJOS_RESOURCE_SUBMISSIONS_OUTPUT_FILE", "OJOS_RESOURCE_OUTPUT_FILE")
		if path == "" {
			path = defaultSubmissionsOutputFile
		}
		dsn, err := resourceoutput.ReadPostgreSQLDSN(path)
		if err != nil {
			return fmt.Errorf("load submissions resource output: %w", err)
		}
		c.Database.Url = dsn
	} else if value := firstEnv("JUDGE_DATABASE_URL", "DATABASE_URL", "POSTGRES_DSN"); value != "" {
		c.Database.Url = value
	}
	if !managed {
		// Direct URLs and long-lived service tokens are development-only escape
		// hatches. A managed workload receives endpoints, TLS roots and rotated
		// credentials exclusively through Agent materialization.
		if value := strings.TrimSpace(os.Getenv("REDIS_URL")); value != "" {
			c.Redis.Url = value
		}
		if value := strings.TrimSpace(os.Getenv("AUTH_SERVICE_ENDPOINT")); value != "" {
			c.AuthService.Endpoint = value
		}
		if value := firstEnv("AUTH_SERVICE_ADMIN_TOKEN", "AUTH_INTERNAL_TOKEN"); value != "" {
			c.AuthService.AdminToken = value
		}
		if value := strings.TrimSpace(os.Getenv("OJOS_STORAGE_SERVICE_ENDPOINT")); value != "" {
			c.Storage.ServiceEndpoint = value
		}
		if value := strings.TrimSpace(os.Getenv("OJOS_INTERNAL_GATEWAY_ENDPOINT")); value != "" {
			c.Storage.InternalGatewayEndpoint = value
		}
		if value := strings.TrimSpace(os.Getenv("OJOS_CALLER_NODE_ID")); value != "" {
			c.Storage.CallerNodeID = value
		}
		if value := strings.TrimSpace(os.Getenv("OJOS_SERVICE_TOKEN")); value != "" {
			c.Storage.ServiceToken = value
		}
		if value := strings.TrimSpace(os.Getenv("OJOS_AUTH_PERMISSION_GATEWAY_ENDPOINT")); value != "" {
			c.AuthService.InternalGatewayEndpoint = value
		}
		if value := firstEnv(
			"OJOS_AUTH_PERMISSION_CALLER_NODE_ID",
			"OJOS_CALLER_NODE_ID",
			"OJOS_NODE_ID",
		); value != "" {
			c.AuthService.CallerNodeID = value
		}
		if value := firstEnv("OJOS_JUDGE_API_SERVICE_TOKEN", "OJOS_SERVICE_TOKEN"); value != "" {
			c.AuthService.ServiceToken = value
		}
	} else {
		// Clear values supplied by a legacy configuration file as well as env.
		c.Redis.Url = ""
		c.AuthService.Endpoint = ""
		c.AuthService.AdminToken = ""
		c.AuthService.InternalGatewayEndpoint = ""
		c.AuthService.CallerNodeID = ""
		c.AuthService.ServiceToken = ""
		c.Storage.ServiceEndpoint = ""
		c.Storage.InternalGatewayEndpoint = ""
		c.Storage.CallerNodeID = ""
		c.Storage.ServiceToken = ""
	}
	if value := strings.TrimSpace(os.Getenv("JAEGER_ENDPOINT")); value != "" {
		c.Jaeger.Endpoint = value
	}
	if value := strings.TrimSpace(os.Getenv("OJOS_SUBMISSIONS_ROOT")); value != "" && !managed {
		c.Storage.SubmissionsRoot = value
	}
	if value := strings.TrimSpace(os.Getenv("OJOS_STORAGE_OBJECT_GET_API_ID")); value != "" && !managed {
		c.Storage.GetApiID = value
	}
	if value := strings.TrimSpace(os.Getenv("OJOS_STORAGE_OBJECT_PUT_API_ID")); value != "" && !managed {
		c.Storage.PutApiID = value
	}
	if value := strings.TrimSpace(os.Getenv("OJOS_STORAGE_OBJECT_HEAD_API_ID")); value != "" && !managed {
		c.Storage.HeadApiID = value
	}
	if value := strings.TrimSpace(os.Getenv("OJOS_STORAGE_SUBMISSIONS_BUCKET")); value != "" {
		c.Storage.Bucket = value
	}
	if value := strings.TrimSpace(os.Getenv("OJOS_CONFIG_SUBMISSION_MAXCODEBYTES")); value != "" {
		parsed, err := strconv.ParseInt(value, 10, 64)
		if err != nil || parsed < 1024 || parsed > 10*1024*1024 {
			return errors.New("OJOS_CONFIG_SUBMISSION_MAXCODEBYTES is invalid")
		}
		c.Submission.MaxCodeBytes = parsed
	}
	if value := strings.TrimSpace(os.Getenv("OJOS_CONFIG_WORKER_LEASETTLSECONDS")); value != "" {
		parsed, err := strconv.ParseInt(value, 10, 64)
		if err != nil || parsed < 10 || parsed > 3600 {
			return errors.New("OJOS_CONFIG_WORKER_LEASETTLSECONDS is invalid")
		}
		c.WorkerAuth.LeaseTTLSeconds = parsed
	}
	if value := strings.TrimSpace(os.Getenv("OJOS_CALLER_SERVICE")); value != "" && !managed {
		c.Storage.CallerService = value
	}
	if value := strings.TrimSpace(os.Getenv("OJOS_WORKLOAD_PUBLIC_KEY_FILE")); value != "" {
		c.WorkloadIdentity.PublicKeyFile = value
	}
	if value := strings.TrimSpace(os.Getenv("OJOS_WORKLOAD_KEY_ID")); value != "" {
		c.WorkloadIdentity.KeyID = value
	}
	if value := strings.TrimSpace(os.Getenv("OJOS_WORKLOAD_ISSUER")); value != "" {
		c.WorkloadIdentity.Issuer = value
	}
	if value := strings.TrimSpace(os.Getenv("OJOS_WORKLOAD_AUDIENCE")); value != "" {
		c.WorkloadIdentity.Audience = value
	}
	if value := strings.TrimSpace(os.Getenv("OJOS_ALLOW_LEGACY_WORKER_TOKEN")); value != "" && !managed {
		c.WorkloadIdentity.AllowLegacyWorkerToken = value == "1" || strings.EqualFold(value, "true")
	}
	if value := strings.TrimSpace(os.Getenv("OJOS_ALLOW_LEGACY_PROBLEM_PACKAGE_DIR")); value != "" && !managed {
		c.ProblemProjection.AllowLegacyPackageDir = value == "1" || strings.EqualFold(value, "true")
	}
	if value := strings.TrimSpace(os.Getenv("OJOS_AUTH_PERMISSION_CHECK_API_ID")); value != "" && !managed {
		c.AuthService.PermissionCheckApiID = value
	}
	if value := strings.TrimSpace(os.Getenv("OJOS_AUTH_PERMISSION_CALLER_SERVICE")); value != "" && !managed {
		c.AuthService.CallerService = value
	}
	return nil
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
