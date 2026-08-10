// Code scaffolded by goctl. Safe to edit.
// goctl 1.10.1

package middleware

import (
	"context"
	"encoding/json"
	"net/http"
	"strings"

	"ojos-auth-service/internal/token"
)

type contextKey string

const ClaimsContextKey contextKey = "auth_claims"
const TokenContextKey contextKey = "auth_token"

const delegatedPermissionCheckAPI = "auth.user.permission.check"
const delegatedPermissionCheckPermission = "auth.permission.check"

type ServiceRouteAuthorizer func(
	context.Context,
	string,
	string,
	string,
	string,
) (bool, error)

type AuthMiddleware struct {
	secret                 string
	internalToken          string
	serviceRouteAuthorizer ServiceRouteAuthorizer
	strictDelegatedRoute   bool
}

func NewAuthMiddleware(secret string, internalToken string, authorizers ...ServiceRouteAuthorizer) *AuthMiddleware {
	middleware := &AuthMiddleware{
		secret:        secret,
		internalToken: strings.TrimSpace(internalToken),
	}
	if len(authorizers) > 0 {
		middleware.serviceRouteAuthorizer = authorizers[0]
	}
	return middleware
}

// NewStrictWorkloadAuthMiddleware makes the formal v2 permission provider a
// workload-only route. In particular, the Auth internal/admin bearer cannot
// bypass the projected ApiBinding authorization check.
func NewStrictWorkloadAuthMiddleware(secret string, internalToken string, authorizer ServiceRouteAuthorizer) *AuthMiddleware {
	middleware := NewAuthMiddleware(secret, internalToken, authorizer)
	middleware.strictDelegatedRoute = true
	return middleware
}

func (m *AuthMiddleware) Handle(next http.HandlerFunc) http.HandlerFunc {
	return func(w http.ResponseWriter, r *http.Request) {
		authHeader := r.Header.Get("Authorization")

		if authHeader == "" {
			writeAuthError(w, 40101, "missing authorization header")
			return
		}

		if !strings.HasPrefix(authHeader, "Bearer ") {
			writeAuthError(w, 40102, "invalid authorization header")
			return
		}

		tokenString := strings.TrimSpace(strings.TrimPrefix(authHeader, "Bearer "))
		if tokenString == "" {
			writeAuthError(w, 40103, "empty token")
			return
		}

		if m.strictDelegatedRoute && isDelegatedPermissionCheck(r) {
			claims, ok := m.authorizeServiceRoute(r, tokenString)
			if !ok {
				writeAuthError(w, 40104, "invalid or expired token")
				return
			}
			ctx := context.WithValue(r.Context(), ClaimsContextKey, claims)
			ctx = context.WithValue(ctx, TokenContextKey, tokenString)
			next(w, r.WithContext(ctx))
			return
		}

		if m.internalToken != "" && tokenString == m.internalToken {
			claims := &token.Claims{
				UserID:   0,
				Username: "internal-service",
				Roles:    []string{"internal"},
			}
			ctx := context.WithValue(r.Context(), ClaimsContextKey, claims)
			ctx = context.WithValue(ctx, TokenContextKey, tokenString)
			next(w, r.WithContext(ctx))
			return
		}

		if claims, ok := m.authorizeServiceRoute(r, tokenString); ok {
			ctx := context.WithValue(r.Context(), ClaimsContextKey, claims)
			ctx = context.WithValue(ctx, TokenContextKey, tokenString)
			next(w, r.WithContext(ctx))
			return
		}

		claims, err := token.Parse(m.secret, tokenString)
		if err != nil {
			writeAuthError(w, 40104, "invalid or expired token")
			return
		}

		ctx := context.WithValue(r.Context(), ClaimsContextKey, claims)
		ctx = context.WithValue(ctx, TokenContextKey, tokenString)

		next(w, r.WithContext(ctx))
	}
}

func isDelegatedPermissionCheck(r *http.Request) bool {
	return r != nil && r.Method == http.MethodPost && r.URL.Path == "/auth/admin/permission-check"
}

func (m *AuthMiddleware) authorizeServiceRoute(r *http.Request, tokenString string) (*token.Claims, bool) {
	if r == nil || r.Method != http.MethodPost {
		return nil, false
	}
	callerService := strings.TrimSpace(r.Header.Get("X-OJOS-Caller-Service"))
	if callerService == "" {
		return nil, false
	}

	switch r.URL.Path {
	case "/auth/permission-check":
		// UserPermissionCheckLogic validates the credential, caller identity,
		// api_id and requested grant together. The middleware only carries the
		// opaque token into that check.
		return &token.Claims{
			UserID:   0,
			Username: "service:" + callerService,
			Roles:    []string{"service"},
		}, true
	case "/auth/admin/permission-check":
		if strings.TrimSpace(r.Header.Get("X-OJOS-Api-Id")) != delegatedPermissionCheckAPI ||
			m.serviceRouteAuthorizer == nil {
			return nil, false
		}
		allowed, err := m.serviceRouteAuthorizer(
			r.Context(),
			callerService,
			tokenString,
			delegatedPermissionCheckAPI,
			delegatedPermissionCheckPermission,
		)
		if err != nil || !allowed {
			return nil, false
		}
		return &token.Claims{
			UserID:   0,
			Username: "service:" + callerService,
			Roles:    []string{"internal"},
		}, true
	default:
		return nil, false
	}
}

func ClaimsFromContext(ctx context.Context) (*token.Claims, bool) {
	claims, ok := ctx.Value(ClaimsContextKey).(*token.Claims)
	return claims, ok
}

func TokenFromContext(ctx context.Context) (string, bool) {
	tokenString, ok := ctx.Value(TokenContextKey).(string)
	if !ok || strings.TrimSpace(tokenString) == "" {
		return "", false
	}
	return tokenString, true
}

func writeAuthError(w http.ResponseWriter, code int, msg string) {
	w.Header().Set("Content-Type", "application/json; charset=utf-8")
	w.WriteHeader(http.StatusUnauthorized)

	_ = json.NewEncoder(w).Encode(map[string]any{
		"code": code,
		"msg":  msg,
	})
}
