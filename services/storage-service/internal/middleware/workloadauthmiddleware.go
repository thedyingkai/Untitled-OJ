package middleware

import (
	"context"
	"encoding/json"
	"net/http"
	"strings"
	"time"

	"ojos-shared/security/workload"
)

type workloadClaimsContextKey struct{}

// WorkloadAuthMiddleware is the provider-side half of a typed ApiBinding. The
// Gateway validates the same short-lived Ed25519 token and replaces all caller
// headers; the provider verifies that signed claims and projection agree.
type WorkloadAuthMiddleware struct {
	enabled       bool
	verifier      *workload.Verifier
	expectedAPIID string
}

func NewWorkloadAuthMiddleware(enabled bool, verifier *workload.Verifier, expectedAPIID string) *WorkloadAuthMiddleware {
	return &WorkloadAuthMiddleware{enabled: enabled, verifier: verifier, expectedAPIID: strings.TrimSpace(expectedAPIID)}
}

func (m *WorkloadAuthMiddleware) Handle(next http.HandlerFunc) http.HandlerFunc {
	return func(w http.ResponseWriter, r *http.Request) {
		if m == nil || !m.enabled {
			next(w, r)
			return
		}
		claims, ok := m.verify(r)
		if !ok {
			writeWorkloadError(w)
			return
		}
		ctx := context.WithValue(r.Context(), workloadClaimsContextKey{}, claims)
		next(w, r.WithContext(ctx))
	}
}

func (m *WorkloadAuthMiddleware) verify(r *http.Request) (*workload.Claims, bool) {
	if m == nil || m.verifier == nil || r == nil {
		return nil, false
	}
	parts := strings.Fields(strings.TrimSpace(r.Header.Get("Authorization")))
	if len(parts) != 2 || !strings.EqualFold(parts[0], "Bearer") {
		return nil, false
	}
	claims, err := m.verifier.Verify(parts[1], time.Now())
	if err != nil {
		return nil, false
	}
	if r.Header.Get("X-OJOS-Gateway-Proxy") != "service-routing" ||
		strings.TrimSpace(r.Header.Get("X-OJOS-Caller-Service")) != claims.ServiceID ||
		strings.TrimSpace(r.Header.Get("X-OJOS-Caller-Node-Id")) != claims.NodeID ||
		strings.TrimSpace(r.Header.Get("X-OJOS-Caller-Deployment-Id")) != claims.DeploymentID ||
		strings.TrimSpace(r.Header.Get("X-OJOS-Binding-Id")) == "" ||
		!strings.EqualFold(strings.TrimSpace(r.Header.Get("X-OJOS-Api-Id")), m.expectedAPIID) {
		return nil, false
	}
	return claims, true
}

func WorkloadClaimsFromContext(ctx context.Context) (*workload.Claims, bool) {
	claims, ok := ctx.Value(workloadClaimsContextKey{}).(*workload.Claims)
	return claims, ok && claims != nil
}

func writeWorkloadError(w http.ResponseWriter) {
	w.Header().Set("Content-Type", "application/json; charset=utf-8")
	w.WriteHeader(http.StatusUnauthorized)
	_ = json.NewEncoder(w).Encode(map[string]any{
		"code":    "STORAGE_WORKLOAD_IDENTITY_REQUIRED",
		"message": "valid bound workload identity is required",
	})
}
