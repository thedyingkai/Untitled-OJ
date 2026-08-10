package middleware

import (
	"context"
	"crypto/subtle"
	"net/http"
	"strings"
	"time"

	"ojos-shared/security/workload"
)

type WorkerAuthMiddleware struct {
	token       string
	verifier    *workload.Verifier
	allowLegacy bool
}

type workloadClaimsContextKey struct{}

func NewWorkerAuthMiddleware(token string, options ...any) *WorkerAuthMiddleware {
	// Fail closed by default. Development callers that still exercise the old
	// shared-token protocol must opt in explicitly with a `true` option.
	middleware := &WorkerAuthMiddleware{token: strings.TrimSpace(token), allowLegacy: false}
	for _, option := range options {
		switch value := option.(type) {
		case *workload.Verifier:
			middleware.verifier = value
			middleware.allowLegacy = false
		case bool:
			middleware.allowLegacy = value
		}
	}
	return middleware
}

func (m *WorkerAuthMiddleware) Handle(next http.HandlerFunc) http.HandlerFunc {
	return func(w http.ResponseWriter, r *http.Request) {
		if claims, ok := m.verifyWorkloadRequest(r); ok {
			ctx := context.WithValue(r.Context(), workloadClaimsContextKey{}, claims)
			next(w, r.WithContext(ctx))
			return
		}
		if !m.allowLegacy || m.token == "" {
			writeJSONError(w, http.StatusUnauthorized, 40130, "valid workload identity is required")
			return
		}
		presented := workerTokenFromRequest(r)
		if presented == "" ||
			subtle.ConstantTimeCompare([]byte(presented), []byte(m.token)) != 1 {
			writeJSONError(w, http.StatusUnauthorized, 40130, "invalid worker token")
			return
		}

		next(w, r)
	}
}

func (m *WorkerAuthMiddleware) verifyWorkloadRequest(r *http.Request) (*workload.Claims, bool) {
	if m == nil || m.verifier == nil || r == nil {
		return nil, false
	}
	parts := strings.Fields(strings.TrimSpace(r.Header.Get("Authorization")))
	if len(parts) != 2 || !strings.EqualFold(parts[0], "Bearer") {
		return nil, false
	}
	claims, err := m.verifier.Verify(strings.TrimSpace(parts[1]), time.Now())
	if err != nil || claims.ServiceID != "judge-worker" {
		return nil, false
	}
	if r.Header.Get("X-OJOS-Gateway-Proxy") != "service-routing" ||
		strings.TrimSpace(r.Header.Get("X-OJOS-Caller-Service")) != claims.ServiceID ||
		strings.TrimSpace(r.Header.Get("X-OJOS-Caller-Node-Id")) != claims.NodeID ||
		strings.TrimSpace(r.Header.Get("X-OJOS-Caller-Deployment-Id")) != claims.DeploymentID ||
		strings.TrimSpace(r.Header.Get("X-OJOS-Binding-Id")) == "" {
		return nil, false
	}
	return claims, true
}

func WorkloadClaimsFromContext(ctx context.Context) (*workload.Claims, bool) {
	claims, ok := ctx.Value(workloadClaimsContextKey{}).(*workload.Claims)
	return claims, ok && claims != nil
}

func workerTokenFromRequest(r *http.Request) string {
	if token := strings.TrimSpace(r.Header.Get("X-OJOS-Worker-Token")); token != "" {
		return token
	}

	auth := strings.TrimSpace(r.Header.Get("Authorization"))
	if auth == "" {
		return ""
	}

	parts := strings.Fields(auth)
	if len(parts) == 2 && strings.EqualFold(parts[0], "Bearer") {
		return strings.TrimSpace(parts[1])
	}

	return ""
}
