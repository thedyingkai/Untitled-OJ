package middleware

import (
	"net/http"
	"strings"

	"github.com/zeromicro/go-zero/rest"
)

func CORSMiddleware() rest.Middleware {
	return func(next http.HandlerFunc) http.HandlerFunc {
		return func(w http.ResponseWriter, r *http.Request) {
			origin := strings.TrimSpace(r.Header.Get("Origin"))
			if allowedCORSOrigin(origin) {
				header := w.Header()
				header.Set("Access-Control-Allow-Origin", origin)
				header.Set("Vary", "Origin")
				header.Set("Access-Control-Allow-Credentials", "true")
				header.Set("Access-Control-Allow-Headers", "Authorization, Content-Type, X-OJOS-Worker-Token")
				header.Set("Access-Control-Allow-Methods", "GET, POST, PUT, PATCH, DELETE, OPTIONS")
			}

			if r.Method == http.MethodOptions {
				w.WriteHeader(http.StatusNoContent)
				return
			}

			next(w, r)
		}
	}
}

func allowedCORSOrigin(origin string) bool {
	switch {
	case origin == "http://localhost:5173",
		origin == "http://127.0.0.1:5173",
		origin == "http://localhost:4173",
		origin == "http://127.0.0.1:4173":
		return true
	default:
		return false
	}
}
