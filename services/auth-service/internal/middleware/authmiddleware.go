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

type AuthMiddleware struct {
	secret        string
	internalToken string
}

func NewAuthMiddleware(secret string, internalToken string) *AuthMiddleware {
	return &AuthMiddleware{
		secret:        secret,
		internalToken: strings.TrimSpace(internalToken),
	}
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
