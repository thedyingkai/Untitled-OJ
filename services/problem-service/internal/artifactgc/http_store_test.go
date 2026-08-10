package artifactgc

import (
	"encoding/json"
	"net/http"
	"net/http/httptest"
	"os"
	"path/filepath"
	"testing"
	"time"

	"ojos-shared/storagecontract"
)

func TestBoundObjectStoreUsesNamedBindingsTokenAndConditionalIdentity(t *testing.T) {
	digest := "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
	intent := boundStoreIntent(digest)
	var methods []string
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		methods = append(methods, r.Method)
		if r.Header.Get("Authorization") != "Bearer workload-token" {
			http.Error(w, "missing workload token", http.StatusUnauthorized)
			return
		}
		if r.URL.Path != "/internal/apis/storage.object.head/problems/"+intent.Key && r.URL.Path != "/internal/apis/storage.object.delete/problems/"+intent.Key {
			http.Error(w, "wrong bound path", http.StatusNotFound)
			return
		}
		if r.Method == http.MethodHead {
			w.Header().Set(storagecontract.ResultHeader, storagecontract.ResultPresent)
			w.Header().Set("X-OJOS-Object-Sha256", digest)
			w.Header().Set("Content-Length", "17")
			return
		}
		if r.Header.Get("X-OJOS-Expected-Sha256") != digest || r.Header.Get("X-OJOS-Expected-Size") != "17" {
			http.Error(w, "missing conditional identity", http.StatusPreconditionFailed)
			return
		}
		w.Header().Set(storagecontract.ResultHeader, storagecontract.ResultDeleted)
		w.Header().Set("Content-Type", "application/json")
		_, _ = w.Write([]byte(`{"deleted":true}`))
	}))
	defer server.Close()
	store := newBoundObjectStoreForTest(t, server.URL)
	deleteTimeout, err := store.DeleteBindingTimeout()
	if err != nil || deleteTimeout != 60*time.Second {
		t.Fatalf("unexpected storage_delete binding timeout: %s (%v)", deleteTimeout, err)
	}
	object, exists, err := store.Inspect(t.Context(), intent)
	if err != nil || !exists || object.SHA256 != digest || object.SizeBytes != 17 {
		t.Fatalf("bound HEAD failed: object=%#v exists=%v err=%v", object, exists, err)
	}
	if err := store.DeleteIfMatches(t.Context(), intent); err != nil {
		t.Fatal(err)
	}
	if len(methods) != 2 || methods[0] != http.MethodHead || methods[1] != http.MethodDelete {
		t.Fatalf("unexpected bound method sequence: %#v", methods)
	}
}

func TestBoundObjectStoreRequiresAuthoritativeDeleteResult(t *testing.T) {
	digest := "eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee"
	intent := boundStoreIntent(digest)
	for _, tt := range []struct {
		name string
		body string
	}{
		{name: "empty"},
		{name: "missing field", body: `{"status":"ok"}`},
		{name: "false", body: `{"deleted":false}`},
		{name: "malformed", body: `{"deleted":`},
	} {
		t.Run(tt.name, func(t *testing.T) {
			server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
				w.Header().Set(storagecontract.ResultHeader, storagecontract.ResultDeleted)
				w.WriteHeader(http.StatusOK)
				_, _ = w.Write([]byte(tt.body))
			}))
			defer server.Close()

			store := newBoundObjectStoreForTest(t, server.URL)
			err := store.DeleteIfMatches(t.Context(), intent)
			if err == nil {
				t.Fatal("unproven HTTP 200 was accepted as an object deletion")
			}
			if !isDeterministicProviderFailure(err) {
				t.Fatalf("delete response contract violation was not deterministic: %v", err)
			}
		})
	}
}

func TestBoundObjectStoreRequiresStorageDeleteProvenance(t *testing.T) {
	digest := "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff"
	intent := boundStoreIntent(digest)
	for _, result := range []string{"", "present", "route-deleted"} {
		t.Run("result_"+result, func(t *testing.T) {
			server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
				if result != "" {
					w.Header().Set(storagecontract.ResultHeader, result)
				}
				_, _ = w.Write([]byte(`{"deleted":true}`))
			}))
			defer server.Close()

			store := newBoundObjectStoreForTest(t, server.URL)
			err := store.DeleteIfMatches(t.Context(), intent)
			if err == nil {
				t.Fatalf("delete provenance %q was accepted", result)
			}
			if !isDeterministicProviderFailure(err) {
				t.Fatalf("delete provenance violation was not deterministic: %v", err)
			}
		})
	}
}

func TestBoundObjectStoreAcceptsOnlyAuthoritativeStorageAbsence(t *testing.T) {
	digest := "cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc"
	intent := boundStoreIntent(digest)
	tests := []struct {
		name       string
		result     string
		wantAbsent bool
	}{
		{name: "storage proof", result: storagecontract.ResultObjectNotFound, wantAbsent: true},
		{name: "gateway route 404"},
		{name: "unknown proof", result: "route-not-found"},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
				if tt.result != "" {
					w.Header().Set(storagecontract.ResultHeader, tt.result)
				}
				w.WriteHeader(http.StatusNotFound)
			}))
			defer server.Close()

			store := newBoundObjectStoreForTest(t, server.URL)
			_, exists, err := store.Inspect(t.Context(), intent)
			if tt.wantAbsent {
				if err != nil || exists {
					t.Fatalf("authoritative absence rejected: exists=%v err=%v", exists, err)
				}
				return
			}
			if err == nil {
				t.Fatalf("unproven 404 was accepted as object absence: exists=%v", exists)
			}
			if !isDeterministicProviderFailure(err) {
				t.Fatalf("unproven provider 404 was not classified deterministic: %v", err)
			}
		})
	}
}

func TestBoundObjectStoreRejectsUnprovenSuccess(t *testing.T) {
	for _, result := range []string{"", "deleted"} {
		t.Run("result_"+result, func(t *testing.T) {
			digest := "dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd"
			intent := boundStoreIntent(digest)
			server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
				w.Header().Set("X-OJOS-Object-Sha256", digest)
				w.Header().Set("Content-Length", "17")
				if result != "" {
					w.Header().Set(storagecontract.ResultHeader, result)
				}
				w.WriteHeader(http.StatusOK)
			}))
			defer server.Close()

			store := newBoundObjectStoreForTest(t, server.URL)
			if _, _, err := store.Inspect(t.Context(), intent); err == nil {
				t.Fatal("unproven 200 was accepted as authoritative Storage metadata")
			} else if !isDeterministicProviderFailure(err) {
				t.Fatalf("provider result contract violation was not deterministic: %v", err)
			}
		})
	}
}

func TestBoundObjectStoreRejectsDeleteRouteNotFound(t *testing.T) {
	digest := "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
	intent := boundStoreIntent(digest)
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if r.Method != http.MethodDelete {
			http.Error(w, "unexpected method", http.StatusMethodNotAllowed)
			return
		}
		http.Error(w, "binding route not found", http.StatusNotFound)
	}))
	defer server.Close()

	store := newBoundObjectStoreForTest(t, server.URL)
	if err := store.DeleteIfMatches(t.Context(), intent); err == nil {
		t.Fatal("Gateway/storage binding 404 was accepted as a successful object deletion")
	} else if !isDeterministicProviderFailure(err) {
		t.Fatalf("Gateway/storage binding 404 was not classified deterministic: %v", err)
	}
}

func newBoundObjectStoreForTest(t *testing.T, origin string) *BoundObjectStore {
	t.Helper()
	dir := t.TempDir()
	token := filepath.Join(dir, "token")
	contextFile := filepath.Join(dir, "context.json")
	if err := os.WriteFile(token, []byte("workload-token"), 0o600); err != nil {
		t.Fatal(err)
	}
	document := map[string]any{
		"schema_version": 1,
		"deployment":     map[string]any{"id": "problem-a", "service": "problem-service", "node": "node-a"},
		"gateway":        map[string]any{"origin": origin},
		"bindings": map[string]any{
			"storage_head":   map[string]any{"binding_id": "head", "api_id": "storage.object.head", "base_path": "/internal/apis/storage.object.head", "timeout_ms": 300000},
			"storage_delete": map[string]any{"binding_id": "delete", "api_id": "storage.object.delete", "base_path": "/internal/apis/storage.object.delete", "timeout_ms": 60000},
		},
		"credential_file": token, "generation": 3,
	}
	bytes, err := json.Marshal(document)
	if err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(contextFile, bytes, 0o600); err != nil {
		t.Fatal(err)
	}
	t.Setenv("OJOS_SERVICE_CONTEXT_FILE", contextFile)
	store, err := NewBoundObjectStore("problems")
	if err != nil {
		t.Fatal(err)
	}
	return store
}

func boundStoreIntent(digest string) Intent {
	key := "package-sha256-" + digest + ".zip"
	return Intent{
		URI: "storage://problems/" + key, Key: key, SHA256: digest,
		SizeBytes: 17, ClaimToken: "claim", AttemptCount: 1,
	}
}
