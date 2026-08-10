package logic

import (
	"crypto/ed25519"
	"crypto/rand"
	"net/http"
	"net/http/httptest"
	"testing"
	"time"

	"ojos-judge-api/internal/middleware"
	"ojos-shared/security/workload"
)

func TestValidateWorkerIdentityBindsWorkerIDToDeployment(t *testing.T) {
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
		CredentialGeneration: 1,
	}, time.Now())
	if err != nil {
		t.Fatal(err)
	}

	handler := middleware.NewWorkerAuthMiddleware("", verifier, false).Handle(func(w http.ResponseWriter, r *http.Request) {
		if err := validateWorkerIdentity(r.Context(), "deployment-worker-b"); err != nil {
			t.Errorf("matching deployment was rejected: %v", err)
		}
		if err := validateWorkerIdentity(r.Context(), "another-worker"); err == nil {
			t.Error("mismatched worker_id was accepted")
		}
		w.WriteHeader(http.StatusNoContent)
	})
	request := httptest.NewRequest(http.MethodPost, "/", nil)
	request.Header.Set("Authorization", "Bearer "+token)
	request.Header.Set("X-OJOS-Gateway-Proxy", "service-routing")
	request.Header.Set("X-OJOS-Caller-Service", "judge-worker")
	request.Header.Set("X-OJOS-Caller-Node-Id", "node-b")
	request.Header.Set("X-OJOS-Caller-Deployment-Id", "deployment-worker-b")
	request.Header.Set("X-OJOS-Binding-Id", "binding-1")
	recorder := httptest.NewRecorder()
	handler(recorder, request)
	if recorder.Code != http.StatusNoContent {
		t.Fatalf("unexpected status %d: %s", recorder.Code, recorder.Body.String())
	}
}
