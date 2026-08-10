package middleware

import (
	"net/http"
	"net/http/httptest"
	"testing"

	"go.uber.org/zap"
)

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
