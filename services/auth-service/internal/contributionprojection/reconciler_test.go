package contributionprojection

import (
	"context"
	"encoding/json"
	"errors"
	"net/http"
	"net/http/httptest"
	"strings"
	"sync"
	"testing"

	"ojos-auth-service/internal/repository"
)

type fakeStore struct {
	mu          sync.Mutex
	digest      string
	definitions []repository.ContributionPermissionDefinitionInput
	calls       int
	err         error
}

func (s *fakeStore) ReconcileContributionPermissions(_ context.Context, digest string, definitions []repository.ContributionPermissionDefinitionInput) error {
	s.mu.Lock()
	defer s.mu.Unlock()
	s.digest = digest
	s.definitions = append([]repository.ContributionPermissionDefinitionInput(nil), definitions...)
	s.calls++
	return s.err
}

func TestReconcilerProjectsOnlyDefinitionsWithInternalAuthentication(t *testing.T) {
	digest := "sha256:" + repeat("a", 64)
	revision := "sha256:" + repeat("b", 64)
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if r.URL.Path != "/api/v1/contributions/snapshot" || r.Header.Get("x-ojos-orchestrator-token") != "projection-token" {
			http.Error(w, "unauthorized", http.StatusUnauthorized)
			return
		}
		_ = json.NewEncoder(w).Encode(map[string]any{
			"data": map[string]any{
				"schema_version":   snapshotSchema,
				"digest":           digest,
				"scope_id":         "default",
				"acknowledgements": []map[string]any{},
				"permission_definitions": []map[string]any{
					{"service_id": "contest-service", "revision_id": revision, "generation": 2, "key": "contest.read", "title": "Read contests", "description": ""},
				},
			},
			"meta": map[string]any{"api_version": "v1", "request_id": "request-1"},
		})
	}))
	defer server.Close()
	store := &fakeStore{}
	reconciler, err := New(server.URL, "projection-token", "", store)
	if err != nil {
		t.Fatal(err)
	}
	if err := reconciler.Reconcile(t.Context()); err != nil {
		t.Fatal(err)
	}
	if store.calls != 1 || store.digest != digest || len(store.definitions) != 1 {
		t.Fatalf("unexpected durable projection: %+v", store)
	}
	definition := store.definitions[0]
	if definition.Code != "contest.read" || definition.ServiceCode != "contest-service" || definition.Title != "Read contests" {
		t.Fatalf("unexpected definition: %+v", definition)
	}
}

func TestReconcilerRejectsDuplicateOrUnprovenDefinitionsWithoutWriting(t *testing.T) {
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
		_ = json.NewEncoder(w).Encode(map[string]any{"data": map[string]any{
			"schema_version":   snapshotSchema,
			"digest":           "sha256:" + repeat("a", 64),
			"scope_id":         "default",
			"acknowledgements": []map[string]any{},
			"permission_definitions": []map[string]any{
				{"service_id": "one", "revision_id": "not-a-digest", "generation": 1, "key": "shared.read", "title": "Read", "description": ""},
			},
		}})
	}))
	defer server.Close()
	store := &fakeStore{}
	reconciler, err := New(server.URL, "projection-token", "", store)
	if err != nil {
		t.Fatal(err)
	}
	if err := reconciler.Reconcile(t.Context()); err == nil {
		t.Fatal("unproven definition was accepted")
	}
	if store.calls != 0 {
		t.Fatal("invalid snapshot reached the durable effect boundary")
	}
}

func TestReconcilerRetriesFailedAcknowledgementAfterDurableApply(t *testing.T) {
	digest := "sha256:" + repeat("a", 64)
	revision := "sha256:" + repeat("b", 64)
	var mu sync.Mutex
	posts := 0
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		mu.Lock()
		defer mu.Unlock()
		if r.Method == http.MethodGet {
			_ = json.NewEncoder(w).Encode(map[string]any{"data": map[string]any{
				"schema_version": snapshotSchema, "digest": digest, "scope_id": "default",
				"acknowledgements": []map[string]any{{
					"activation_id": "activation-1", "service_id": "contest", "candidate_revision_id": revision,
					"candidate_generation": 2, "expected_state": "ACTIVE", "observed_revision_id": revision, "observed_generation": 2,
				}},
				"permission_definitions": []map[string]any{},
			}, "meta": map[string]any{"api_version": "v1", "request_id": "get"}})
			return
		}
		posts++
		if r.Header.Get("x-ojos-orchestrator-token") != "projection-token" || r.Header.Get("x-ojos-contribution-ack-token") != "ack-token" || r.Header.Get("Idempotency-Key") != "contribution-projection-ack:AUTH:"+digest {
			t.Errorf("wrong acknowledgement headers: %v", r.Header)
		}
		var body struct {
			Acknowledgements []Acknowledgement `json:"acknowledgements"`
		}
		if err := json.NewDecoder(r.Body).Decode(&body); err != nil || len(body.Acknowledgements) != 1 || body.Acknowledgements[0].ActivationID != "activation-1" {
			t.Errorf("obligations were not returned unchanged: body=%+v err=%v", body, err)
		}
		if posts == 1 {
			http.Error(w, "retry", http.StatusServiceUnavailable)
			return
		}
		w.Header().Set("Content-Type", "application/json")
		_ = json.NewEncoder(w).Encode(map[string]any{"data": map[string]any{
			"schema_version": acknowledgementSchema, "target": "AUTH", "scope_id": "default", "snapshot_digest": digest, "accepted": true,
		}, "meta": map[string]any{"api_version": "v1", "request_id": "ack"}})
	}))
	defer server.Close()
	store := &fakeStore{}
	reconciler, err := New(server.URL, "projection-token", "ack-token", store)
	if err != nil {
		t.Fatal(err)
	}
	if err := reconciler.Reconcile(t.Context()); err == nil {
		t.Fatal("first acknowledgement failure was hidden")
	}
	if store.calls != 1 || reconciler.pending == nil {
		t.Fatal("durable apply was rolled back after acknowledgement failure")
	}
	if err := reconciler.Reconcile(t.Context()); err != nil {
		t.Fatalf("retry acknowledgement: %v", err)
	}
	if store.calls != 2 || posts != 2 || reconciler.pending != nil {
		t.Fatalf("unexpected retry state calls=%d posts=%d pending=%v", store.calls, posts, reconciler.pending != nil)
	}
}

func TestReconcilerDoesNotAcknowledgeFailedDurableApply(t *testing.T) {
	digest := "sha256:" + repeat("a", 64)
	posts := 0
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if r.Method == http.MethodPost {
			posts++
			return
		}
		_ = json.NewEncoder(w).Encode(map[string]any{"data": map[string]any{
			"schema_version": snapshotSchema, "digest": digest, "scope_id": "default",
			"acknowledgements": []map[string]any{}, "permission_definitions": []map[string]any{},
		}})
	}))
	defer server.Close()
	store := &fakeStore{err: errors.New("apply failed")}
	reconciler, err := New(server.URL, "projection-token", "ack-token", store)
	if err != nil {
		t.Fatal(err)
	}
	if err := reconciler.Reconcile(t.Context()); err == nil {
		t.Fatal("durable apply failure was hidden")
	}
	if posts != 0 {
		t.Fatalf("durable apply failure emitted %d acknowledgements", posts)
	}
}

func TestReconcilerRejectsInvalidAcknowledgementResponse(t *testing.T) {
	digest := "sha256:" + repeat("a", 64)
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if r.Method == http.MethodGet {
			_ = json.NewEncoder(w).Encode(map[string]any{"data": map[string]any{
				"schema_version": snapshotSchema, "digest": digest, "scope_id": "default",
				"acknowledgements": []map[string]any{}, "permission_definitions": []map[string]any{},
			}})
			return
		}
		w.Header().Set("Content-Type", "application/json")
		_ = json.NewEncoder(w).Encode(map[string]any{"data": map[string]any{
			"schema_version": acknowledgementSchema, "target": "GATEWAY", "scope_id": "default", "snapshot_digest": digest, "accepted": true,
		}, "meta": map[string]any{"api_version": "v1"}})
	}))
	defer server.Close()
	reconciler, err := New(server.URL, "projection-token", "ack-token", &fakeStore{})
	if err != nil {
		t.Fatal(err)
	}
	if err := reconciler.Reconcile(t.Context()); err == nil {
		t.Fatal("invalid acknowledgement response was accepted")
	}
}

func TestReconcilerRejectsLegacyRootStatusDecoration(t *testing.T) {
	digest := "sha256:" + repeat("a", 64)
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if r.Method == http.MethodGet {
			_ = json.NewEncoder(w).Encode(map[string]any{"data": map[string]any{
				"schema_version": snapshotSchema, "digest": digest, "scope_id": "default",
				"acknowledgements": []map[string]any{}, "permission_definitions": []map[string]any{},
			}})
			return
		}
		w.Header().Set("Content-Type", "application/json")
		_ = json.NewEncoder(w).Encode(map[string]any{
			"data": map[string]any{
				"schema_version": acknowledgementSchema, "target": "AUTH", "scope_id": "default",
				"snapshot_digest": digest, "accepted": true,
			},
			"meta":   map[string]any{"api_version": "v1", "request_id": "ack"},
			"status": "ok",
		})
	}))
	defer server.Close()
	reconciler, err := New(server.URL, "projection-token", "ack-token", &fakeStore{})
	if err != nil {
		t.Fatal(err)
	}
	err = reconciler.Reconcile(t.Context())
	if err == nil || !strings.Contains(err.Error(), `unknown field "status"`) {
		t.Fatalf("legacy root status decoration was not rejected: %v", err)
	}
}

func repeat(value string, count int) string {
	result := ""
	for range count {
		result += value
	}
	return result
}
