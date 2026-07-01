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

func writeEnvelope(t *testing.T, w http.ResponseWriter, data any) {
	t.Helper()
	w.Header().Set("content-type", "application/json")
	if err := json.NewEncoder(w).Encode(envelope[any]{Code: 0, Msg: "ok", Data: data}); err != nil {
		t.Fatal(err)
	}
}
