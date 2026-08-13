package svc

import (
	"encoding/json"
	"os"
	"path/filepath"
	"strings"
	"testing"

	"ojos-gateway/internal/config"
	sharedperm "ojos-shared/security/permission"
	"ojos-shared/servicecontext"
)

func setManagedGatewayEnvironment(t *testing.T) {
	t.Helper()
	t.Setenv("OJOS_MANAGED_WORKLOAD", "true")
	t.Setenv("OJOS_SECRET_REDIS_URL", "redis://managed-redis:6379/0")
	t.Setenv("OJOS_SECRET_JWT_SECRET", strings.Repeat("j", 32))
	t.Setenv("OJOS_CONFIG_ORCHESTRATOR_ENDPOINT", "https://orchestrator.internal")
	t.Setenv("OJOS_SECRET_ORCHESTRATOR_INTERNALTOKEN", strings.Repeat("o", 32))
	t.Setenv("OJOS_SECRET_ORCHESTRATOR_CONTRIBUTIONACKTOKEN", strings.Repeat("a", 32))
	t.Setenv("OJOS_CONFIG_ORCHESTRATOR_NODEID", "node-a")
	t.Setenv("OJOS_CONFIG_TRACING_ENDPOINT", "tracing.internal:4317")
	t.Setenv("OJOS_WORKLOAD_PUBLIC_KEY_FILE", "/run/ojos/service/workload-public-key.pem")
	t.Setenv("OJOS_WORKLOAD_KEY_ID", "workload-1")
	t.Setenv("OJOS_WORKLOAD_ISSUER", "ojos-auth/workload")
	t.Setenv("OJOS_WORKLOAD_AUDIENCE", "ojos-gateway")
}

func TestManagedPermissionCheckerUsesHotServiceContextBinding(t *testing.T) {
	directory := t.TempDir()
	credential := filepath.Join(directory, "credential")
	contextFile := filepath.Join(directory, "context.json")
	if err := os.WriteFile(credential, []byte("generation-one\n"), 0o600); err != nil {
		t.Fatal(err)
	}
	snapshot := servicecontext.ServiceContext{
		SchemaVersion: 1,
		Deployment:    servicecontext.DeploymentIdentity{ID: "gateway-a", Service: "gateway", Node: "node-a"},
		Gateway:       servicecontext.GatewayContext{Origin: "http://127.0.0.1:8080"},
		Bindings: map[string]servicecontext.APIBinding{
			permissionBindingName: {
				BindingID: "binding-auth-permission", APIID: sharedperm.DefaultPermissionCheckApiID,
				BasePath: "/internal/apis/" + sharedperm.DefaultPermissionCheckApiID, TimeoutMS: 5000,
			},
		},
		CredentialFile: credential,
		Generation:     1,
	}
	payload, err := json.Marshal(snapshot)
	if err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(contextFile, payload, 0o600); err != nil {
		t.Fatal(err)
	}
	provider, err := servicecontext.NewContextProvider(contextFile, servicecontext.ProviderOptions{})
	if err != nil {
		t.Fatal(err)
	}
	defer provider.Close()
	checker, err := sharedperm.NewContextProviderUserChecker(provider, permissionBindingName)
	if err != nil || checker == nil {
		t.Fatalf("configure managed permission checker: %v", err)
	}
	current, err := provider.Current(t.Context())
	if err != nil || current.RequireService("gateway") != nil {
		t.Fatalf("managed Gateway context identity is invalid: %v", err)
	}
	request, err := current.NewRequest(t.Context(), permissionBindingName, "POST", "", strings.NewReader(`{}`))
	if err != nil || request.Header.Get("Authorization") != "Bearer generation-one" {
		t.Fatalf("first Agent credential was not consumed: header=%q err=%v", request.Header.Get("Authorization"), err)
	}
	if err := os.WriteFile(credential, []byte("generation-two\n"), 0o600); err != nil {
		t.Fatal(err)
	}
	request, err = current.NewRequest(t.Context(), permissionBindingName, "POST", "", strings.NewReader(`{}`))
	if err != nil || request.Header.Get("Authorization") != "Bearer generation-two" {
		t.Fatalf("rotated Agent credential was not reloaded: header=%q err=%v", request.Header.Get("Authorization"), err)
	}
}

func TestManagedGatewayDiscardsImageOwnedBusinessAddresses(t *testing.T) {
	setManagedGatewayEnvironment(t)
	c := config.Config{
		Redis: config.RedisConfig{Url: "redis://image-redis"},
		Jwt:   config.JwtConfig{Secret: "image-secret"},
		Proxy: config.ProxyConfig{
			Routes:          []config.ProxyRouteConfig{{Prefix: "/api/problem", Target: "http://problem-service:8083"}},
			TrustedServices: []config.ProxyTrustedServiceConfig{{ServiceID: "auth-service", Target: "http://auth-service:8081"}},
		},
		ServiceStatus: config.ServiceStatusConfig{ComposeServices: []string{"problem-service"}},
		AuthService:   config.AuthServiceConfig{Endpoint: "http://auth-service:8081"},
		Storage:       config.StorageConfig{ProblemsRoot: "/legacy/problems", SubmissionsRoot: "/legacy/submissions"},
	}
	if err := applyEnvOverrides(&c); err != nil {
		t.Fatal(err)
	}
	if len(c.Proxy.Routes) != 0 || len(c.Proxy.TrustedServices) != 0 || len(c.ServiceStatus.ComposeServices) != 0 {
		t.Fatalf("managed Gateway retained image-owned business routing: proxy=%+v status=%+v", c.Proxy, c.ServiceStatus)
	}
	if c.AuthService.Endpoint != "" || c.Storage.ProblemsRoot != "" || c.Storage.SubmissionsRoot != "" {
		t.Fatalf("managed Gateway retained legacy addresses: auth=%q storage=%+v", c.AuthService.Endpoint, c.Storage)
	}
	if c.Orchestrator.Endpoint != "https://orchestrator.internal" || c.WorkloadIdentity.PublicKeyFile != "/run/ojos/service/workload-public-key.pem" {
		t.Fatalf("platform bootstrap materialization was not retained: orchestrator=%+v workload=%+v", c.Orchestrator, c.WorkloadIdentity)
	}
}

func TestManagedGatewayRejectsLegacyEndpointOrTokenPollution(t *testing.T) {
	setManagedGatewayEnvironment(t)
	for _, variable := range []string{"REDIS_URL", "JWT_SECRET", "AUTH_SERVICE_ENDPOINT", "ORCHESTRATOR_INTERNAL_TOKEN"} {
		t.Run(variable, func(t *testing.T) {
			t.Setenv(variable, "legacy-value")
			if err := applyEnvOverrides(&config.Config{}); err == nil || !strings.Contains(err.Error(), variable) {
				t.Fatalf("managed Gateway accepted %s: %v", variable, err)
			}
		})
	}
}

func TestPlatformBootstrapGatewayRetainsOnlyReservedAuthRoute(t *testing.T) {
	t.Setenv("OJOS_ENVIRONMENT", "production")
	t.Setenv("OJOS_PLATFORM_BOOTSTRAP", "1")
	t.Setenv("OJOS_MANAGED_WORKLOAD", "")
	t.Setenv("REDIS_URL", "redis://:secret@redis:6379/0")
	t.Setenv("JWT_SECRET", strings.Repeat("j", 32))
	t.Setenv("ORCHESTRATOR_PLATFORM_ORIGIN", "https://orchestrator:8090")
	t.Setenv("ORCHESTRATOR_INTERNAL_TOKEN", strings.Repeat("o", 32))
	t.Setenv("ORCHESTRATOR_GATEWAY_ADMIN_TOKEN", strings.Repeat("g", 32))
	t.Setenv("ORCHESTRATOR_CONTRIBUTION_GATEWAY_ACK_TOKEN", strings.Repeat("a", 32))
	t.Setenv("AUTH_SERVICE_ENDPOINT", "http://auth-service:8081")
	t.Setenv("OJOS_WORKLOAD_PUBLIC_KEY_FILE", "/run/secrets/workload-public.pem")
	t.Setenv("OJOS_WORKLOAD_KEY_ID", "workload-1")
	t.Setenv("OJOS_WORKLOAD_ISSUER", "ojos-auth/workload")
	t.Setenv("OJOS_WORKLOAD_AUDIENCE", "ojos-gateway")
	c := config.Config{Proxy: config.ProxyConfig{
		Routes: []config.ProxyRouteConfig{
			{Prefix: "/api/problem", Target: "http://problem-service:8083"},
			{Prefix: "/api/judge", Target: "http://judge-api:8082"},
		},
		TrustedServices: []config.ProxyTrustedServiceConfig{{ServiceID: "user-service", Target: "http://user-service:8084"}},
	}}
	if err := applyEnvOverrides(&c); err != nil {
		t.Fatal(err)
	}
	if managedEnvironment() || !platformBootstrapEnvironment() || !productionModeEnabled() {
		t.Fatal("platform bootstrap mode was not separated from Agent-managed mode")
	}
	if len(c.Proxy.Routes) != 1 || c.Proxy.Routes[0].Prefix != "/api/auth" || len(c.Proxy.TrustedServices) != 1 || c.Proxy.TrustedServices[0].ServiceID != "auth-service" {
		t.Fatalf("bootstrap Gateway retained a static business route: %+v", c.Proxy)
	}
	if c.Orchestrator.ManagementToken != strings.Repeat("g", 32) || c.Orchestrator.ManagementToken == c.Orchestrator.InternalToken {
		t.Fatal("bootstrap Gateway did not separate inbound provider and outbound Orchestrator credentials")
	}
}

func TestProductionGatewayRejectsImplicitDevelopmentFallback(t *testing.T) {
	t.Setenv("OJOS_ENVIRONMENT", "production")
	t.Setenv("OJOS_PLATFORM_BOOTSTRAP", "")
	t.Setenv("OJOS_MANAGED_WORKLOAD", "")
	t.Setenv("REDIS_URL", "redis://legacy:6379/0")
	err := applyEnvOverrides(&config.Config{})
	if err == nil || !strings.Contains(err.Error(), "OJOS_PLATFORM_BOOTSTRAP") {
		t.Fatalf("production Gateway accepted implicit development configuration: %v", err)
	}
}
