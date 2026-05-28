package middleware

import (
	"net/http"
	"time"

	"ojos-shared/logger"

	"go.uber.org/zap"
)

func Logging(log *zap.Logger, next http.Handler) http.Handler {
	return http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		start := time.Now()

		next.ServeHTTP(w, r)

		logger.WithTrace(r.Context(), log).Info(
			"http request",
			zap.String("method", r.Method),
			zap.String("path", r.URL.Path),
			zap.Duration("duration", time.Since(start)),
		)
	})
}
