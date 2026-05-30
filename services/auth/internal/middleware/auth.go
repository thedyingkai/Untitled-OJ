package middleware

import (
	"context"
	"net/http"
	"strings"

	"ojos-auth/internal/token"

	"ojos-shared/response"
)

type contextKey string

const claimsContextKey contextKey = "auth_claims"

func JWT(secret string, next http.HandlerFunc) http.HandlerFunc {
	return func(w http.ResponseWriter, r *http.Request) {
		authHeader := r.Header.Get("Authorization")

		if authHeader == "" {
			response.Error(w, 40101, "missing authorization header")
			return
		}

		if !strings.HasPrefix(authHeader, "Bearer ") {
			response.Error(w, 40102, "invalid authorization header")
			return
		}

		tokenString := strings.TrimSpace(strings.TrimPrefix(authHeader, "Bearer "))
		if tokenString == "" {
			response.Error(w, 40103, "empty token")
			return
		}

		claims, err := token.Parse(secret, tokenString)
		if err != nil {
			response.Error(w, 40104, "invalid or expired token")
			return
		}

		ctx := context.WithValue(r.Context(), claimsContextKey, claims)

		next(w, r.WithContext(ctx))
	}
}

func ClaimsFromContext(ctx context.Context) (*token.Claims, bool) {
	claims, ok := ctx.Value(claimsContextKey).(*token.Claims)
	return claims, ok
}
