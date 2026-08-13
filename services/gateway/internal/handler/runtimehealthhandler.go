package handler

import (
	"encoding/json"
	"net/http"

	"ojos-gateway/internal/svc"
)

func livenessHandler() http.HandlerFunc {
	return func(w http.ResponseWriter, _ *http.Request) {
		writeRuntimeHealth(w, http.StatusOK, "ok")
	}
}

func readinessHandler(svcCtx *svc.ServiceContext) http.HandlerFunc {
	return func(w http.ResponseWriter, r *http.Request) {
		if err := svcCtx.Ready(r.Context()); err != nil {
			writeRuntimeHealth(w, http.StatusServiceUnavailable, "unavailable")
			return
		}
		writeRuntimeHealth(w, http.StatusOK, "ok")
	}
}

func writeRuntimeHealth(w http.ResponseWriter, status int, value string) {
	w.Header().Set("Content-Type", "application/json")
	w.WriteHeader(status)
	_ = json.NewEncoder(w).Encode(map[string]string{"status": value})
}
