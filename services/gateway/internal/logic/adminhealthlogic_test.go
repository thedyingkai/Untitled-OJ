package logic

import (
	"errors"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"
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
