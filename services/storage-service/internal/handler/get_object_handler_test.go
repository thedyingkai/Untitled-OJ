package handler

import (
	"context"
	"errors"
	"net/http"
	"net/http/httptest"
	"testing"
)

func TestObjectStreamErrorDoesNotAppendJSONAfterCommit(t *testing.T) {
	recorder := httptest.NewRecorder()
	stream := &objectResponseWriter{ResponseWriter: recorder}
	recorder.Header().Set("Content-Length", "100")
	if _, err := stream.Write([]byte("partial-object")); err != nil {
		t.Fatal(err)
	}

	handleObjectStreamError(context.Background(), recorder, stream, errors.New("backend stream ended early"))

	if got := recorder.Body.String(); got != "partial-object" {
		t.Fatalf("stream error appended a response body: %q", got)
	}
}

func TestObjectStreamErrorClearsObjectHeadersBeforeCommit(t *testing.T) {
	recorder := httptest.NewRecorder()
	stream := &objectResponseWriter{ResponseWriter: recorder}
	recorder.Header().Set("Content-Length", "100")
	recorder.Header().Set("Content-Type", "application/zip")
	recorder.Header().Set("X-OJOS-Object-Sha256", "digest")

	handleObjectStreamError(context.Background(), recorder, stream, errors.New("metadata failed"))

	if got := recorder.Header().Get("Content-Length"); got != "" {
		t.Fatalf("error response retained object Content-Length %q", got)
	}
	if recorder.Code == http.StatusOK {
		t.Fatal("pre-commit stream error returned 200")
	}
}
