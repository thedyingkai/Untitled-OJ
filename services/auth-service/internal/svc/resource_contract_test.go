package svc

import (
	"os"
	"path/filepath"
	"strings"
	"testing"

	"ojos-auth-service/internal/config"
)

func TestManagedEnvironmentUsesAgentAuthResourceAndMaterializedSecrets(t *testing.T) {
	resourcePath := filepath.Join(t.TempDir(), "dsn")
	if err := os.WriteFile(resourcePath, []byte(`{"dsn":"postgres://auth:secret@database:5432/auth?sslmode=require"}`), 0o600); err != nil {
		t.Fatal(err)
	}
	t.Setenv("OJOS_MANAGED_WORKLOAD", "true")
	t.Setenv("OJOS_RESOURCE_AUTH_OUTPUT_FILE", resourcePath)
	t.Setenv("DATABASE_URL", "postgres://legacy:ignored@legacy:5432/legacy")
	t.Setenv("JWT_SECRET", "legacy-jwt")
	t.Setenv("AUTH_INTERNAL_TOKEN", "legacy-management")
	t.Setenv("OJOS_SECRET_JWT_SECRET", strings.Repeat("j", 32))
	t.Setenv("OJOS_SECRET_MANAGEMENT_TOKEN", strings.Repeat("m", 32))
	t.Setenv("OJOS_SECRET_WORKLOAD_PRIVATEKEYPEM", "agent-pem")
	t.Setenv("OJOS_SECRET_WORKLOAD_CONTROLPLANETOKEN", strings.Repeat("w", 32))
	t.Setenv("OJOS_CONFIG_ORCHESTRATOR_ENDPOINT", "https://orchestrator.internal")
	t.Setenv("OJOS_SECRET_ORCHESTRATOR_INTERNALTOKEN", strings.Repeat("o", 32))
	t.Setenv("OJOS_SECRET_ORCHESTRATOR_CONTRIBUTIONACKTOKEN", strings.Repeat("a", 32))
	var value config.Config
	if err := applyEnvOverrides(&value); err != nil {
		t.Fatal(err)
	}
	if value.Database.Url != "postgres://auth:secret@database:5432/auth?sslmode=require" {
		t.Fatalf("managed DSN did not come from Agent resource output")
	}
	if value.Jwt.Secret != strings.Repeat("j", 32) || value.InternalAuth.Token != strings.Repeat("m", 32) {
		t.Fatal("managed credentials did not come from materialized secret variables")
	}
}

func TestManagedEnvironmentRejectsLegacyOrYamlCredentialFallback(t *testing.T) {
	resourcePath := filepath.Join(t.TempDir(), "dsn")
	if err := os.WriteFile(resourcePath, []byte(`{"dsn":"postgres://auth:secret@database:5432/auth"}`), 0o600); err != nil {
		t.Fatal(err)
	}
	t.Setenv("OJOS_MANAGED_WORKLOAD", "true")
	t.Setenv("OJOS_RESOURCE_AUTH_OUTPUT_FILE", resourcePath)
	t.Setenv("JWT_SECRET", strings.Repeat("j", 32))
	t.Setenv("AUTH_INTERNAL_TOKEN", strings.Repeat("m", 32))
	t.Setenv("OJOS_WORKLOAD_PRIVATE_KEY_FILE", "/legacy/private.pem")
	t.Setenv("OJOS_WORKLOAD_CONTROL_PLANE_TOKEN", strings.Repeat("w", 32))
	t.Setenv("ORCHESTRATOR_ENDPOINT", "https://legacy-orchestrator.invalid")
	t.Setenv("ORCHESTRATOR_INTERNAL_TOKEN", strings.Repeat("o", 32))
	t.Setenv("ORCHESTRATOR_AUTH_ADMIN_TOKEN", strings.Repeat("g", 32))
	value := config.Config{
		Jwt:          config.JwtConfig{Secret: strings.Repeat("y", 32)},
		InternalAuth: config.InternalAuthConfig{Token: strings.Repeat("n", 32)},
		WorkloadIdentity: config.WorkloadIdentityConfig{
			PrivateKeyFile:    "/yaml/private.pem",
			ControlPlaneToken: strings.Repeat("p", 32),
		},
		Orchestrator: config.OrchestratorConfig{
			Endpoint:      "https://yaml-orchestrator.invalid",
			InternalToken: strings.Repeat("q", 32),
		},
	}
	err := applyEnvOverrides(&value)
	if err == nil || !strings.Contains(err.Error(), "Agent-materialized") {
		t.Fatalf("managed Auth accepted legacy/YAML credential fallback: %v", err)
	}
	if value.Jwt.Secret != "" || value.InternalAuth.Token != "" || value.WorkloadIdentity.PrivateKeyFile != "" || value.Orchestrator.Endpoint != "" {
		t.Fatal("managed Auth retained a legacy or YAML-managed field")
	}
}

func TestManagedEnvironmentClearsYamlFieldsBeforeAgentMaterialization(t *testing.T) {
	value := config.Config{
		Database:     config.DatabaseConfig{Url: "postgres://yaml.invalid/auth"},
		Jwt:          config.JwtConfig{Secret: strings.Repeat("y", 32)},
		InternalAuth: config.InternalAuthConfig{Token: strings.Repeat("n", 32)},
		AdminBootstrap: config.AdminBootstrapConfig{
			Secret:     strings.Repeat("b", 32),
			SecretFile: "/yaml/bootstrap",
		},
		WorkloadIdentity: config.WorkloadIdentityConfig{
			PrivateKeyFile:    "/yaml/private.pem",
			PrivateKeyPEM:     "yaml-pem",
			ControlPlaneToken: strings.Repeat("p", 32),
		},
		Orchestrator: config.OrchestratorConfig{
			Endpoint:      "https://yaml-orchestrator.invalid",
			InternalToken: strings.Repeat("q", 32),
		},
		Jaeger: config.JaegerConfig{Endpoint: "https://yaml-tracing.invalid"},
	}
	clearManagedRuntimeFields(&value)
	if value.Database.Url != "" || value.Jwt.Secret != "" || value.InternalAuth.Token != "" || value.AdminBootstrap.Secret != "" || value.AdminBootstrap.SecretFile != "" || value.WorkloadIdentity.PrivateKeyFile != "" || value.WorkloadIdentity.PrivateKeyPEM != "" || value.WorkloadIdentity.ControlPlaneToken != "" || value.Orchestrator.Endpoint != "" || value.Orchestrator.InternalToken != "" || value.Jaeger.Endpoint != "" {
		t.Fatal("managed Auth retained a YAML-managed runtime field")
	}
}

func TestManagedEnvironmentFailsClosedWithoutAgentResourceOrSecrets(t *testing.T) {
	t.Setenv("OJOS_MANAGED_WORKLOAD", "1")
	t.Setenv("OJOS_RESOURCE_AUTH_OUTPUT_FILE", filepath.Join(t.TempDir(), "missing"))
	var value config.Config
	err := applyEnvOverrides(&value)
	if err == nil {
		t.Fatal("expected missing Agent resource output to fail closed")
	}
	if strings.Contains(err.Error(), "postgres://") {
		t.Fatalf("resource failure leaked credential material: %v", err)
	}
}

func TestUnmanagedEnvironmentRetainsExplicitDevelopmentAliases(t *testing.T) {
	t.Setenv("OJOS_MANAGED_WORKLOAD", "")
	t.Setenv("OJOS_ENVIRONMENT", "development")
	t.Setenv("DATABASE_URL", "postgres://local:secret@database:5432/auth")
	var value config.Config
	if err := applyEnvOverrides(&value); err != nil {
		t.Fatal(err)
	}
	if value.Database.Url != "postgres://local:secret@database:5432/auth" {
		t.Fatal("development database alias was not retained")
	}
}

func TestPlatformBootstrapUsesStrictProductionInputsWithoutAgentResource(t *testing.T) {
	t.Setenv("OJOS_ENVIRONMENT", "production")
	t.Setenv("OJOS_PLATFORM_BOOTSTRAP", "1")
	t.Setenv("OJOS_MANAGED_WORKLOAD", "")
	t.Setenv("OJOS_RESOURCE_AUTH_OUTPUT_FILE", filepath.Join(t.TempDir(), "must-not-be-read"))
	t.Setenv("AUTH_DATABASE_URL", "postgres://auth:secret@auth-db:5432/auth?sslmode=disable")
	t.Setenv("JWT_SECRET", strings.Repeat("j", 32))
	t.Setenv("AUTH_INTERNAL_TOKEN", strings.Repeat("m", 32))
	t.Setenv("OJOS_WORKLOAD_PRIVATE_KEY_FILE", "/run/secrets/workload-private.pem")
	t.Setenv("ORCHESTRATOR_AUTH_WORKLOAD_TOKEN", strings.Repeat("w", 32))
	t.Setenv("OJOS_WORKLOAD_KEY_ID", "workload-1")
	t.Setenv("OJOS_WORKLOAD_ISSUER", "ojos-auth/workload")
	t.Setenv("OJOS_WORKLOAD_AUDIENCE", "ojos-gateway")
	t.Setenv("ORCHESTRATOR_PLATFORM_ORIGIN", "https://orchestrator:8090")
	t.Setenv("ORCHESTRATOR_INTERNAL_TOKEN", strings.Repeat("o", 32))
	t.Setenv("ORCHESTRATOR_AUTH_ADMIN_TOKEN", strings.Repeat("g", 32))
	t.Setenv("ORCHESTRATOR_CONTRIBUTION_AUTH_ACK_TOKEN", strings.Repeat("a", 32))
	value := config.Config{Database: config.DatabaseConfig{Url: "postgres://yaml.invalid/ignored"}}
	if err := applyEnvOverrides(&value); err != nil {
		t.Fatal(err)
	}
	if managedEnvironment() || !platformBootstrapEnvironment() || !productionModeEnabled() {
		t.Fatal("platform bootstrap mode was not separated from Agent-managed mode")
	}
	if value.Database.Url != "postgres://auth:secret@auth-db:5432/auth?sslmode=disable" || value.WorkloadIdentity.PrivateKeyFile != "/run/secrets/workload-private.pem" {
		t.Fatalf("bootstrap Auth did not use strict production inputs: %+v", value)
	}
	if value.Orchestrator.ManagementToken != strings.Repeat("g", 32) || value.Orchestrator.ManagementToken == value.InternalAuth.Token {
		t.Fatal("bootstrap Auth did not separate topology provider and general internal credentials")
	}
}

func TestProductionAuthRejectsImplicitDevelopmentFallback(t *testing.T) {
	t.Setenv("OJOS_ENVIRONMENT", "production")
	t.Setenv("OJOS_PLATFORM_BOOTSTRAP", "")
	t.Setenv("OJOS_MANAGED_WORKLOAD", "")
	t.Setenv("DATABASE_URL", "postgres://legacy:secret@legacy:5432/auth")
	err := applyEnvOverrides(&config.Config{})
	if err == nil || !strings.Contains(err.Error(), "OJOS_PLATFORM_BOOTSTRAP") {
		t.Fatalf("production Auth accepted implicit development configuration: %v", err)
	}
}

func TestPlatformBootstrapRejectsAgentMaterializationAliases(t *testing.T) {
	for _, name := range []string{
		"AUTH_ADMIN_BOOTSTRAP_SECRET",
		"OJOS_SECRET_ADMINBOOTSTRAP_SECRET",
		"OJOS_SECRET_JWT_SECRET",
		"OJOS_SECRET_MANAGEMENT_TOKEN",
		"OJOS_SECRET_ORCHESTRATOR_INTERNALTOKEN",
	} {
		t.Run(name, func(t *testing.T) {
			t.Setenv("OJOS_ENVIRONMENT", "production")
			t.Setenv("OJOS_PLATFORM_BOOTSTRAP", "1")
			t.Setenv(name, strings.Repeat("x", 32))
			err := applyEnvOverrides(&config.Config{})
			if err == nil || !strings.Contains(err.Error(), name) {
				t.Fatalf("platform bootstrap accepted %s: %v", name, err)
			}
		})
	}
}
