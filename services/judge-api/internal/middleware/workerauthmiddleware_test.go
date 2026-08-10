package middleware

import (
	"crypto/ed25519"
	"crypto/rand"
	"net/http"
	"net/http/httptest"
	"testing"
	"time"

	"ojos-shared/security/workload"
)

func TestWorkerAuthMiddlewareAcceptsBoundGatewayWorkloadIdentity(t *testing.T) {
	publicKey, privateKey, err := ed25519.GenerateKey(rand.Reader)
	if err != nil {
		t.Fatal(err)
	}
	issuer, err := workload.NewIssuer(privateKey, "worker-key", "issuer", "gateway", time.Minute)
	if err != nil {
		t.Fatal(err)
	}
	verifier, err := workload.NewVerifier(publicKey, "worker-key", "issuer", "gateway")
	if err != nil {
		t.Fatal(err)
	}
	token, _, err := issuer.Issue(workload.IssueRequest{
		DeploymentID:         "deployment-worker-b",
		ServiceID:            "judge-worker",
		NodeID:               "node-b",
		CredentialGeneration: 3,
	}, time.Now())
	if err != nil {
		t.Fatal(err)
	}

	authenticated := false
	handler := NewWorkerAuthMiddleware("legacy-token", verifier, false).Handle(func(w http.ResponseWriter, r *http.Request) {
		claims, ok := WorkloadClaimsFromContext(r.Context())
		authenticated = ok && claims.DeploymentID == "deployment-worker-b" && claims.CredentialGeneration == 3
		w.WriteHeader(http.StatusNoContent)
	})

	request := httptest.NewRequest(http.MethodPost, "/judge/worker/register", nil)
	request.Header.Set("Authorization", "Bearer "+token)
	request.Header.Set("X-OJOS-Gateway-Proxy", "service-routing")
	request.Header.Set("X-OJOS-Caller-Service", "judge-worker")
	request.Header.Set("X-OJOS-Caller-Node-Id", "node-b")
	request.Header.Set("X-OJOS-Caller-Deployment-Id", "deployment-worker-b")
	request.Header.Set("X-OJOS-Binding-Id", "binding-worker-control")
	recorder := httptest.NewRecorder()
	handler(recorder, request)
	if recorder.Code != http.StatusNoContent || !authenticated {
		t.Fatalf("valid bound workload was rejected: status=%d body=%s", recorder.Code, recorder.Body.String())
	}

	request = httptest.NewRequest(http.MethodPost, "/judge/worker/register", nil)
	request.Header.Set("Authorization", "Bearer "+token)
	request.Header.Set("X-OJOS-Gateway-Proxy", "service-routing")
	request.Header.Set("X-OJOS-Caller-Service", "judge-worker")
	request.Header.Set("X-OJOS-Caller-Node-Id", "node-b")
	request.Header.Set("X-OJOS-Caller-Deployment-Id", "forged-deployment")
	request.Header.Set("X-OJOS-Binding-Id", "binding-worker-control")
	recorder = httptest.NewRecorder()
	handler(recorder, request)
	if recorder.Code != http.StatusUnauthorized {
		t.Fatalf("forged gateway identity must be rejected, got %d", recorder.Code)
	}

	request = httptest.NewRequest(http.MethodPost, "/judge/worker/register", nil)
	request.Header.Set("X-OJOS-Worker-Token", "legacy-token")
	recorder = httptest.NewRecorder()
	handler(recorder, request)
	if recorder.Code != http.StatusUnauthorized {
		t.Fatalf("legacy token must remain disabled in managed mode, got %d", recorder.Code)
	}
}

func TestWorkerAuthMiddlewareLegacyTokenRequiresExplicitDevelopmentOptIn(t *testing.T) {
	request := httptest.NewRequest(http.MethodPost, "/judge/worker/register", nil)
	request.Header.Set("X-OJOS-Worker-Token", "legacy-token")

	defaultRecorder := httptest.NewRecorder()
	NewWorkerAuthMiddleware("legacy-token").Handle(func(w http.ResponseWriter, _ *http.Request) {
		w.WriteHeader(http.StatusNoContent)
	})(defaultRecorder, request)
	if defaultRecorder.Code != http.StatusUnauthorized {
		t.Fatalf("legacy token must be disabled by default, got %d", defaultRecorder.Code)
	}

	developmentRecorder := httptest.NewRecorder()
	NewWorkerAuthMiddleware("legacy-token", true).Handle(func(w http.ResponseWriter, _ *http.Request) {
		w.WriteHeader(http.StatusNoContent)
	})(developmentRecorder, request)
	if developmentRecorder.Code != http.StatusNoContent {
		t.Fatalf("explicit development compatibility token was rejected: %d", developmentRecorder.Code)
	}
}
