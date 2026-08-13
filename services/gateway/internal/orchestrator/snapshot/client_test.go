package orchestratorsnapshot

import (
	"context"
	"encoding/json"
	"net/http"
	"net/http/httptest"
	"strings"
	"testing"
)

func TestClientUsesOnlyGetForOrchestratorSnapshotReads(t *testing.T) {
	var methods []string
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		methods = append(methods, r.Method)
		if r.Header.Get(orchestratorTokenHeader) != "token" {
			t.Fatalf("missing orchestrator token header")
		}
		writeEnvelope(t, w, map[string]any{
			"service_definitions": []any{
				map[string]any{"service_id": "gateway"},
			},
			"topology": map[string]any{
				"dependency_edges": []any{},
			},
			"components":      []any{},
			"permissions":     []any{},
			"menus":           []any{},
			"frontend_routes": []any{},
			"gateway_routes":  []any{},
		})
	}))
	defer server.Close()

	client := NewClient(server.URL, "token")
	if _, err := client.ListServices(context.Background()); err != nil {
		t.Fatalf("ListServices failed: %v", err)
	}
	if _, err := client.Topology(context.Background()); err != nil {
		t.Fatalf("Topology failed: %v", err)
	}

	if len(methods) != 2 {
		t.Fatalf("expected 2 requests, got %d", len(methods))
	}
	for _, method := range methods {
		if method != http.MethodGet {
			t.Fatalf("orchestrator snapshot client must use GET only, got %s", method)
		}
	}
}

func TestClientDecodesNodeEffectiveRoutesFromBareOrchestratorJSON(t *testing.T) {
	var gotPath string
	var gotQuery string
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		gotPath = r.URL.Path
		gotQuery = r.URL.RawQuery
		w.Header().Set("content-type", "application/json")
		if err := json.NewEncoder(w).Encode(map[string]any{
			"version": "1",
			"node_id": "child",
			"routes": []any{
				map[string]any{
					"api_id":                "storage.object.get",
					"provider_node_id":      "root",
					"provider_endpoint":     "10.0.0.1:8085:storage-service",
					"provider_service_name": "storage-service",
					"prefix":                "/api/storage/objects",
					"target_service":        "storage-service",
					"upstream_base":         "http://10.0.0.1:8085",
					"auth_mode":             "service",
					"required_permission":   "storage.object.read",
					"methods":               []string{"GET"},
					"enabled":               true,
					"proxy_enabled":         true,
					"priority":              20,
					"created_from":          "orchestrator_effective_api_view",
					"status":                "active",
				},
			},
			"warnings":  []any{},
			"can_proxy": true,
		}); err != nil {
			t.Fatal(err)
		}
	}))
	defer server.Close()

	client := NewClient(server.URL, "token")
	var table struct {
		Routes []GatewayRoute `json:"routes"`
	}
	if err := client.DecodeNodeOrchestratorRoutes(context.Background(), "child", true, &table); err != nil {
		t.Fatalf("DecodeNodeOrchestratorRoutes failed: %v", err)
	}
	if gotPath != "/internal/orchestrator/nodes/child/routes" {
		t.Fatalf("unexpected path %s", gotPath)
	}
	if !strings.Contains(gotQuery, "include_upstream=true") {
		t.Fatalf("include_upstream query missing: %s", gotQuery)
	}
	if len(table.Routes) != 1 || table.Routes[0].ApiID != "storage.object.get" {
		t.Fatalf("expected storage api route, got %#v", table.Routes)
	}
}

func TestClientDecodesTypedContributionSnapshot(t *testing.T) {
	var gotPath string
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		gotPath = r.URL.Path
		w.Header().Set("content-type", "application/json")
		if err := json.NewEncoder(w).Encode(map[string]any{"data": map[string]any{
			"schema_version": "ojos.dev/contribution-snapshot/v1",
			"digest":         "sha256:fixture",
			"scope_id":       "default",
			"revisions": []any{map[string]any{
				"service_id": "contest-service", "deployment_id": "dep-1",
				"revision_id": "rev-1", "generation": 4, "runtime_ready": true,
			}},
			"gateway_routes": []any{map[string]any{
				"service_id": "contest-service", "deployment_id": "dep-1",
				"revision_id": "rev-1", "generation": 4, "audience": "user",
				"method": "GET", "path": "/api/contests/{contestId}",
				"api_id": "contest-service.api", "operation_id": "getContest",
				"provider_path": "/contests/{contestId}", "auth": "REQUIRED",
				"permission": "contest.read", "permission_scope": map[string]any{"type": "contest", "pathParameter": "contestId"},
				"upstream_base": "http://contest:8080", "enabled": true,
			}},
		}, "meta": map[string]any{"request_id": "req-1", "api_version": "v1"}}); err != nil {
			t.Fatal(err)
		}
	}))
	defer server.Close()

	snapshot, err := NewClient(server.URL, "token").ContributionSnapshot(context.Background())
	if err != nil {
		t.Fatal(err)
	}
	if gotPath != "/api/v1/contributions/snapshot" || snapshot.Digest != "sha256:fixture" || len(snapshot.GatewayRoutes) != 1 {
		t.Fatalf("unexpected contribution snapshot: path=%q snapshot=%#v", gotPath, snapshot)
	}
	route := snapshot.GatewayRoutes[0]
	if route.OperationID != "getContest" || route.ProviderPath != "/contests/{contestId}" || route.Generation != 4 {
		t.Fatalf("typed route identity was not decoded: %#v", route)
	}
	if route.PermissionScope == nil || route.PermissionScope.Kind != "path_parameter" || route.PermissionScope.Type != "contest" || route.PermissionScope.PathParameter != "contestId" {
		t.Fatalf("typed permission scope was not decoded: %#v", route.PermissionScope)
	}
}

func TestContributionAcknowledgementRejectsInvalidResponse(t *testing.T) {
	digest := "sha256:" + strings.Repeat("a", 64)
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		w.Header().Set("Content-Type", "application/json")
		_ = json.NewEncoder(w).Encode(map[string]any{"data": map[string]any{
			"schema_version":  contributionAckSchema,
			"target":          "AUTH",
			"scope_id":        "default",
			"snapshot_digest": digest,
			"accepted":        true,
		}, "meta": map[string]any{"request_id": "request", "api_version": "v1"}})
	}))
	defer server.Close()
	err := NewClient(server.URL, "internal", "ack").AcknowledgeContributionSnapshot(context.Background(), ContributionSnapshot{
		SchemaVersion: "ojos.dev/contribution-snapshot/v1", Digest: digest, ScopeID: "default",
	})
	if err == nil || !strings.Contains(err.Error(), "identity is invalid") {
		t.Fatalf("invalid acknowledgement response accepted: %v", err)
	}
}

func TestContributionAcknowledgementAcceptsExactV1Envelope(t *testing.T) {
	digest := "sha256:" + strings.Repeat("a", 64)
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		w.Header().Set("Content-Type", "application/json")
		_ = json.NewEncoder(w).Encode(map[string]any{"data": map[string]any{
			"schema_version":  contributionAckSchema,
			"target":          "GATEWAY",
			"scope_id":        "default",
			"snapshot_digest": digest,
			"accepted":        true,
		}, "meta": map[string]any{"request_id": "request", "api_version": "v1"}})
	}))
	defer server.Close()
	err := NewClient(server.URL, "internal", "ack").AcknowledgeContributionSnapshot(context.Background(), ContributionSnapshot{
		SchemaVersion: "ojos.dev/contribution-snapshot/v1", Digest: digest, ScopeID: "default",
	})
	if err != nil {
		t.Fatalf("exact v1 acknowledgement envelope was rejected: %v", err)
	}
}

func TestContributionAcknowledgementRejectsLegacyRootStatusDecoration(t *testing.T) {
	digest := "sha256:" + strings.Repeat("a", 64)
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		w.Header().Set("Content-Type", "application/json")
		_ = json.NewEncoder(w).Encode(map[string]any{
			"data": map[string]any{
				"schema_version": contributionAckSchema, "target": "GATEWAY", "scope_id": "default",
				"snapshot_digest": digest, "accepted": true,
			},
			"meta":   map[string]any{"request_id": "request", "api_version": "v1"},
			"status": "ok",
		})
	}))
	defer server.Close()
	err := NewClient(server.URL, "internal", "ack").AcknowledgeContributionSnapshot(context.Background(), ContributionSnapshot{
		SchemaVersion: "ojos.dev/contribution-snapshot/v1", Digest: digest, ScopeID: "default",
	})
	if err == nil || !strings.Contains(err.Error(), `unknown field "status"`) {
		t.Fatalf("legacy root status decoration was not rejected: %v", err)
	}
}

func TestPermissionScopeUsesStableWireRepresentation(t *testing.T) {
	resource := PermissionScope{Kind: "path_parameter", Type: "contest", PathParameter: "contestId"}
	encoded, err := json.Marshal(resource)
	if err != nil || string(encoded) != `{"type":"contest","pathParameter":"contestId"}` {
		t.Fatalf("unexpected resource scope encoding: %s err=%v", encoded, err)
	}
	system, err := json.Marshal(PermissionScope{Kind: "system", Type: "system"})
	if err != nil || string(system) != `"system"` {
		t.Fatalf("unexpected system scope encoding: %s err=%v", system, err)
	}
	for _, invalid := range []string{
		`{"type":"system","pathParameter":"id"}`,
		`{"type":"contest","pathParameter":"id","header":"x"}`,
		`{"type":"Contest","pathParameter":"id"}`,
	} {
		var decoded PermissionScope
		if err := json.Unmarshal([]byte(invalid), &decoded); err == nil {
			t.Fatalf("invalid permission scope decoded: %s -> %#v", invalid, decoded)
		}
	}
}

func writeEnvelope(t *testing.T, w http.ResponseWriter, data any) {
	t.Helper()
	w.Header().Set("content-type", "application/json")
	if err := json.NewEncoder(w).Encode(envelope[any]{Code: 0, Msg: "ok", Data: data}); err != nil {
		t.Fatal(err)
	}
}
