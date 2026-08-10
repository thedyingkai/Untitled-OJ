package middleware

import (
	"net/http"
	"time"

	"ojos-shared/logger"

	"github.com/zeromicro/go-zero/rest"
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

func LoggingMiddleware(log *zap.Logger, tp *sdktrace.TracerProvider) rest.Middleware {
	return func(next http.HandlerFunc) http.HandlerFunc {
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
			"http",
			otelhttp.WithTracerProvider(tp),
			otelhttp.WithSpanNameFormatter(func(operation string, r *http.Request) string {
				return r.Method + " " + r.URL.Path
			}),
		)

		return observed.ServeHTTP
	}
}

func RecoveryMiddleware(log *zap.Logger) rest.Middleware {
	return func(next http.HandlerFunc) http.HandlerFunc {
		return Recovery(log, next)
	}
}

func Recovery(log *zap.Logger, next http.HandlerFunc) http.HandlerFunc {
	return func(w http.ResponseWriter, r *http.Request) {
		defer func() {
			if err := recover(); err != nil {
				// ReverseProxy uses this sentinel after a response stream has begun.
				// Let net/http abort the connection instead of appending a JSON error
				// to an incomplete object body.
				if err == http.ErrAbortHandler {
					panic(err)
				}
				log.Error("panic recovered", zap.Any("error", err))

				w.Header().Set("Content-Type", "application/json; charset=utf-8")
				w.WriteHeader(http.StatusInternalServerError)
				_, _ = w.Write([]byte(`{"code":50001,"msg":"internal server error"}`))
			}
		}()

		next(w, r)
	}
}
