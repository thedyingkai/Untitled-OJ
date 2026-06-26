package middleware

import (
	"crypto/subtle"
	"net/http"
	"strings"
)

type WorkerAuthMiddleware struct {
	token string
}

func NewWorkerAuthMiddleware(token string) *WorkerAuthMiddleware {
	return &WorkerAuthMiddleware{token: strings.TrimSpace(token)}
}

func (m *WorkerAuthMiddleware) Handle(next http.HandlerFunc) http.HandlerFunc {
	return func(w http.ResponseWriter, r *http.Request) {
		if m.token == "" {
			writeJSONError(w, http.StatusServiceUnavailable, 50310, "worker auth token is not configured")
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
