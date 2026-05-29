package middleware

import (
	"net/http"
	"time"

	"ojos-shared/logger"

	"go.opentelemetry.io/contrib/instrumentation/net/http/otelhttp"
	sdktrace "go.opentelemetry.io/otel/sdk/trace"
	"go.uber.org/zap"
)

type statusWriter struct {
	http.ResponseWriter
	status int
}

func (w *statusWriter) WriteHeader(code int) {
	w.status = code
	w.ResponseWriter.WriteHeader(code)
}

func Logging(log *zap.Logger, tp *sdktrace.TracerProvider, next http.HandlerFunc) http.HandlerFunc {
	observed := otelhttp.NewHandler(
		http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
			start := time.Now()

			sw := &statusWriter{
				ResponseWriter: w,
				status:         http.StatusOK,
			}

			next(sw, r)

			logger.WithTrace(r.Context(), log).Info(
				"http request",
				zap.String("method", r.Method),
				zap.String("path", r.URL.Path),
				zap.Int("status", sw.status),
				zap.Duration("duration", time.Since(start)),
			)
		}),
		"gateway-http",
		otelhttp.WithTracerProvider(tp),
		otelhttp.WithSpanNameFormatter(func(operation string, r *http.Request) string {
			return r.Method + " " + r.URL.Path
		}),
	)

	return func(w http.ResponseWriter, r *http.Request) {
		observed.ServeHTTP(w, r)
	}
}
