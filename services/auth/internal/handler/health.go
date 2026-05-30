package handler

import (
	"net/http"

	"ojos-shared/events"
	"ojos-shared/response"
)

func Health(bus *events.Bus) http.HandlerFunc {
	return func(w http.ResponseWriter, r *http.Request) {
		if bus != nil {
			_ = bus.Publish(
				r.Context(),
				"auth.health.checked",
				"auth.health.checked",
				map[string]any{
					"path": r.URL.Path,
				},
			)
		}

		response.Success(w, map[string]any{
			"status":  "ok",
			"service": "auth",
		})
	}
}
