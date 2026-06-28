package logic

import (
	"errors"
	"net/http"
	"net/http/httptest"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"

	"ojos-gateway/internal/config"
	"ojos-gateway/internal/kernel/serviceruntime"
	"ojos-gateway/internal/svc"
)

func TestComponentMapsErrorToHealthStatus(t *testing.T) {
	start := time.Now().Add(-5 * time.Millisecond)

	ok := component("gateway", start, nil)
	if ok.Status != "ok" {
		t.Fatalf("expected ok status, got %q", ok.Status)
	}
	if ok.Name != "gateway" {
		t.Fatalf("expected component name to be preserved, got %q", ok.Name)
	}
	if ok.Latency < 0 {
		t.Fatalf("expected non-negative latency, got %d", ok.Latency)
	}

	failed := component("redis", start, errors.New("connection refused"))
	if failed.Status != "error" {
		t.Fatalf("expected error status, got %q", failed.Status)
	}
	if failed.Message != "connection refused" {
		t.Fatalf("expected error message to be exposed, got %q", failed.Message)
	}
}

func TestCheckDirReadable(t *testing.T) {
	tempDir := t.TempDir()
	if err := checkDirReadable(tempDir, "artifact root"); err != nil {
		t.Fatalf("expected temp dir to be readable: %v", err)
	}

	if err := checkDirReadable("", "artifact root"); err == nil || !strings.Contains(err.Error(), "not configured") {
		t.Fatalf("expected not configured error, got %v", err)
	}

	missing := filepath.Join(tempDir, "missing")
	if err := checkDirReadable(missing, "artifact root"); err == nil {
		t.Fatalf("expected missing directory to fail")
	}

	filePath := filepath.Join(tempDir, "artifact.txt")
	if err := os.WriteFile(filePath, []byte("not a directory"), 0644); err != nil {
		t.Fatal(err)
	}
	if err := checkDirReadable(filePath, "artifact root"); err == nil || !strings.Contains(err.Error(), "not a directory") {
		t.Fatalf("expected not a directory error, got %v", err)
	}
}

func TestStatusFromBool(t *testing.T) {
	if got := statusFromBool(true); got != "ok" {
		t.Fatalf("expected ok, got %q", got)
	}
	if got := statusFromBool(false); got != "error" {
		t.Fatalf("expected error, got %q", got)
	}
}

func TestCheckHTTPUsesInternalHealthEndpoint(t *testing.T) {
	var requestedPath string
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		requestedPath = r.URL.Path
		if r.URL.Path != "/health" {
			http.NotFound(w, r)
			return
		}
		_, _ = w.Write([]byte(`{"status":"ok"}`))
	}))
	defer server.Close()

	logic := NewAdminHealthLogic(t.Context(), &svc.ServiceContext{})
	got := logic.checkHTTP(config.ProxyRouteConfig{
		Prefix:      "/api/judge",
		Target:      server.URL,
		StripPrefix: "/api",
	})

	if requestedPath != "/health" {
		t.Fatalf("expected gateway to probe internal /health, got %q", requestedPath)
	}
	if got.Name != "judge" {
		t.Fatalf("expected component name judge, got %q", got.Name)
	}
	if got.Status != "ok" {
		t.Fatalf("expected judge health ok, got status=%q message=%q", got.Status, got.Message)
	}
}

func TestCheckHTTPMarksJudgeHealth404AsError(t *testing.T) {
	server := httptest.NewServer(http.NotFoundHandler())
	defer server.Close()

	logic := NewAdminHealthLogic(t.Context(), &svc.ServiceContext{})
	got := logic.checkHTTP(config.ProxyRouteConfig{
		Prefix: "/api/judge",
		Target: server.URL,
	})

	if got.Name != "judge" {
		t.Fatalf("expected component name judge, got %q", got.Name)
	}
	if got.Status != "error" {
		t.Fatalf("expected judge health 404 to be error, got %q", got.Status)
	}
	if !strings.Contains(got.Message, "404") {
		t.Fatalf("expected 404 message, got %q", got.Message)
	}
}

func TestRuntimeHealthMessageMarksMetadataRegistration(t *testing.T) {
	got := runtimeHealthMessage(serviceruntime.RuntimeComponent{
		ServiceID:   "ojos.demo-service",
		ComponentID: "demo-health",
		Type:        "health_check",
		Status:      "DISABLED",
		Config:      []byte(`{"type":"metadata","optional":true}`),
	})
	if !strings.Contains(got, "metadata optional registered") {
		t.Fatalf("expected metadata registration message, got %q", got)
	}
}
