package svc

import (
	"os"
	"path/filepath"
	"strings"
	"testing"

	"ojos-auth-service/internal/config"
)

func TestResolveAdminBootstrapSecretIsDisabledByDefault(t *testing.T) {
	secret, enabled, err := resolveAdminBootstrapSecret(config.AdminBootstrapConfig{})
	if err != nil || enabled || secret != nil {
		t.Fatalf("unexpected disabled result: secret=%q enabled=%v err=%v", secret, enabled, err)
	}
}

func TestResolveAdminBootstrapSecretReadsDedicatedFile(t *testing.T) {
	path := filepath.Join(t.TempDir(), "initial-admin")
	want := strings.Repeat("a", minAdminBootstrapSecretBytes)
	if err := os.WriteFile(path, []byte(want+"\n"), 0o600); err != nil {
		t.Fatal(err)
	}
	secret, enabled, err := resolveAdminBootstrapSecret(config.AdminBootstrapConfig{SecretFile: path})
	if err != nil {
		t.Fatal(err)
	}
	if !enabled || string(secret) != want {
		t.Fatalf("unexpected file result: secret=%q enabled=%v", secret, enabled)
	}
}

func TestAdminBootstrapEnvironmentOverridesAreExplicit(t *testing.T) {
	inline := strings.Repeat("inline-bootstrap-", 3)
	t.Setenv("AUTH_ADMIN_BOOTSTRAP_SECRET", inline)
	t.Setenv("AUTH_ADMIN_BOOTSTRAP_SECRET_FILE", "")
	var cfg config.Config
	applyEnvOverrides(&cfg)
	secret, enabled, err := resolveAdminBootstrapSecret(cfg.AdminBootstrap)
	if err != nil {
		t.Fatal(err)
	}
	if !enabled || string(secret) != inline {
		t.Fatalf("unexpected environment override: enabled=%v secret=%q", enabled, secret)
	}
}

func TestResolveAdminBootstrapSecretFailsClosedOnAmbiguousOrWeakConfig(t *testing.T) {
	for name, cfg := range map[string]config.AdminBootstrapConfig{
		"both sources": {Secret: strings.Repeat("a", 32), SecretFile: "unused"},
		"weak inline":  {Secret: "too-short"},
		"missing file": {SecretFile: filepath.Join(t.TempDir(), "missing")},
	} {
		t.Run(name, func(t *testing.T) {
			if _, _, err := resolveAdminBootstrapSecret(cfg); err == nil {
				t.Fatal("expected fail-closed configuration error")
			}
		})
	}
}

func TestAdminBootstrapSecretCannotReuseRuntimeCredentials(t *testing.T) {
	secret := []byte(strings.Repeat("bootstrap-", 4))
	if err := validateAdminBootstrapSecretSeparation(secret, map[string]string{
		"JWT secret": string(secret),
	}); err == nil {
		t.Fatal("expected reused JWT/bootstrap secret to be rejected")
	}
	if err := validateAdminBootstrapSecretSeparation(secret, map[string]string{
		"JWT secret":                   strings.Repeat("jwt-", 10),
		"internal bearer":              strings.Repeat("internal-", 5),
		"workload control-plane token": strings.Repeat("workload-", 5),
	}); err != nil {
		t.Fatalf("distinct secrets rejected: %v", err)
	}
}
