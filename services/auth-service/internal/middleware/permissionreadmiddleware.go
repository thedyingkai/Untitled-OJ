package middleware

import (
	"context"
	"crypto/subtle"
	"net/http"
	"strings"
)

type permissionReadContextKey struct{}

// PermissionReadMiddleware accepts the control-plane management credential only
// on the effective-permissions read route. It does not create an admin identity.
// Existing authenticated administrators continue through the ordinary middleware.
type PermissionReadMiddleware struct {
	managementToken string
	fallback        func(http.HandlerFunc) http.HandlerFunc
}

func NewPermissionReadMiddleware(managementToken string, fallback func(http.HandlerFunc) http.HandlerFunc) *PermissionReadMiddleware {
	return &PermissionReadMiddleware{
		managementToken: strings.TrimSpace(managementToken),
		fallback:        fallback,
	}
}

func (m *PermissionReadMiddleware) Handle(next http.HandlerFunc) http.HandlerFunc {
	return func(w http.ResponseWriter, r *http.Request) {
		authorization := r.Header.Get("Authorization")
		if r.Method == http.MethodGet && m.managementToken != "" && strings.HasPrefix(authorization, "Bearer ") {
			presented := strings.TrimSpace(strings.TrimPrefix(authorization, "Bearer "))
			if subtle.ConstantTimeCompare([]byte(presented), []byte(m.managementToken)) == 1 {
				ctx := context.WithValue(r.Context(), permissionReadContextKey{}, true)
				next(w, r.WithContext(ctx))
				return
			}
		}
		if m.fallback == nil {
			writeAuthError(w, 40104, "invalid or expired token")
			return
		}
		m.fallback(next)(w, r)
	}
}

// ControlPlanePermissionRead is consumed only by the read-only effective
// permission use case; it cannot satisfy general administrator authorization.
func ControlPlanePermissionRead(ctx context.Context) bool {
	allowed, _ := ctx.Value(permissionReadContextKey{}).(bool)
	return allowed
}
