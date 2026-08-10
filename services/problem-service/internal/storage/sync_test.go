package storage

import (
	"context"
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
	"fmt"
	"io"
	"net/http"
	"net/http/httptest"
	"os"
	"path/filepath"
	"strings"
	"sync"
	"testing"

	"ojos-problem-service/internal/config"
	"ojos-problem-service/internal/packagefs"
	"ojos-shared/eventing"
)

func TestSyncProblemFilesUsesImmutableContentAddressedObjects(t *testing.T) {
	t.Setenv("OJOS_SERVICE_CONTEXT_FILE", "")
	t.Setenv("OJOS_SERVICE_CONTEXT_DIR", "")

	objects := map[string]string{}
	var mu sync.Mutex
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if r.Method != http.MethodPut {
			http.Error(w, "method", http.StatusMethodNotAllowed)
			return
		}
		body, err := io.ReadAll(r.Body)
		if err != nil {
			http.Error(w, err.Error(), http.StatusBadRequest)
			return
		}
		mu.Lock()
		objects[r.URL.Path] = string(body)
		mu.Unlock()
		digest := sha256.Sum256(body)
		w.WriteHeader(http.StatusCreated)
		_ = json.NewEncoder(w).Encode(map[string]any{
			"sha256": hex.EncodeToString(digest[:]), "size_bytes": len(body),
		})
	}))
	defer server.Close()

	path := filepath.Join(t.TempDir(), "problem.yaml")
	intents := &recordingIntentRegistrar{}
	syncVersion := func(content string) packagefs.IndexedFile {
		t.Helper()
		if err := os.WriteFile(path, []byte(content), 0o644); err != nil {
			t.Fatal(err)
		}
		digestBytes := sha256.Sum256([]byte(content))
		digest := hex.EncodeToString(digestBytes[:])
		files, err := SyncProblemFiles(t.Context(), config.StorageConfig{
			ServiceEndpoint: server.URL,
			Bucket:          "problems",
		}, 17, []packagefs.IndexedFile{{
			LogicalPath: "problem.yaml",
			StoragePath: path,
			Sha256:      digest,
			SizeBytes:   int64(len(content)),
			MimeType:    "application/yaml",
		}}, intents)
		if err != nil {
			t.Fatal(err)
		}
		return files[0]
	}

	oldFile := syncVersion("title: old\n")
	newFile := syncVersion("title: new\n")
	if oldFile.StoragePath == newFile.StoragePath {
		t.Fatal("different staged bytes overwrote the same storage object")
	}

	mu.Lock()
	defer mu.Unlock()
	oldPath := "/api/storage/objects/problems/" + strings.TrimPrefix(oldFile.StoragePath, "storage://problems/")
	newPath := "/api/storage/objects/problems/" + strings.TrimPrefix(newFile.StoragePath, "storage://problems/")
	if objects[oldPath] != "title: old\n" || objects[newPath] != "title: new\n" {
		t.Fatalf("content-addressed objects were not retained independently: %#v", objects)
	}
	if len(intents.artifacts) != 2 ||
		intents.artifacts[0].URI != oldFile.StoragePath ||
		intents.artifacts[1].URI != newFile.StoragePath {
		t.Fatalf("each immutable file upload must have a matching durable intent: %#v", intents.artifacts)
	}
}

func TestSyncProblemFilesRejectsBytesChangedAfterIndex(t *testing.T) {
	t.Setenv("OJOS_SERVICE_CONTEXT_FILE", "")
	t.Setenv("OJOS_SERVICE_CONTEXT_DIR", "")
	uploaded := make(chan struct{}, 1)
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
		uploaded <- struct{}{}
		w.WriteHeader(http.StatusCreated)
	}))
	defer server.Close()

	path := filepath.Join(t.TempDir(), "case.in")
	if err := os.WriteFile(path, []byte("changed"), 0o644); err != nil {
		t.Fatal(err)
	}
	intents := &recordingIntentRegistrar{}
	_, err := SyncProblemFiles(t.Context(), config.StorageConfig{ServiceEndpoint: server.URL}, 19, []packagefs.IndexedFile{{
		LogicalPath: "tests/1.in",
		StoragePath: path,
		Sha256:      strings.Repeat("0", 64),
	}}, intents)
	if err == nil || !strings.Contains(err.Error(), "changed while staging") {
		t.Fatalf("expected staging digest mismatch, got %v", err)
	}
	select {
	case <-uploaded:
		t.Fatal("mismatched bytes were uploaded")
	default:
	}
	if len(intents.artifacts) != 0 {
		t.Fatalf("changed bytes registered an upload intent: %#v", intents.artifacts)
	}
}

func TestSyncProblemFilesRegistersIntentBeforeEveryRemotePut(t *testing.T) {
	t.Setenv("OJOS_SERVICE_CONTEXT_FILE", "")
	t.Setenv("OJOS_SERVICE_CONTEXT_DIR", "")
	var events []string
	registrar := &orderedIntentRegistrar{events: &events}
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		body, _ := io.ReadAll(r.Body)
		digest := sha256.Sum256(body)
		events = append(events, "put:"+filepath.Base(r.URL.Path))
		w.WriteHeader(http.StatusCreated)
		_ = json.NewEncoder(w).Encode(map[string]any{
			"sha256": hex.EncodeToString(digest[:]), "size_bytes": len(body),
		})
	}))
	defer server.Close()

	root := t.TempDir()
	files := make([]packagefs.IndexedFile, 0, 2)
	for index, content := range []string{"alpha", "beta"} {
		path := filepath.Join(root, fmt.Sprintf("%d.txt", index))
		if err := os.WriteFile(path, []byte(content), 0o600); err != nil {
			t.Fatal(err)
		}
		files = append(files, packagefs.IndexedFile{
			LogicalPath: filepath.Base(path), StoragePath: path, MimeType: "text/plain",
		})
	}

	synced, err := SyncProblemFiles(t.Context(), config.StorageConfig{
		ServiceEndpoint: server.URL, Bucket: "problems",
	}, 23, files, registrar)
	if err != nil {
		t.Fatal(err)
	}
	if len(events) != 6 || !strings.HasPrefix(events[0], "intent:") ||
		!strings.HasPrefix(events[1], "put:") || !strings.HasPrefix(events[2], "completed:") ||
		!strings.HasPrefix(events[3], "intent:") || !strings.HasPrefix(events[4], "put:") ||
		!strings.HasPrefix(events[5], "completed:") {
		t.Fatalf("remote file publication was not intent-before-PUT-before-complete: %#v", events)
	}
	for index := range synced {
		offset := index * 3
		intentKey := strings.TrimPrefix(events[offset], "intent:")
		if intentKey != strings.TrimPrefix(events[offset+1], "put:") ||
			intentKey != strings.TrimPrefix(events[offset+2], "completed:") {
			t.Fatalf("intent and PUT key differ: %#v", events)
		}
	}
}

func TestSyncProblemFilesRefusesUntrackedRemotePublication(t *testing.T) {
	t.Setenv("OJOS_SERVICE_CONTEXT_FILE", "")
	t.Setenv("OJOS_SERVICE_CONTEXT_DIR", "")
	requests := 0
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
		requests++
		w.WriteHeader(http.StatusCreated)
	}))
	defer server.Close()
	path := filepath.Join(t.TempDir(), "problem.yaml")
	if err := os.WriteFile(path, []byte("title: untracked\n"), 0o600); err != nil {
		t.Fatal(err)
	}
	_, err := SyncProblemFiles(t.Context(), config.StorageConfig{ServiceEndpoint: server.URL}, 24, []packagefs.IndexedFile{{
		LogicalPath: "problem.yaml", StoragePath: path,
	}}, nil)
	if err == nil || !strings.Contains(err.Error(), "upload-intent registrar") || requests != 0 {
		t.Fatalf("untracked remote publication was not rejected before PUT: err=%v requests=%d", err, requests)
	}
}

func TestSyncProblemFilesVerifiesExistingImmutableObjectOnPrecondition(t *testing.T) {
	t.Setenv("OJOS_SERVICE_CONTEXT_FILE", "")
	t.Setenv("OJOS_SERVICE_CONTEXT_DIR", "")
	content := []byte("same immutable bytes")
	digestBytes := sha256.Sum256(content)
	digest := hex.EncodeToString(digestBytes[:])
	var methods []string
	putCount := 0
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		methods = append(methods, r.Method)
		switch r.Method {
		case http.MethodPut:
			putCount++
			if r.Header.Get("If-None-Match") != "*" || r.Header.Get("X-OJOS-Content-Sha256") != digest {
				http.Error(w, "missing immutable precondition", http.StatusBadRequest)
				return
			}
			if putCount > 1 {
				w.WriteHeader(http.StatusPreconditionFailed)
				return
			}
			_ = json.NewEncoder(w).Encode(map[string]any{"sha256": digest, "size_bytes": len(content)})
		case http.MethodHead:
			w.Header().Set("X-OJOS-Object-Sha256", digest)
			w.Header().Set("Content-Length", fmt.Sprintf("%d", len(content)))
		default:
			w.WriteHeader(http.StatusMethodNotAllowed)
		}
	}))
	defer server.Close()
	path := filepath.Join(t.TempDir(), "case.in")
	if err := os.WriteFile(path, content, 0o600); err != nil {
		t.Fatal(err)
	}
	registrar := &recordingIntentRegistrar{}
	files := []packagefs.IndexedFile{{LogicalPath: "tests/1.in", StoragePath: path}}
	for range 2 {
		if _, err := SyncProblemFiles(t.Context(), config.StorageConfig{ServiceEndpoint: server.URL}, 25, files, registrar); err != nil {
			t.Fatal(err)
		}
	}
	if strings.Join(methods, ",") != "PUT,PUT,HEAD" || len(registrar.artifacts) != 2 {
		t.Fatalf("duplicate immutable upload was not verified by HEAD: methods=%v intents=%#v", methods, registrar.artifacts)
	}
}

func TestSyncProblemFilesFailsClosedOnImmutableObjectCollision(t *testing.T) {
	t.Setenv("OJOS_SERVICE_CONTEXT_FILE", "")
	t.Setenv("OJOS_SERVICE_CONTEXT_DIR", "")
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if r.Method == http.MethodPut {
			w.WriteHeader(http.StatusPreconditionFailed)
			return
		}
		w.Header().Set("X-OJOS-Object-Sha256", strings.Repeat("f", 64))
		w.Header().Set("Content-Length", "999")
	}))
	defer server.Close()
	path := filepath.Join(t.TempDir(), "case.out")
	if err := os.WriteFile(path, []byte("expected"), 0o600); err != nil {
		t.Fatal(err)
	}
	registrar := &recordingIntentRegistrar{}
	_, err := SyncProblemFiles(t.Context(), config.StorageConfig{ServiceEndpoint: server.URL}, 26, []packagefs.IndexedFile{{
		LogicalPath: "tests/1.out", StoragePath: path,
	}}, registrar)
	if err == nil || !strings.Contains(err.Error(), "collision") || len(registrar.artifacts) != 1 {
		t.Fatalf("immutable object collision did not fail closed after intent registration: err=%v intents=%#v", err, registrar.artifacts)
	}
}

func TestSyncProblemFilesSupportsZeroByteContentObject(t *testing.T) {
	t.Setenv("OJOS_SERVICE_CONTEXT_FILE", "")
	t.Setenv("OJOS_SERVICE_CONTEXT_DIR", "")
	const emptyDigest = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if r.Header.Get("X-OJOS-Content-Sha256") != emptyDigest {
			// Keep the assertion below authoritative; return a useful failure if
			// the request omitted the digest header.
			http.Error(w, "missing empty-object digest", http.StatusBadRequest)
			return
		}
		_ = json.NewEncoder(w).Encode(map[string]any{
			"sha256":     emptyDigest,
			"size_bytes": 0,
		})
	}))
	defer server.Close()
	path := filepath.Join(t.TempDir(), "empty.in")
	if err := os.WriteFile(path, nil, 0o600); err != nil {
		t.Fatal(err)
	}
	registrar := &recordingIntentRegistrar{}
	files, err := SyncProblemFiles(t.Context(), config.StorageConfig{ServiceEndpoint: server.URL}, 27, []packagefs.IndexedFile{{
		LogicalPath: "tests/empty.in", StoragePath: path,
	}}, registrar)
	if err != nil {
		t.Fatal(err)
	}
	if len(files) != 1 || files[0].SizeBytes != 0 || len(registrar.artifacts) != 1 || registrar.artifacts[0].SizeBytes != 0 {
		t.Fatalf("zero-byte content object identity was not preserved: files=%#v intents=%#v", files, registrar.artifacts)
	}
}

type orderedIntentRegistrar struct {
	events *[]string
}

func (r *orderedIntentRegistrar) RegisterArtifactUploadIntent(_ context.Context, artifact eventing.ArtifactRef) error {
	*r.events = append(*r.events, "intent:"+filepath.Base(artifact.URI))
	return nil
}

func (r *orderedIntentRegistrar) MarkArtifactUploadCompleted(_ context.Context, artifact eventing.ArtifactRef) error {
	*r.events = append(*r.events, "completed:"+filepath.Base(artifact.URI))
	return nil
}
