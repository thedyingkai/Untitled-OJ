package handler

import (
	"net/http"

	authmw "ojos-auth/internal/middleware"

	"ojos-shared/response"
)

func Profile() http.HandlerFunc {
	return func(w http.ResponseWriter, r *http.Request) {
		claims, ok := authmw.ClaimsFromContext(r.Context())
		if !ok || claims == nil {
			response.Error(w, 40105, "unauthorized")
			return
		}

		response.Success(w, map[string]any{
			"user_id":  claims.UserID,
			"username": claims.Username,
			"roles":    claims.Roles,
		})
	}
}
