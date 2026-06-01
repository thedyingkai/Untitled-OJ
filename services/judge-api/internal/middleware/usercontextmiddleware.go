// Code scaffolded by goctl. Safe to edit.
// goctl 1.10.1

package middleware

import (
	"encoding/json"
	"errors"
	"net/http"

	"ojos-shared/security/authctx"
)

type UserContextMiddleware struct{}

func NewUserContextMiddleware() *UserContextMiddleware {
	return &UserContextMiddleware{}
}

func (m *UserContextMiddleware) Handle(next http.HandlerFunc) http.HandlerFunc {
	return func(w http.ResponseWriter, r *http.Request) {
		user, err := authctx.FromHeaders(r.Header)
		if err != nil {
			switch {
			case errors.Is(err, authctx.ErrInvalidUserID):
				writeJSONError(w, http.StatusUnauthorized, 40106, "invalid user context")
			default:
				writeJSONError(w, http.StatusUnauthorized, 40105, "unauthorized")
			}
			return
		}

		ctx := authctx.NewContext(r.Context(), user)
		next(w, r.WithContext(ctx))
	}
}

func writeJSONError(w http.ResponseWriter, httpStatus int, code int, msg string) {
	w.Header().Set("Content-Type", "application/json; charset=utf-8")
	w.WriteHeader(httpStatus)

	_ = json.NewEncoder(w).Encode(map[string]any{
		"code": code,
		"msg":  msg,
	})
}
