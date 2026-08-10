package svc

import (
	"context"
	"crypto/ed25519"
	"crypto/rand"
	"encoding/json"
	"testing"
	"time"

	"ojos-auth-service/internal/config"
	atopology "ojos-auth-service/internal/topologyprojection"
	"ojos-shared/security/workload"
	shared "ojos-shared/topologyprojection"
)

func TestProductionWorkloadIdentityConfigurationFailsClosed(t *testing.T) {
	valid := config.WorkloadIdentityConfig{
		PrivateKeyFile:    "/run/secrets/workload-private.pem",
		ControlPlaneToken: "dedicated-token",
		TTLSeconds:        900,
	}
	if err := validateWorkloadIdentityConfig(valid, true); err != nil {
		t.Fatalf("valid production workload identity rejected: %v", err)
	}

	for name, invalid := range map[string]config.WorkloadIdentityConfig{
		"missing all":     {},
		"missing token":   {PrivateKeyFile: valid.PrivateKeyFile, TTLSeconds: 900},
		"missing key":     {ControlPlaneToken: valid.ControlPlaneToken, TTLSeconds: 900},
		"nonstandard ttl": {PrivateKeyFile: valid.PrivateKeyFile, ControlPlaneToken: valid.ControlPlaneToken, TTLSeconds: 600},
	} {
		t.Run(name, func(t *testing.T) {
			if err := validateWorkloadIdentityConfig(invalid, true); err == nil {
				t.Fatal("invalid production workload identity was accepted")
			}
		})
	}
}

func TestDevelopmentMayDisableWorkloadIdentityButNeverConfigureHalf(t *testing.T) {
	if err := validateWorkloadIdentityConfig(config.WorkloadIdentityConfig{}, false); err != nil {
		t.Fatalf("disabled development workload identity rejected: %v", err)
	}
	if err := validateWorkloadIdentityConfig(config.WorkloadIdentityConfig{PrivateKeyFile: "key.pem"}, false); err == nil {
		t.Fatal("development accepted a signing key without its dedicated issuer token")
	}
}

func TestProductionServiceRouteAuthorizerRequiresExactWorkloadGrant(t *testing.T) {
	_, privateKey, err := ed25519.GenerateKey(rand.Reader)
	if err != nil {
		t.Fatal(err)
	}
	issuer, err := workload.NewIssuer(privateKey, "workload-1", "issuer", "gateway", 15*time.Minute)
	if err != nil {
		t.Fatal(err)
	}
	projection := atopology.NewStore(nil)
	revision := "revision-1"
	hash := "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
	request := shared.Request{
		APIVersion: shared.APIVersion, Provider: "auth", Action: "apply",
		TopologyID: "primary", AttemptedRevisionID: revision, DesiredRevisionID: &revision,
		DesiredContentSHA256: &hash, OperationID: "operation-1",
		Spec: json.RawMessage(`{"topology_id":"primary","endpoints":[],"links":[]}`),
		Routes: []shared.BindingRoute{{
			BindingID: "binding-permission", RequirementName: "permission_check", ConsumerDeploymentID: "judge-a",
			ConsumerServiceID: "judge-api", ConsumerNodeID: "node-a",
			CredentialGeneration: 3, APIID: "auth.user.permission.check", ProviderDeploymentID: "auth-a",
			ProviderServiceID: "auth-service", ProviderNodeID: "node-a", ProviderEndpoint: "auth-a",
			UpstreamBase: "https://auth.internal", ProviderPath: "/auth/admin/permission-check",
			VirtualPath: "/internal/apis/auth.user.permission.check", AuthMode: "workload", ProviderAuthMode: "workload",
			Permission: "auth.permission.check", Methods: []string{"POST"}, TimeoutMS: 5000,
		}},
		Grants: []shared.BindingGrant{{
			BindingID: "binding-permission", RequirementName: "permission_check", ConsumerDeploymentID: "judge-a",
			ConsumerServiceID: "judge-api", ConsumerNodeID: "node-a", CredentialGeneration: 3,
			APIID: "auth.user.permission.check", Permission: "auth.permission.check",
		}},
	}
	if err := request.Validate("auth", "primary"); err != nil {
		t.Fatal(err)
	}
	if err := projection.Apply(context.Background(), request); err != nil {
		t.Fatal(err)
	}
	authorize := newServiceRouteAuthorizer(true, issuer.Verifier(), projection, func(context.Context, string, string, string, string) (bool, error) {
		return true, nil
	})
	issue := func(deployment, service, node string, generation uint64) string {
		t.Helper()
		token, _, err := issuer.Issue(workload.IssueRequest{
			DeploymentID: deployment, ServiceID: service, NodeID: node, CredentialGeneration: generation,
		}, time.Now())
		if err != nil {
			t.Fatal(err)
		}
		return token
	}
	tests := []struct {
		name, caller, token string
		want                bool
	}{
		{name: "exact", caller: "judge-api", token: issue("judge-a", "judge-api", "node-a", 3), want: true},
		{name: "wrong header service", caller: "problem-service", token: issue("judge-a", "judge-api", "node-a", 3)},
		{name: "wrong deployment", caller: "judge-api", token: issue("judge-other", "judge-api", "node-a", 3)},
		{name: "wrong token service", caller: "problem-service", token: issue("judge-a", "problem-service", "node-a", 3)},
		{name: "wrong node", caller: "judge-api", token: issue("judge-a", "judge-api", "node-b", 3)},
		{name: "old generation", caller: "judge-api", token: issue("judge-a", "judge-api", "node-a", 2)},
		{name: "legacy credential", caller: "judge-api", token: "legacy-service-token"},
	}
	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			allowed, err := authorize(t.Context(), test.caller, test.token, "auth.user.permission.check", "auth.permission.check")
			if err != nil || allowed != test.want {
				t.Fatalf("allowed=%v want=%v err=%v", allowed, test.want, err)
			}
		})
	}
	if err := projection.Delete(t.Context(), "primary"); err != nil {
		t.Fatal(err)
	}
	allowed, err := authorize(t.Context(), "judge-api", issue("judge-a", "judge-api", "node-a", 3), "auth.user.permission.check", "auth.permission.check")
	if err != nil || allowed {
		t.Fatalf("unlinked topology remained authorized: allowed=%v err=%v", allowed, err)
	}
}

func TestDevelopmentAuthorizerKeepsExplicitLegacyCompatibility(t *testing.T) {
	authorize := newServiceRouteAuthorizer(false, nil, nil, func(_ context.Context, service, token, apiID, permission string) (bool, error) {
		return service == "judge-api" && token == "legacy" && apiID == "auth.user.permission.check" && permission == "auth.permission.check", nil
	})
	allowed, err := authorize(t.Context(), "judge-api", "legacy", "auth.user.permission.check", "auth.permission.check")
	if err != nil || !allowed {
		t.Fatalf("explicit development compatibility failed: allowed=%v err=%v", allowed, err)
	}
}
