package orchestratorsnapshot

import (
	"context"
	"encoding/json"
	"net/http"
	"net/http/httptest"
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

func writeEnvelope(t *testing.T, w http.ResponseWriter, data any) {
	t.Helper()
	w.Header().Set("content-type", "application/json")
	if err := json.NewEncoder(w).Encode(envelope[any]{Code: 0, Msg: "ok", Data: data}); err != nil {
		t.Fatal(err)
	}
}
