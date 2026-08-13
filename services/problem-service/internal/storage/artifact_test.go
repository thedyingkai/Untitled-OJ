package storage

import (
	"context"
	"encoding/json"
	"fmt"
	"io"
	"net/http"
	"net/http/httptest"
	"os"
	"path/filepath"
	"runtime"
	"strings"
	"testing"
	"time"

	"ojos-problem-events/problemv1"
	"ojos-problem-service/internal/config"
)

type recordingIntentRegistrar struct {
	artifacts []problemv1.ArtifactRef
	completed []problemv1.ArtifactRef
}

func (r *recordingIntentRegistrar) MarkArtifactUploadCompleted(_ context.Context, artifact problemv1.ArtifactRef) error {
	r.completed = append(r.completed, artifact)
	return nil
}

func (r *recordingIntentRegistrar) RegisterArtifactUploadIntent(_ context.Context, artifact problemv1.ArtifactRef) error {
	r.artifacts = append(r.artifacts, artifact)
	return nil
}

func TestBuildDeterministicPackageArtifactIgnoresSourceMTime(t *testing.T) {
	problemsRoot := t.TempDir()
	root := filepath.Join(problemsRoot, "problem-1")
	if err := os.MkdirAll(filepath.Join(root, "tests"), 0o755); err != nil {
		t.Fatal(err)
	}
	manifest := filepath.Join(root, "problem.yaml")
	input := filepath.Join(root, "tests", "001.in")
	if err := os.WriteFile(manifest, []byte("format: ojos\n"), 0o644); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(input, []byte("1 1\n"), 0o644); err != nil {
		t.Fatal(err)
	}

	firstPath, firstDigest, firstSize, err := BuildDeterministicPackageArtifact(problemsRoot, root)
	if err != nil {
		t.Fatal(err)
	}
	defer os.Remove(firstPath)
	if filepath.Dir(firstPath) != filepath.Join(problemsRoot, artifactBuildDirectory) {
		t.Fatalf("artifact temporary escaped problems volume: %s", firstPath)
	}
	if info, err := os.Stat(firstPath); err != nil || runtime.GOOS != "windows" && info.Mode().Perm() != 0o600 {
		t.Fatalf("artifact temporary must be a private regular file: info=%v err=%v", info, err)
	}

	future := time.Now().Add(24 * time.Hour)
	if err := os.Chtimes(manifest, future, future); err != nil {
		t.Fatal(err)
	}
	if err := os.Chtimes(input, future, future); err != nil {
		t.Fatal(err)
	}
	secondPath, secondDigest, secondSize, err := BuildDeterministicPackageArtifact(problemsRoot, root)
	if err != nil {
		t.Fatal(err)
	}
	defer os.Remove(secondPath)
	if firstDigest != secondDigest || firstSize != secondSize {
		t.Fatalf("deterministic artifact changed: %s/%d != %s/%d", firstDigest, firstSize, secondDigest, secondSize)
	}
}

func TestBuildDeterministicPackageArtifactRejectsPackageOutsideProblemsRoot(t *testing.T) {
	problemsRoot := t.TempDir()
	packageRoot := t.TempDir()
	if err := os.WriteFile(filepath.Join(packageRoot, "problem.yaml"), []byte("format: ojos\n"), 0o644); err != nil {
		t.Fatal(err)
	}
	if path, _, _, err := BuildDeterministicPackageArtifact(problemsRoot, packageRoot); err == nil || path != "" {
		t.Fatalf("package outside managed volume was accepted: path=%q err=%v", path, err)
	}
}

func TestBuildDeterministicPackageArtifactRejectsSymlinkEntries(t *testing.T) {
	if runtime.GOOS == "windows" {
		t.Skip("creating symbolic links requires elevated privileges on some Windows hosts")
	}
	problemsRoot := t.TempDir()
	packageRoot := filepath.Join(problemsRoot, "problem-1")
	if err := os.MkdirAll(packageRoot, 0o755); err != nil {
		t.Fatal(err)
	}
	external := filepath.Join(t.TempDir(), "secret.txt")
	if err := os.WriteFile(external, []byte("must not be packaged"), 0o600); err != nil {
		t.Fatal(err)
	}
	if err := os.Symlink(external, filepath.Join(packageRoot, "linked-secret.txt")); err != nil {
		t.Fatal(err)
	}
	if path, _, _, err := BuildDeterministicPackageArtifact(problemsRoot, packageRoot); err == nil || path != "" {
		t.Fatalf("symbolic link entry was accepted: path=%q err=%v", path, err)
	}
}

func TestPublishPackageArtifactUsesDigestAddressedStorageObject(t *testing.T) {
	root := t.TempDir()
	if err := os.WriteFile(filepath.Join(root, "problem.yaml"), []byte("format: ojos\n"), 0o644); err != nil {
		t.Fatal(err)
	}
	var storedPath string
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if r.Method != http.MethodPut {
			t.Errorf("unexpected method %s", r.Method)
			w.WriteHeader(http.StatusMethodNotAllowed)
			return
		}
		if r.Header.Get("If-None-Match") != "*" {
			t.Errorf("missing immutable create precondition")
		}
		if r.ContentLength <= 0 {
			t.Errorf("missing content length: %d", r.ContentLength)
		}
		storedPath = r.URL.Path
		data, err := io.ReadAll(r.Body)
		if err != nil {
			t.Error(err)
			w.WriteHeader(http.StatusInternalServerError)
			return
		}
		if got := r.Header.Get("X-OJOS-Content-Sha256"); got != sha256Hex(data) {
			t.Errorf("content digest header mismatch: got %s want %s", got, sha256Hex(data))
		}
		_ = json.NewEncoder(w).Encode(map[string]any{
			"sha256":     sha256Hex(data),
			"size_bytes": len(data),
		})
	}))
	defer server.Close()
	intents := &recordingIntentRegistrar{}
	artifact, err := PublishPackageArtifactTracked(t.Context(), config.StorageConfig{
		ProblemsRoot:    root,
		ServiceEndpoint: server.URL,
		Bucket:          "problems",
	}, 7, root, intents)
	if err != nil {
		t.Fatal(err)
	}
	if !strings.Contains(storedPath, "package-sha256-"+artifact.SHA256+".zip") {
		t.Fatalf("artifact was not stored by digest: path=%s artifact=%#v", storedPath, artifact)
	}
	if artifact.URI != "storage://problems/package-sha256-"+artifact.SHA256+".zip" {
		t.Fatalf("unexpected artifact URI: %s", artifact.URI)
	}
	if len(intents.artifacts) != 1 || intents.artifacts[0] != artifact {
		t.Fatalf("upload intent was not durably requested before publication: %#v", intents.artifacts)
	}
}

func TestManagedProblemStorageUsesNamedBindingsAndWorkloadToken(t *testing.T) {
	root := t.TempDir()
	if err := os.WriteFile(filepath.Join(root, "problem.yaml"), []byte("format: ojos\n"), 0o644); err != nil {
		t.Fatal(err)
	}
	var storedPath string
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		storedPath = r.URL.Path
		if r.Header.Get("Authorization") != "Bearer deployment-token" {
			http.Error(w, "missing workload token", http.StatusUnauthorized)
			return
		}
		if r.Header.Get("X-OJOS-Caller-Service") != "" || r.Header.Get("X-OJOS-Caller-Node-Id") != "" {
			http.Error(w, "caller identity must be derived by Gateway", http.StatusBadRequest)
			return
		}
		data, _ := io.ReadAll(r.Body)
		_ = json.NewEncoder(w).Encode(map[string]any{"sha256": sha256Hex(data), "size_bytes": len(data)})
	}))
	defer server.Close()
	tokenPath := filepath.Join(root, "token")
	contextPath := filepath.Join(root, "context.json")
	if err := os.WriteFile(tokenPath, []byte("deployment-token"), 0o600); err != nil {
		t.Fatal(err)
	}
	contextJSON, err := json.Marshal(map[string]any{
		"schema_version": 1,
		"deployment":     map[string]any{"id": "problem-a", "service": "problem-service", "node": "node-a"},
		"gateway":        map[string]any{"origin": server.URL},
		"bindings": map[string]any{
			"storage.object.put":  map[string]any{"binding_id": "binding-put", "api_id": "storage.object.put", "base_path": "/internal/apis/storage.object.put", "timeout_ms": 300000},
			"storage.object.head": map[string]any{"binding_id": "binding-head", "api_id": "storage.object.head", "base_path": "/internal/apis/storage.object.head", "timeout_ms": 300000},
		},
		"credential_file": tokenPath,
		"generation":      2,
	})
	if err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(contextPath, contextJSON, 0o600); err != nil {
		t.Fatal(err)
	}
	t.Setenv("OJOS_SERVICE_CONTEXT_FILE", contextPath)
	artifact, err := PublishPackageArtifactTracked(t.Context(), config.StorageConfig{ProblemsRoot: root, Bucket: "problems"}, 71, root, &recordingIntentRegistrar{})
	if err != nil {
		t.Fatal(err)
	}
	if !strings.HasPrefix(storedPath, "/internal/apis/storage.object.put/problems/package-sha256-") || !strings.HasSuffix(storedPath, artifact.SHA256+".zip") {
		t.Fatalf("managed upload bypassed named binding: %s", storedPath)
	}
}

func TestPublishPackageArtifactAcceptsMatchingConditionalCreateCollision(t *testing.T) {
	root := t.TempDir()
	if err := os.WriteFile(filepath.Join(root, "problem.yaml"), []byte("format: ojos\n"), 0o644); err != nil {
		t.Fatal(err)
	}
	artifactPath, digest, size, err := BuildDeterministicPackageArtifact(root, root)
	if err != nil {
		t.Fatal(err)
	}
	defer os.Remove(artifactPath)
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		switch r.Method {
		case http.MethodPut:
			w.WriteHeader(http.StatusPreconditionFailed)
		case http.MethodHead:
			w.Header().Set("X-OJOS-Object-Sha256", digest)
			w.Header().Set("Content-Length", fmt.Sprintf("%d", size))
			w.WriteHeader(http.StatusOK)
		default:
			w.WriteHeader(http.StatusMethodNotAllowed)
		}
	}))
	defer server.Close()
	artifact, err := PublishPackageArtifactTracked(t.Context(), config.StorageConfig{ServiceEndpoint: server.URL, Bucket: "problems"}, 8, root, &recordingIntentRegistrar{})
	if err != nil {
		t.Fatal(err)
	}
	if artifact.SHA256 != digest || artifact.SizeBytes != size {
		t.Fatalf("unexpected artifact: %#v", artifact)
	}
}

func TestPublishPackageArtifactRejectsMismatchedConditionalCreateCollision(t *testing.T) {
	root := t.TempDir()
	if err := os.WriteFile(filepath.Join(root, "problem.yaml"), []byte("format: ojos\n"), 0o644); err != nil {
		t.Fatal(err)
	}
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if r.Method == http.MethodPut {
			w.WriteHeader(http.StatusPreconditionFailed)
			return
		}
		w.Header().Set("X-OJOS-Object-Sha256", strings.Repeat("0", 64))
		w.Header().Set("Content-Length", "1")
		w.WriteHeader(http.StatusOK)
	}))
	defer server.Close()
	_, err := PublishPackageArtifactTracked(t.Context(), config.StorageConfig{ServiceEndpoint: server.URL, Bucket: "problems"}, 9, root, &recordingIntentRegistrar{})
	if err == nil || !strings.Contains(err.Error(), "collision") {
		t.Fatalf("expected collision error, got %v", err)
	}
}
