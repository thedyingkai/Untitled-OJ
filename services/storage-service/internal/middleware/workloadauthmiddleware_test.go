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

func TestWorkloadAuthAcceptsOnlyMatchingGatewayProjectionAndAPI(t *testing.T) {
	publicKey, privateKey, err := ed25519.GenerateKey(rand.Reader)
	if err != nil {
		t.Fatal(err)
	}
	issuer, err := workload.NewIssuer(privateKey, "workload-1", "issuer", "gateway", time.Minute)
	if err != nil {
		t.Fatal(err)
	}
	verifier, err := workload.NewVerifier(publicKey, "workload-1", "issuer", "gateway")
	if err != nil {
		t.Fatal(err)
	}
	token, _, err := issuer.Issue(workload.IssueRequest{
		DeploymentID: "deployment-problem", ServiceID: "problem-service", NodeID: "node-a", CredentialGeneration: 3,
	}, time.Now())
	if err != nil {
		t.Fatal(err)
	}

	handler := NewWorkloadAuthMiddleware(true, verifier, "storage.object.put").Handle(func(w http.ResponseWriter, r *http.Request) {
		claims, ok := WorkloadClaimsFromContext(r.Context())
		if !ok || claims.CredentialGeneration != 3 {
			t.Fatal("verified claims were not placed in context")
		}
		w.WriteHeader(http.StatusNoContent)
	})
	request := httptest.NewRequest(http.MethodPut, "/problems/object", nil)
	setGatewayWorkloadHeaders(request, token)
	recorder := httptest.NewRecorder()
	handler(recorder, request)
	if recorder.Code != http.StatusNoContent {
		t.Fatalf("valid bound workload rejected: %d %s", recorder.Code, recorder.Body.String())
	}

	for name, mutate := range map[string]func(*http.Request){
		"caller":     func(request *http.Request) { request.Header.Set("X-OJOS-Caller-Service", "judge-api") },
		"node":       func(request *http.Request) { request.Header.Set("X-OJOS-Caller-Node-Id", "node-b") },
		"deployment": func(request *http.Request) { request.Header.Set("X-OJOS-Caller-Deployment-Id", "forged") },
		"binding":    func(request *http.Request) { request.Header.Del("X-OJOS-Binding-Id") },
		"api":        func(request *http.Request) { request.Header.Set("X-OJOS-Api-Id", "storage.object.delete") },
		"gateway":    func(request *http.Request) { request.Header.Del("X-OJOS-Gateway-Proxy") },
	} {
		t.Run(name, func(t *testing.T) {
			request := httptest.NewRequest(http.MethodPut, "/problems/object", nil)
			setGatewayWorkloadHeaders(request, token)
			mutate(request)
			recorder := httptest.NewRecorder()
			handler(recorder, request)
			if recorder.Code != http.StatusUnauthorized {
				t.Fatalf("forged projection returned %d", recorder.Code)
			}
		})
	}
}

func TestWorkloadAuthFailsClosedWithoutVerifierAndMayBeDisabledForDevelopment(t *testing.T) {
	request := httptest.NewRequest(http.MethodGet, "/problems/object", nil)
	closed := httptest.NewRecorder()
	NewWorkloadAuthMiddleware(true, nil, "storage.object.get").Handle(func(http.ResponseWriter, *http.Request) {
		t.Fatal("request reached protected handler")
	})(closed, request)
	if closed.Code != http.StatusUnauthorized {
		t.Fatalf("missing verifier returned %d", closed.Code)
	}

	development := httptest.NewRecorder()
	NewWorkloadAuthMiddleware(false, nil, "storage.object.get").Handle(func(w http.ResponseWriter, _ *http.Request) {
		w.WriteHeader(http.StatusNoContent)
	})(development, request)
	if development.Code != http.StatusNoContent {
		t.Fatalf("development mode returned %d", development.Code)
	}
}

func setGatewayWorkloadHeaders(request *http.Request, token string) {
	request.Header.Set("Authorization", "Bearer "+token)
	request.Header.Set("X-OJOS-Gateway-Proxy", "service-routing")
	request.Header.Set("X-OJOS-Caller-Service", "problem-service")
	request.Header.Set("X-OJOS-Caller-Node-Id", "node-a")
	request.Header.Set("X-OJOS-Caller-Deployment-Id", "deployment-problem")
	request.Header.Set("X-OJOS-Binding-Id", "binding-storage-put")
	request.Header.Set("X-OJOS-Api-Id", "storage.object.put")
}
