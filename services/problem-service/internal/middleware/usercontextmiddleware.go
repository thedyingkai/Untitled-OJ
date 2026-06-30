// Code scaffolded by goctl. Safe to edit.
// goctl 1.10.1

package middleware

import (
	"net/http"
	"strconv"
	"strings"

	"ojos-shared/security/authctx"
)

type UserContextMiddleware struct {
}

func NewUserContextMiddleware() *UserContextMiddleware {
	return &UserContextMiddleware{}
}

func (m *UserContextMiddleware) Handle(next http.HandlerFunc) http.HandlerFunc {
	return func(w http.ResponseWriter, r *http.Request) {
		if r.Header.Get("X-Auth-Verified") != "true" {
			http.Error(w, "unauthorized", http.StatusUnauthorized)
			return
		}

		userIDText := strings.TrimSpace(r.Header.Get("X-User-Id"))
		if userIDText == "" {
			http.Error(w, "unauthorized", http.StatusUnauthorized)
			return
		}

		userID, err := strconv.ParseInt(userIDText, 10, 64)
		if err != nil || userID <= 0 {
			http.Error(w, "unauthorized", http.StatusUnauthorized)
			return
		}

		username := strings.TrimSpace(r.Header.Get("X-Username"))

		var roles []string
		for _, role := range strings.Split(r.Header.Get("X-Roles"), ",") {
			role = strings.TrimSpace(role)
			if role != "" {
				roles = append(roles, role)
			}
		}

		user := &authctx.UserContext{
			UserID:   userID,
			Username: username,
			Roles:    roles,
		}

		ctx := authctx.NewContext(r.Context(), user)
		next(w, r.WithContext(ctx))
	}
}
