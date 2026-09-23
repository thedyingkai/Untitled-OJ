package svc

import (
	"errors"
	"fmt"
	"log"
	"os"
	"strconv"
	"strings"
	"time"

	"ojos-auth-service/internal/config"

	"ojos-shared/resourceoutput"
	"ojos-shared/security/workload"
)

const defaultAuthResourceFile = "/run/ojos/resources/auth/dsn"

func applyEnvOverrides(c *config.Config) error {
	managed := managedEnvironment()
	bootstrap := platformBootstrapEnvironment()
	production := productionModeEnabled()
	if managed && bootstrap {
		return errors.New("Auth cannot be both Agent-managed and a platform bootstrap service")
	}
	if managed {
		// The checked-in YAML is an unmanaged development default. Managed
		// workloads reconstruct every sensitive/runtime-bound field exclusively
		// from Agent materialization so a legacy value cannot silently survive.
		clearManagedRuntimeFields(c)
		path := firstEnv("OJOS_RESOURCE_AUTH_OUTPUT_FILE", "OJOS_RESOURCE_OUTPUT_FILE")
		if path == "" {
			path = defaultAuthResourceFile
		}
		dsn, err := resourceoutput.ReadPostgreSQLDSN(path)
		if err != nil {
			return fmt.Errorf("load Auth resource output: %w", err)
		}
		c.Database.Url = dsn
	} else if bootstrap {
		if !strings.EqualFold(strings.TrimSpace(os.Getenv("OJOS_ENVIRONMENT")), "production") {
			return errors.New("platform bootstrap Auth requires OJOS_ENVIRONMENT=production")
		}
		if err := rejectPlatformBootstrapMaterializationAliases(); err != nil {
			return err
		}
		clearManagedRuntimeFields(c)
		if err := applyPlatformBootstrapEnv(c); err != nil {
			return err
		}
	} else if production {
		return errors.New("production Auth requires OJOS_MANAGED_WORKLOAD=1 or OJOS_PLATFORM_BOOTSTRAP=1")
	} else if value := firstEnv("AUTH_DATABASE_URL", "DATABASE_URL", "POSTGRES_DSN"); value != "" {
		c.Database.Url = value
	}
	if !managed && !bootstrap && !production {
		applyDevelopmentEnvOverrides(c)
	}
	if value := strings.TrimSpace(os.Getenv("OJOS_SECRET_JWT_SECRET")); value != "" {
		c.Jwt.Secret = value
	}
	if value := strings.TrimSpace(os.Getenv("OJOS_CONFIG_JWT_EXPIREHOURS")); value != "" {
		hours, err := strconv.Atoi(value)
		if err != nil || hours < 1 || hours > 168 {
			return errors.New("OJOS_CONFIG_JWT_EXPIREHOURS is invalid")
		}
		c.Jwt.ExpireHours = hours
	}
	if value := strings.TrimSpace(os.Getenv("OJOS_SECRET_MANAGEMENT_TOKEN")); value != "" {
		c.InternalAuth.Token = value
	}
	if value := strings.TrimSpace(os.Getenv("OJOS_SECRET_ADMINBOOTSTRAP_SECRET")); value != "" {
		c.AdminBootstrap.Secret = value
		c.AdminBootstrap.SecretFile = ""
	}
	if value := strings.TrimSpace(os.Getenv("OJOS_SECRET_WORKLOAD_PRIVATEKEYPEM")); value != "" {
		c.WorkloadIdentity.PrivateKeyPEM = value
		c.WorkloadIdentity.PrivateKeyFile = ""
	}
	if value := strings.TrimSpace(os.Getenv("OJOS_SECRET_WORKLOAD_CONTROLPLANETOKEN")); value != "" {
		c.WorkloadIdentity.ControlPlaneToken = value
	}
	if value := strings.TrimSpace(os.Getenv("OJOS_CONFIG_WORKLOAD_KEYID")); value != "" {
		c.WorkloadIdentity.KeyID = value
	}
	if value := strings.TrimSpace(os.Getenv("OJOS_CONFIG_WORKLOAD_ISSUER")); value != "" {
		c.WorkloadIdentity.Issuer = value
	}
	if value := strings.TrimSpace(os.Getenv("OJOS_CONFIG_WORKLOAD_AUDIENCE")); value != "" {
		c.WorkloadIdentity.Audience = value
	}
	if value := strings.TrimSpace(os.Getenv("OJOS_CONFIG_WORKLOAD_TTLSECONDS")); value != "" {
		ttl, err := strconv.ParseInt(value, 10, 64)
		if err != nil || ttl != int64(workload.DefaultTTL/time.Second) {
			return errors.New("OJOS_CONFIG_WORKLOAD_TTLSECONDS must be 900")
		}
		c.WorkloadIdentity.TTLSeconds = ttl
	}
	if value := strings.TrimSpace(os.Getenv("OJOS_CONFIG_ORCHESTRATOR_ENDPOINT")); value != "" {
		c.Orchestrator.Endpoint = value
	}
	if value := strings.TrimSpace(os.Getenv("OJOS_SECRET_ORCHESTRATOR_INTERNALTOKEN")); value != "" {
		c.Orchestrator.InternalToken = value
	}
	if value := strings.TrimSpace(os.Getenv("OJOS_SECRET_ORCHESTRATOR_CONTRIBUTIONACKTOKEN")); value != "" {
		c.Orchestrator.ContributionAckToken = value
	}
	if value := strings.TrimSpace(os.Getenv("OJOS_CONFIG_TRACING_ENDPOINT")); value != "" {
		c.Jaeger.Endpoint = value
	}
	if managed {
		if c.Jwt.Secret == "" || c.InternalAuth.Token == "" || c.WorkloadIdentity.PrivateKeyPEM == "" || c.WorkloadIdentity.ControlPlaneToken == "" {
			return errors.New("managed Auth requires Agent-materialized JWT, management, and workload identity secrets")
		}
		if c.Orchestrator.Endpoint == "" || c.Orchestrator.InternalToken == "" || c.Orchestrator.ContributionAckToken == "" {
			return errors.New("managed Auth requires Agent-materialized Orchestrator projection configuration")
		}
	}
	if bootstrap {
		if c.Database.Url == "" || c.Jwt.Secret == "" || c.InternalAuth.Token == "" || c.WorkloadIdentity.PrivateKeyFile == "" || c.WorkloadIdentity.ControlPlaneToken == "" {
			return errors.New("platform bootstrap Auth requires explicit database, JWT, management, and workload identity configuration")
		}
		if c.Orchestrator.Endpoint == "" || c.Orchestrator.InternalToken == "" || c.Orchestrator.ContributionAckToken == "" {
			return errors.New("platform bootstrap Auth requires explicit Orchestrator projection configuration")
		}
	}
	return nil
}

func rejectPlatformBootstrapMaterializationAliases() error {
	for _, name := range []string{
		"AUTH_ADMIN_BOOTSTRAP_SECRET",
		"OJOS_SECRET_JWT_SECRET",
		"OJOS_CONFIG_JWT_EXPIREHOURS",
		"OJOS_SECRET_MANAGEMENT_TOKEN",
		"OJOS_SECRET_ADMINBOOTSTRAP_SECRET",
		"OJOS_SECRET_WORKLOAD_PRIVATEKEYPEM",
		"OJOS_SECRET_WORKLOAD_CONTROLPLANETOKEN",
		"OJOS_CONFIG_WORKLOAD_KEYID",
		"OJOS_CONFIG_WORKLOAD_ISSUER",
		"OJOS_CONFIG_WORKLOAD_AUDIENCE",
		"OJOS_CONFIG_WORKLOAD_TTLSECONDS",
		"OJOS_CONFIG_ORCHESTRATOR_ENDPOINT",
		"OJOS_SECRET_ORCHESTRATOR_INTERNALTOKEN",
		"OJOS_SECRET_ORCHESTRATOR_CONTRIBUTIONACKTOKEN",
		"OJOS_CONFIG_TRACING_ENDPOINT",
	} {
		if strings.TrimSpace(os.Getenv(name)) != "" {
			return fmt.Errorf("platform bootstrap Auth forbids Agent materialization variable %s", name)
		}
	}
	return nil
}

func applyPlatformBootstrapEnv(c *config.Config) error {
	required := func(name string) (string, error) {
		value := strings.TrimSpace(os.Getenv(name))
		if value == "" {
			return "", fmt.Errorf("platform bootstrap Auth requires %s", name)
		}
		return value, nil
	}
	var err error
	if c.Database.Url, err = required("AUTH_DATABASE_URL"); err != nil {
		return err
	}
	if c.Jwt.Secret, err = required("JWT_SECRET"); err != nil {
		return err
	}
	if c.InternalAuth.Token, err = required("AUTH_INTERNAL_TOKEN"); err != nil {
		return err
	}
	if c.WorkloadIdentity.PrivateKeyFile, err = required("OJOS_WORKLOAD_PRIVATE_KEY_FILE"); err != nil {
		return err
	}
	if c.WorkloadIdentity.ControlPlaneToken, err = required("ORCHESTRATOR_AUTH_WORKLOAD_TOKEN"); err != nil {
		return err
	}
	if c.WorkloadIdentity.KeyID, err = required("OJOS_WORKLOAD_KEY_ID"); err != nil {
		return err
	}
	if c.WorkloadIdentity.Issuer, err = required("OJOS_WORKLOAD_ISSUER"); err != nil {
		return err
	}
	if c.WorkloadIdentity.Audience, err = required("OJOS_WORKLOAD_AUDIENCE"); err != nil {
		return err
	}
	c.WorkloadIdentity.TTLSeconds = int64(workload.DefaultTTL / time.Second)
	if c.Orchestrator.Endpoint, err = required("ORCHESTRATOR_PLATFORM_ORIGIN"); err != nil {
		return err
	}
	if c.Orchestrator.InternalToken, err = required("ORCHESTRATOR_INTERNAL_TOKEN"); err != nil {
		return err
	}
	if c.Orchestrator.ManagementToken, err = required("ORCHESTRATOR_AUTH_ADMIN_TOKEN"); err != nil {
		return err
	}
	if c.Orchestrator.ContributionAckToken, err = required("ORCHESTRATOR_CONTRIBUTION_AUTH_ACK_TOKEN"); err != nil {
		return err
	}
	c.Jaeger.Endpoint = strings.TrimSpace(os.Getenv("JAEGER_ENDPOINT"))
	if file := strings.TrimSpace(os.Getenv("AUTH_ADMIN_BOOTSTRAP_SECRET_FILE")); file != "" {
		c.AdminBootstrap.SecretFile = file
	}
	for name, value := range map[string]string{
		"JWT_SECRET":                               c.Jwt.Secret,
		"AUTH_INTERNAL_TOKEN":                      c.InternalAuth.Token,
		"ORCHESTRATOR_AUTH_WORKLOAD_TOKEN":         c.WorkloadIdentity.ControlPlaneToken,
		"ORCHESTRATOR_INTERNAL_TOKEN":              c.Orchestrator.InternalToken,
		"ORCHESTRATOR_AUTH_ADMIN_TOKEN":            c.Orchestrator.ManagementToken,
		"ORCHESTRATOR_CONTRIBUTION_AUTH_ACK_TOKEN": c.Orchestrator.ContributionAckToken,
	} {
		if len(value) < 32 {
			return fmt.Errorf("platform bootstrap Auth requires %s to be at least 32 bytes", name)
		}
	}
	for name, value := range map[string]string{
		"JWT_SECRET":                               c.Jwt.Secret,
		"AUTH_INTERNAL_TOKEN":                      c.InternalAuth.Token,
		"ORCHESTRATOR_INTERNAL_TOKEN":              c.Orchestrator.InternalToken,
		"ORCHESTRATOR_CONTRIBUTION_AUTH_ACK_TOKEN": c.Orchestrator.ContributionAckToken,
		"ORCHESTRATOR_AUTH_WORKLOAD_TOKEN":         c.WorkloadIdentity.ControlPlaneToken,
	} {
		if c.Orchestrator.ManagementToken == value {
			return fmt.Errorf("platform bootstrap Auth requires ORCHESTRATOR_AUTH_ADMIN_TOKEN to be distinct from %s", name)
		}
	}
	return nil
}

func clearManagedRuntimeFields(c *config.Config) {
	c.Database.Url = ""
	c.Jwt.Secret = ""
	c.InternalAuth.Token = ""
	c.AdminBootstrap.Secret = ""
	c.AdminBootstrap.SecretFile = ""
	c.WorkloadIdentity.PrivateKeyFile = ""
	c.WorkloadIdentity.PrivateKeyPEM = ""
	c.WorkloadIdentity.ControlPlaneToken = ""
	c.Orchestrator.Endpoint = ""
	c.Orchestrator.InternalToken = ""
	c.Orchestrator.ManagementToken = ""
	c.Orchestrator.ContributionAckToken = ""
	c.Jaeger.Endpoint = ""
}

func applyDevelopmentEnvOverrides(c *config.Config) {
	if value := strings.TrimSpace(os.Getenv("JAEGER_ENDPOINT")); value != "" {
		c.Jaeger.Endpoint = value
	}
	if value := strings.TrimSpace(os.Getenv("JWT_SECRET")); value != "" {
		c.Jwt.Secret = value
	}
	if value := strings.TrimSpace(os.Getenv("AUTH_INTERNAL_TOKEN")); value != "" {
		c.InternalAuth.Token = value
	}
	if value := strings.TrimSpace(os.Getenv("AUTH_ADMIN_BOOTSTRAP_SECRET")); value != "" {
		c.AdminBootstrap.Secret = value
	}
	if value := strings.TrimSpace(os.Getenv("AUTH_ADMIN_BOOTSTRAP_SECRET_FILE")); value != "" {
		c.AdminBootstrap.SecretFile = value
	}
	if value := strings.TrimSpace(os.Getenv("OJOS_WORKLOAD_PRIVATE_KEY_FILE")); value != "" {
		c.WorkloadIdentity.PrivateKeyFile = value
	}
	if value := strings.TrimSpace(os.Getenv("OJOS_WORKLOAD_CONTROL_PLANE_TOKEN")); value != "" {
		c.WorkloadIdentity.ControlPlaneToken = value
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
	if value := strings.TrimSpace(os.Getenv("OJOS_WORKLOAD_TTL_SECONDS")); value != "" {
		ttl, err := strconv.ParseInt(value, 10, 64)
		if err != nil || ttl <= 0 || ttl > 3600 {
			log.Printf("ignoring invalid OJOS_WORKLOAD_TTL_SECONDS")
		} else {
			c.WorkloadIdentity.TTLSeconds = ttl
		}
	}
	if value := strings.TrimSpace(os.Getenv("ORCHESTRATOR_ENDPOINT")); value != "" {
		c.Orchestrator.Endpoint = value
	}
	if value := strings.TrimSpace(os.Getenv("ORCHESTRATOR_INTERNAL_TOKEN")); value != "" {
		c.Orchestrator.InternalToken = value
	}
	if value := strings.TrimSpace(os.Getenv("CONTRIBUTION_ACK_TOKEN")); value != "" {
		c.Orchestrator.ContributionAckToken = value
	}
}

func firstEnv(keys ...string) string {
	for _, key := range keys {
		if value := strings.TrimSpace(os.Getenv(key)); value != "" {
			return value
		}
	}
	return ""
}

func productionModeEnabled() bool {
	return managedEnvironment() || platformBootstrapEnvironment() ||
		strings.EqualFold(strings.TrimSpace(os.Getenv("OJOS_ENVIRONMENT")), "production")
}

func managedEnvironment() bool {
	value := strings.TrimSpace(os.Getenv("OJOS_MANAGED_WORKLOAD"))
	return value == "1" || strings.EqualFold(value, "true")

}

func platformBootstrapEnvironment() bool {
	value := strings.TrimSpace(os.Getenv("OJOS_PLATFORM_BOOTSTRAP"))
	return value == "1" || strings.EqualFold(value, "true")
}
