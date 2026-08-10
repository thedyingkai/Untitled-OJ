package handler

import (
	"bytes"
	"crypto/ed25519"
	"crypto/rand"
	"encoding/json"
	"net/http"
	"net/http/httptest"
	"testing"
	"time"

	"ojos-auth-service/internal/config"
	"ojos-auth-service/internal/middleware"
	"ojos-auth-service/internal/svc"
	"ojos-shared/security/workload"
)

func TestWorkloadTokenIssueRequiresControlPlaneAndReturnsDeploymentJWT(t *testing.T) {
	publicKey, privateKey, err := ed25519.GenerateKey(rand.Reader)
	if err != nil {
		t.Fatal(err)
	}
	issuer, err := workload.NewIssuer(privateKey, "workload-key", "ojos-auth", "ojos-gateway", 15*time.Minute)
	if err != nil {
		t.Fatal(err)
	}
	verifier, err := workload.NewVerifier(publicKey, "workload-key", "ojos-auth", "ojos-gateway")
	if err != nil {
		t.Fatal(err)
	}
	serviceContext := &svc.ServiceContext{
		Config: config.Config{
			InternalAuth: config.InternalAuthConfig{Token: "admin-token"},
			WorkloadIdentity: config.WorkloadIdentityConfig{
				ControlPlaneToken: "workload-control-plane-token",
			},
		},
		WorkloadIssuer: issuer,
	}
	handler := middleware.NewAuthMiddleware("user-secret", "workload-control-plane-token").Handle(
		workloadTokenIssueHandler(serviceContext),
	)

	body := []byte(`{"deployment_id":"deployment-worker-b","service_id":"judge-worker","node_id":"node-b","credential_generation":3}`)
	request := httptest.NewRequest(http.MethodPost, "/auth/internal/workload-tokens:issue", bytes.NewReader(body))
	request.Header.Set("Authorization", "Bearer workload-control-plane-token")
	recorder := httptest.NewRecorder()
	handler(recorder, request)
	if recorder.Code != http.StatusOK {
		t.Fatalf("issue status %d: %s", recorder.Code, recorder.Body.String())
	}
	if recorder.Header().Get("Cache-Control") != "no-store" {
		t.Fatalf("workload credential response must not be cached")
	}
	var response map[string]any
	if err := json.NewDecoder(recorder.Body).Decode(&response); err != nil {
		t.Fatal(err)
	}
	for _, field := range []string{"access_token", "token_type", "expires_at", "expires_in"} {
		if _, ok := response[field]; !ok {
			t.Fatalf("workload credential response is missing %q: %#v", field, response)
		}
	}
	if len(response) != 4 {
		t.Fatalf("workload credential response must be an exact internal contract, got %#v", response)
	}
	if response["token_type"] != "Bearer" {
		t.Fatalf("unexpected workload token type: %#v", response["token_type"])
	}
	accessToken, ok := response["access_token"].(string)
	if !ok || accessToken == "" {
		t.Fatalf("invalid access_token: %#v", response["access_token"])
	}
	claims, err := verifier.Verify(accessToken, time.Now())
	if err != nil {
		t.Fatalf("issued token did not verify: %v", err)
	}
	if claims.DeploymentID != "deployment-worker-b" || claims.ServiceID != "judge-worker" || claims.NodeID != "node-b" || claims.CredentialGeneration != 3 {
		t.Fatalf("issued claims do not match assignment: %#v", claims)
	}
	expiresAt, ok := response["expires_at"].(string)
	if !ok {
		t.Fatalf("invalid expires_at: %#v", response["expires_at"])
	}
	if _, err := time.Parse(time.RFC3339Nano, expiresAt); err != nil {
		t.Fatalf("invalid expires_at: %v", err)
	}
	if expiresIn, ok := response["expires_in"].(float64); !ok || expiresIn != 15*60 {
		t.Fatalf("workload expires_in must remain seconds, got %#v", response["expires_in"])
	}

	request = httptest.NewRequest(http.MethodPost, "/auth/internal/workload-tokens:issue", bytes.NewReader(body))
	request.Header.Set("Authorization", "Bearer admin-token")
	recorder = httptest.NewRecorder()
	handler(recorder, request)
	if recorder.Code != http.StatusUnauthorized {
		t.Fatalf("Auth admin credential must not issue workload tokens, got %d", recorder.Code)
	}

	request = httptest.NewRequest(http.MethodPost, "/auth/internal/workload-tokens:issue", bytes.NewReader(body))
	request.Header.Set("Authorization", "Bearer wrong-token")
	recorder = httptest.NewRecorder()
	handler(recorder, request)
	if recorder.Code != http.StatusUnauthorized {
		t.Fatalf("non-control-plane credential must be rejected, got %d", recorder.Code)
	}
}
