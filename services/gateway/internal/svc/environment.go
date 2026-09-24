// Runtime configuration normalization and validation.
package svc

import (
	"errors"
	"fmt"
	"net/url"
	"os"
	"strings"

	"ojos-gateway/internal/config"
	"ojos-shared/security/workload"
)

func applyEnvOverrides(c *config.Config) error {
	if c == nil {
		return errors.New("Gateway config is nil")
	}
	managed := managedEnvironment()
	bootstrap := platformBootstrapEnvironment()
	production := productionModeEnabled()
	if managed && bootstrap {
		return errors.New("Gateway cannot be both Agent-managed and a platform bootstrap service")
	}
	if managed {
		return applyManagedEnv(c)
	}
	if bootstrap {
		return applyPlatformBootstrapEnv(c)
	}
	if production {
		return errors.New("production Gateway requires OJOS_MANAGED_WORKLOAD=1 or OJOS_PLATFORM_BOOTSTRAP=1")
	}
	if value := strings.TrimSpace(os.Getenv("REDIS_URL")); value != "" {
		c.Redis.Url = value
	}
	if value := strings.TrimSpace(os.Getenv("JAEGER_ENDPOINT")); value != "" {
		c.Jaeger.Endpoint = value
	}
	if value := strings.TrimSpace(os.Getenv("JWT_SECRET")); value != "" {
		c.Jwt.Secret = value
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
	if value := strings.TrimSpace(os.Getenv("ORCHESTRATOR_NODE_ID")); value != "" {
		c.Orchestrator.NodeID = value
	}
	if value := strings.TrimSpace(os.Getenv("AUTH_SERVICE_ENDPOINT")); value != "" {
		c.AuthService.Endpoint = value
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
	if value := strings.TrimSpace(os.Getenv("OJOS_PROBLEMS_ROOT")); value != "" {
		c.Storage.ProblemsRoot = value
	}
	if value := strings.TrimSpace(os.Getenv("OJOS_SUBMISSIONS_ROOT")); value != "" {
		c.Storage.SubmissionsRoot = value
	}
	return nil
}

func applyPlatformBootstrapEnv(c *config.Config) error {
	if !strings.EqualFold(strings.TrimSpace(os.Getenv("OJOS_ENVIRONMENT")), "production") {
		return errors.New("platform bootstrap Gateway requires OJOS_ENVIRONMENT=production")
	}
	required := func(name string) (string, error) {
		value := strings.TrimSpace(os.Getenv(name))
		if value == "" {
			return "", fmt.Errorf("platform bootstrap Gateway requires %s", name)
		}
		return value, nil
	}
	var err error
	if c.Redis.Url, err = required("REDIS_URL"); err != nil {
		return err
	}
	if c.Jwt.Secret, err = required("JWT_SECRET"); err != nil {
		return err
	}
	if c.Orchestrator.Endpoint, err = required("ORCHESTRATOR_PLATFORM_ORIGIN"); err != nil {
		return err
	}
	if c.Orchestrator.InternalToken, err = required("ORCHESTRATOR_INTERNAL_TOKEN"); err != nil {
		return err
	}
	if c.Orchestrator.ManagementToken, err = required("ORCHESTRATOR_GATEWAY_ADMIN_TOKEN"); err != nil {
		return err
	}
	if c.Orchestrator.ContributionAckToken, err = required("ORCHESTRATOR_CONTRIBUTION_GATEWAY_ACK_TOKEN"); err != nil {
		return err
	}
	c.Orchestrator.NodeID = strings.TrimSpace(os.Getenv("ORCHESTRATOR_NODE_ID"))
	if c.AuthService.Endpoint, err = required("AUTH_SERVICE_ENDPOINT"); err != nil {
		return err
	}
	if c.WorkloadIdentity.PublicKeyFile, err = required("OJOS_WORKLOAD_PUBLIC_KEY_FILE"); err != nil {
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
	c.Jaeger.Endpoint = strings.TrimSpace(os.Getenv("JAEGER_ENDPOINT"))
	// Auth is the only reserved static platform upstream. Every business route,
	// trusted service and status entry comes from an active Contribution revision.
	c.Proxy = config.ProxyConfig{
		TrustedServices: []config.ProxyTrustedServiceConfig{{
			ServiceID: "auth-service", Target: c.AuthService.Endpoint,
			StripPrefix: "/api", HealthCheckID: "auth-service-health",
		}},
		Routes: []config.ProxyRouteConfig{{
			Prefix: "/api/auth", Target: c.AuthService.Endpoint,
			StripPrefix: "/api", AuthMode: "optional", TimeoutMS: 30000,
		}},
	}
	c.ServiceStatus = config.ServiceStatusConfig{ComposeServices: []string{"auth-service", "gateway"}}
	c.Storage = config.StorageConfig{}
	c.Database = config.DatabaseConfig{}
	c.InternalAuth = config.InternalAuthConfig{}
	for name, value := range map[string]string{
		"JWT_SECRET":                                  c.Jwt.Secret,
		"ORCHESTRATOR_INTERNAL_TOKEN":                 c.Orchestrator.InternalToken,
		"ORCHESTRATOR_GATEWAY_ADMIN_TOKEN":            c.Orchestrator.ManagementToken,
		"ORCHESTRATOR_CONTRIBUTION_GATEWAY_ACK_TOKEN": c.Orchestrator.ContributionAckToken,
	} {
		if len(value) < 32 {
			return fmt.Errorf("platform bootstrap Gateway requires %s to be at least 32 bytes", name)
		}
	}
	if c.Orchestrator.ManagementToken == c.Jwt.Secret ||
		c.Orchestrator.ManagementToken == c.Orchestrator.InternalToken ||
		c.Orchestrator.ManagementToken == c.Orchestrator.ContributionAckToken {
		return errors.New("platform bootstrap Gateway management token must be distinct from JWT and outbound Orchestrator credentials")
	}
	return nil
}

func applyManagedEnv(c *config.Config) error {
	for _, name := range []string{
		"REDIS_URL", "JAEGER_ENDPOINT", "JWT_SECRET", "ORCHESTRATOR_ENDPOINT",
		"ORCHESTRATOR_INTERNAL_TOKEN", "CONTRIBUTION_ACK_TOKEN", "ORCHESTRATOR_NODE_ID", "AUTH_SERVICE_ENDPOINT",
		"OJOS_PROBLEMS_ROOT", "OJOS_SUBMISSIONS_ROOT",
	} {
		if strings.TrimSpace(os.Getenv(name)) != "" {
			return fmt.Errorf("managed Gateway rejects legacy configuration variable %s", name)
		}
	}
	// The image YAML remains a Compose/development fallback only. Managed
	// workloads discard every legacy address, token and static business route
	// before consuming compiler-generated Agent materialization.
	c.Redis = config.RedisConfig{Url: strings.TrimSpace(os.Getenv("OJOS_SECRET_REDIS_URL"))}
	c.Jwt = config.JwtConfig{Secret: strings.TrimSpace(os.Getenv("OJOS_SECRET_JWT_SECRET"))}
	c.Jaeger = config.JaegerConfig{Endpoint: strings.TrimSpace(os.Getenv("OJOS_CONFIG_TRACING_ENDPOINT"))}
	c.Orchestrator = config.OrchestratorConfig{
		Endpoint:             strings.TrimSpace(os.Getenv("OJOS_CONFIG_ORCHESTRATOR_ENDPOINT")),
		InternalToken:        strings.TrimSpace(os.Getenv("OJOS_SECRET_ORCHESTRATOR_INTERNALTOKEN")),
		ContributionAckToken: strings.TrimSpace(os.Getenv("OJOS_SECRET_ORCHESTRATOR_CONTRIBUTIONACKTOKEN")),
		NodeID:               strings.TrimSpace(os.Getenv("OJOS_CONFIG_ORCHESTRATOR_NODEID")),
	}
	c.WorkloadIdentity = config.WorkloadIdentityConfig{
		PublicKeyFile: strings.TrimSpace(os.Getenv("OJOS_WORKLOAD_PUBLIC_KEY_FILE")),
		KeyID:         strings.TrimSpace(os.Getenv("OJOS_WORKLOAD_KEY_ID")),
		Issuer:        strings.TrimSpace(os.Getenv("OJOS_WORKLOAD_ISSUER")),
		Audience:      strings.TrimSpace(os.Getenv("OJOS_WORKLOAD_AUDIENCE")),
	}
	c.Proxy = config.ProxyConfig{}
	c.ServiceStatus = config.ServiceStatusConfig{}
	c.AuthService = config.AuthServiceConfig{}
	c.Storage = config.StorageConfig{}
	c.Database = config.DatabaseConfig{}
	c.InternalAuth = config.InternalAuthConfig{}
	for name, value := range map[string]string{
		"redis.url":                         c.Redis.Url,
		"jwt.secret":                        c.Jwt.Secret,
		"orchestrator.endpoint":             c.Orchestrator.Endpoint,
		"orchestrator.internalToken":        c.Orchestrator.InternalToken,
		"orchestrator.contributionAckToken": c.Orchestrator.ContributionAckToken,
		"orchestrator.nodeId":               c.Orchestrator.NodeID,
		"workload.publicKeyFile":            c.WorkloadIdentity.PublicKeyFile,
		"workload.keyId":                    c.WorkloadIdentity.KeyID,
		"workload.issuer":                   c.WorkloadIdentity.Issuer,
		"workload.audience":                 c.WorkloadIdentity.Audience,
	} {
		if strings.TrimSpace(value) == "" {
			return fmt.Errorf("managed Gateway requires Agent materialization for %s", name)
		}
	}
	if len(c.Jwt.Secret) < 32 || len(c.Orchestrator.InternalToken) < 32 || len(c.Orchestrator.ContributionAckToken) < 32 {
		return errors.New("managed Gateway JWT and Orchestrator secrets must be at least 32 bytes")
	}
	return nil
}

func managedEnvironment() bool {
	value := strings.TrimSpace(os.Getenv("OJOS_MANAGED_WORKLOAD"))
	return value == "1" || strings.EqualFold(value, "true")
}

func platformBootstrapEnvironment() bool {
	value := strings.TrimSpace(os.Getenv("OJOS_PLATFORM_BOOTSTRAP"))
	return value == "1" || strings.EqualFold(value, "true")
}

func productionModeEnabled() bool {
	return managedEnvironment() || platformBootstrapEnvironment() ||
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

func inferServiceID(target string) string {
	targetURL, err := url.Parse(strings.TrimSpace(target))
	if err != nil {
		return ""
	}
	return targetURL.Hostname()
}

func validateWorkloadIdentityConfig(c config.WorkloadIdentityConfig, production bool) error {
	if production && strings.TrimSpace(c.PublicKeyFile) == "" {
		return fmt.Errorf("production Gateway requires the workload identity public key")
	}
	return nil
}

func workloadIdentityVerifier(c config.WorkloadIdentityConfig) (*workload.Verifier, error) {
	return workload.NewVerifierFromPEMFile(c.PublicKeyFile, c.KeyID, c.Issuer, c.Audience)
}
