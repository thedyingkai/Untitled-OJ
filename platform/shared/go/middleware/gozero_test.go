package middleware

import (
	"io"
	"net/http"
	"net/http/httptest"
	"strings"
	"testing"

	"github.com/prometheus/client_golang/prometheus/promhttp"
	"github.com/prometheus/client_golang/prometheus/testutil"
	sdktrace "go.opentelemetry.io/otel/sdk/trace"
	"go.uber.org/zap"
)

func TestServiceLoggingMiddlewareRecordsBoundedRequestMetric(t *testing.T) {
	before := testutil.ToFloat64(httpRequests.WithLabelValues("test-service", http.MethodGet, "503"))
	handler := ServiceLoggingMiddleware("test-service", zap.NewNop(), sdktrace.NewTracerProvider())(
		func(w http.ResponseWriter, _ *http.Request) { w.WriteHeader(http.StatusServiceUnavailable) },
	)
	handler(httptest.NewRecorder(), httptest.NewRequest(http.MethodGet, "/users/secret-id", nil))
	after := testutil.ToFloat64(httpRequests.WithLabelValues("test-service", http.MethodGet, "503"))
	if after != before+1 {
		t.Fatalf("request metric delta = %v, want 1", after-before)
	}
	metrics := httptest.NewRecorder()
	promhttp.Handler().ServeHTTP(metrics, httptest.NewRequest(http.MethodGet, "/metrics", nil))
	body, _ := io.ReadAll(metrics.Result().Body)
	if string(body) == "" || !strings.Contains(string(body), `ojos_http_requests_total{method="GET",service="test-service",status="503"}`) {
		t.Fatalf("expected bounded request series, got:\n%s", body)
	}
	if strings.Contains(string(body), "secret-id") {
		t.Fatal("request path leaked into Prometheus labels")
	}
}

func TestServiceLoggingMiddlewareRecordsImplicitAndFirstStatus(t *testing.T) {
	cases := []struct {
		name   string
		serve  http.HandlerFunc
		status string
	}{
		{
			name: "implicit write",
			serve: func(w http.ResponseWriter, _ *http.Request) {
				_, _ = w.Write([]byte("ok"))
			},
			status: "200",
		},
		{
			name: "first header wins",
			serve: func(w http.ResponseWriter, _ *http.Request) {
				w.WriteHeader(http.StatusCreated)
				w.WriteHeader(http.StatusInternalServerError)
			},
			status: "201",
		},
	}
	for _, test := range cases {
		t.Run(test.name, func(t *testing.T) {
			before := testutil.ToFloat64(httpRequests.WithLabelValues("status-test", http.MethodGet, test.status))
			handler := ServiceLoggingMiddleware("status-test", zap.NewNop(), sdktrace.NewTracerProvider())(test.serve)
			handler(httptest.NewRecorder(), httptest.NewRequest(http.MethodGet, "/", nil))
			after := testutil.ToFloat64(httpRequests.WithLabelValues("status-test", http.MethodGet, test.status))
			if after != before+1 {
				t.Fatalf("request metric delta = %v, want 1", after-before)
			}
		})
	}
}

func TestRecoveryPropagatesAbortHandler(t *testing.T) {
	handler := Recovery(zap.NewNop(), func(http.ResponseWriter, *http.Request) {
		panic(http.ErrAbortHandler)
	})
	defer func() {
		if recovered := recover(); recovered != http.ErrAbortHandler {
			t.Fatalf("recovered panic = %#v, want http.ErrAbortHandler", recovered)
		}
	}()
	handler(httptest.NewRecorder(), httptest.NewRequest(http.MethodGet, "/stream", nil))
}
