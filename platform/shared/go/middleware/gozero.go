package middleware

import (
	"net/http"
	"strconv"
	"strings"
	"time"

	"ojos-shared/logger"

	"github.com/prometheus/client_golang/prometheus"
	"github.com/zeromicro/go-zero/rest"
	"go.opentelemetry.io/contrib/instrumentation/net/http/otelhttp"
	sdktrace "go.opentelemetry.io/otel/sdk/trace"
	"go.uber.org/zap"
)

var httpRequests = prometheus.NewCounterVec(
	prometheus.CounterOpts{
		Namespace: "ojos",
		Subsystem: "http",
		Name:      "requests_total",
		Help:      "Completed HTTP requests handled by OJOS Go services.",
	},
	[]string{"service", "method", "status"},
)

func init() {
	prometheus.MustRegister(httpRequests)
}

type statusWriter struct {
	http.ResponseWriter
	status int
}

func (w *statusWriter) Write(body []byte) (int, error) {
	if w.status == 0 {
		w.WriteHeader(http.StatusOK)
	}
	return w.ResponseWriter.Write(body)
}

func (w *statusWriter) WriteHeader(code int) {
	if w.status != 0 {
		return
	}
	w.status = code
	w.ResponseWriter.WriteHeader(code)
}

func LoggingMiddleware(log *zap.Logger, tp *sdktrace.TracerProvider) rest.Middleware {
	return ServiceLoggingMiddleware("unknown", log, tp)
}

// ServiceLoggingMiddleware records a bounded service/method/status metric in
// addition to the structured request log. Routes are intentionally omitted to
// prevent IDs in request paths from becoming unbounded Prometheus labels.
func ServiceLoggingMiddleware(service string, log *zap.Logger, tp *sdktrace.TracerProvider) rest.Middleware {
	service = strings.TrimSpace(service)
	if service == "" {
		service = "unknown"
	}
	if log == nil {
		log = zap.NewNop()
	}
	requestLog := log
	if tp == nil {
		tp = sdktrace.NewTracerProvider()
	}
	tracerProvider := tp
	return func(next http.HandlerFunc) http.HandlerFunc {
		observed := otelhttp.NewHandler(
			http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
				start := time.Now()

				sw := &statusWriter{
					ResponseWriter: w,
				}

				next(sw, r)
				if sw.status == 0 {
					sw.status = http.StatusOK
				}
				httpRequests.WithLabelValues(service, r.Method, strconv.Itoa(sw.status)).Inc()

				logger.WithTrace(r.Context(), requestLog).Info(
					"http request",
					zap.String("method", r.Method),
					zap.String("path", r.URL.Path),
					zap.Int("status", sw.status),
					zap.Duration("duration", time.Since(start)),
				)
			}),
			"http",
			otelhttp.WithTracerProvider(tracerProvider),
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
