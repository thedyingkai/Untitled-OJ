package types

import (
	"encoding/json"
	"net/http"
	"net/http/httptest"
	"strings"
	"testing"

	"github.com/zeromicro/go-zero/rest/httpx"
)

func TestAdminRoutesReloadAcceptsOmittedOptionalRouteMetadata(t *testing.T) {
	body := `{
		"pushed_route_table": true,
		"routes": [{
			"route_id": "compose-smoke:problem-service",
			"owner_service_id": "problem-service",
			"prefix": "/api/problem",
			"service_id": "problem-service",
			"target_service": "problem-service",
			"auth_mode": "user",
			"methods": ["ANY"],
			"enabled": true,
			"proxy_enabled": true,
			"priority": 12,
			"created_from": "compose_smoke_pushed_route_table",
			"status": "active",
			"conflicts": [],
			"warnings": [],
			"blocked_by": []
		}]
	}`
	request := httptest.NewRequest(http.MethodPost, "/api/admin/orchestrator/routes/reload", strings.NewReader(body))
	request.Header.Set("Content-Type", "application/json")

	var parsed AdminRoutesReloadReq
	if err := httpx.Parse(request, &parsed); err != nil {
		t.Fatalf("parse pushed route table: %v", err)
	}
	if len(parsed.Routes) != 1 || parsed.Routes[0].RouteId != "compose-smoke:problem-service" {
		t.Fatalf("unexpected parsed route table: %#v", parsed)
	}
	if parsed.Routes[0].BindingId != "" || parsed.Routes[0].ConsumerDeploymentId != "" {
		t.Fatalf("omitted optional binding metadata was synthesized: %#v", parsed.Routes[0])
	}

	encoded, err := json.Marshal(parsed.Routes[0])
	if err != nil {
		t.Fatal(err)
	}
	if strings.Contains(string(encoded), "binding_id") || strings.Contains(string(encoded), "consumer_deployment_id") {
		t.Fatalf("omitempty behavior was lost: %s", encoded)
	}
}
