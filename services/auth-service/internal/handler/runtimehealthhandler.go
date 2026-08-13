package handler

import (
	"context"
	"encoding/json"
	"net/http"
	"time"

	"ojos-auth-service/internal/svc"
)

func runtimeHealthHandler(_ *svc.ServiceContext) http.HandlerFunc {
	return func(w http.ResponseWriter, _ *http.Request) {
		writeRuntimeStatus(w, http.StatusOK)
	}
}

func runtimeReadyHandler(serviceContext *svc.ServiceContext) http.HandlerFunc {
	return func(w http.ResponseWriter, r *http.Request) {
		ctx, cancel := context.WithTimeout(r.Context(), 2*time.Second)
		defer cancel()
		if err := serviceContext.Ready(ctx); err != nil {
			w.Header().Set("Content-Type", "application/problem+json")
			w.WriteHeader(http.StatusServiceUnavailable)
			_ = json.NewEncoder(w).Encode(map[string]any{
				"type": "https://ojos.dev/problems/auth-not-ready", "title": "Auth Service Not Ready",
				"status": http.StatusServiceUnavailable, "detail": err.Error(),
			})
			return
		}
		writeRuntimeStatus(w, http.StatusOK)
	}
}

func writeRuntimeStatus(w http.ResponseWriter, status int) {
	w.Header().Set("Content-Type", "application/json")
	w.WriteHeader(status)
	_ = json.NewEncoder(w).Encode(map[string]string{"status": "ok", "service": "auth-service"})
}
