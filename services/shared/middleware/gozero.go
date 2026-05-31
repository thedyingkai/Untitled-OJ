package middleware

import (
	"net/http"
	"time"

	"ojos-shared/logger"

	"go.opentelemetry.io/contrib/instrumentation/net/http/otelhttp"
	sdktrace "go.opentelemetry.io/otel/sdk/trace"
	"go.uber.org/zap"
)

func RecoveryMiddleware(log *zap.Logger) func(http.HandlerFunc) http.HandlerFunc {
	return func(next http.HandlerFunc) http.HandlerFunc {
		return Recovery(log, next)
	}
}

func LoggingMiddleware(log *zap.Logger, tp *sdktrace.TracerProvider) func(http.HandlerFunc) http.HandlerFunc {
	return func(next http.HandlerFunc) http.HandlerFunc {
		return func(w http.ResponseWriter, r *http.Request) {
			start := time.Now()

			handler := otelhttp.NewHandler(
				http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
					next(w, r)
				}),
				r.Method+" "+r.URL.Path,
				otelhttp.WithTracerProvider(tp),
			)

			handler.ServeHTTP(w, r)

			logger.WithTrace(r.Context(), log).Info(
				"http request",
				zap.String("method", r.Method),
				zap.String("path", r.URL.Path),
				zap.Duration("duration", time.Since(start)),
			)
		}
	}
}
