package handler

import (
	"encoding/json"
	"net/http"
	"net/http/httptest"
	"strings"
	"testing"

	orchestratorsnapshot "ojos-gateway/internal/orchestrator/snapshot"
	"ojos-gateway/internal/svc"
)

func TestContributionSnapshotHandlerUsesServerSideOrchestratorCredential(t *testing.T) {
	orchestrator := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if r.Header.Get("x-ojos-orchestrator-token") != "internal-token" || r.Header.Get("Authorization") != "" {
			http.Error(w, "bad credentials", http.StatusUnauthorized)
			return
		}
		w.Header().Set("Content-Type", "application/json")
		_, _ = w.Write([]byte(`{"data":{"schema_version":"ojos.dev/contribution-snapshot/v1","digest":"sha256:` + strings.Repeat("a", 64) + `","scope_id":"default","revisions":[{"service_id":"contest","deployment_id":"internal-deployment","revision_id":"rev-1","generation":1,"runtime_ready":true}],"gateway_routes":[{"service_id":"contest","deployment_id":"dep-1","revision_id":"rev-1","generation":1,"audience":"USER","method":"GET","path":"/api/contests","api_id":"contest.v1","operation_id":"listContests","provider_path":"/contests","auth":"user","permission":"contest.read","upstream_base":"http://10.0.0.8:8080","enabled":true},{"service_id":"contest","deployment_id":"dep-1","revision_id":"rev-1","generation":1,"audience":"INTERNAL","method":"GET","path":"/internal/contest","api_id":"contest.internal","operation_id":"internalContest","provider_path":"/internal","auth":"internal","upstream_base":"http://10.0.0.8:8080","enabled":true}],"permission_definitions":[{"service_id":"contest","revision_id":"rev-1","generation":1,"key":"contest.read","title":"Read"}],"user_frontend_modules":[{"service_id":"contest","deployment_id":"dep-1","revision_id":"rev-1","generation":1,"target":"user-shell","module_id":"contest.user","surface_id":"contest.list","route":"/contests","menu_label":"Contests","menu":true,"order":1,"permission":"contest.read","artifact":"bundle.js","host_api_range":"^1","manifest_digest":"sha256:` + strings.Repeat("b", 64) + `","manifest_reference":"https://artifacts.example/` + strings.Repeat("b", 64) + `/manifest.json","bundle_digest":"sha256:` + strings.Repeat("c", 64) + `","bundle_reference":"https://artifacts.example/` + strings.Repeat("c", 64) + `/bundle.js","enabled":true}],"admin_frontend_modules":[{"service_id":"contest","deployment_id":"dep-1","revision_id":"rev-1","generation":1,"target":"admin-shell","module_id":"contest.admin","surface_id":"contest.admin","route":"/admin/contests","menu_label":"Admin","menu":true,"order":1,"artifact":"bundle.js","host_api_range":"^1","manifest_digest":"sha256:` + strings.Repeat("d", 64) + `","manifest_reference":"https://artifacts.example/` + strings.Repeat("d", 64) + `/manifest.json","bundle_digest":"sha256:` + strings.Repeat("e", 64) + `","bundle_reference":"https://artifacts.example/` + strings.Repeat("e", 64) + `/bundle.js","enabled":true}]},"meta":{"api_version":"v1"}}`))
	}))
	defer orchestrator.Close()
	ctx := &svc.ServiceContext{Orchestrator: orchestratorsnapshot.NewClient(orchestrator.URL, "internal-token")}
	req := httptest.NewRequest(http.MethodGet, "/api/v1/contributions/snapshot", nil)
	req.Header.Set("Authorization", "Bearer browser-token")
	response := httptest.NewRecorder()
	contributionSnapshotHandler(ctx).ServeHTTP(response, req)
	if response.Code != http.StatusOK || !strings.Contains(response.Body.String(), "ojos.dev/contribution-snapshot/v1") {
		t.Fatalf("unexpected response status=%d body=%s", response.Code, response.Body.String())
	}
	var envelope struct {
		Data orchestratorsnapshot.ContributionSnapshot `json:"data"`
	}
	if err := json.Unmarshal(response.Body.Bytes(), &envelope); err != nil {
		t.Fatal(err)
	}
	if len(envelope.Data.Revisions) != 0 || len(envelope.Data.PermissionDefinitions) != 0 || len(envelope.Data.AdminFrontendModules) != 0 {
		t.Fatalf("control-plane or admin projection leaked to user Shell: %+v", envelope.Data)
	}
	if len(envelope.Data.GatewayRoutes) != 1 || envelope.Data.GatewayRoutes[0].OperationID != "listContests" || envelope.Data.GatewayRoutes[0].UpstreamBase != "" {
		t.Fatalf("user route projection was not sanitized: %+v", envelope.Data.GatewayRoutes)
	}
	if len(envelope.Data.UserFrontendModules) != 1 || envelope.Data.UserFrontendModules[0].SurfaceID != "contest.list" || envelope.Data.UserFrontendModules[0].MenuLabel != "Contests" {
		t.Fatalf("user frontend surface fields were lost: %+v", envelope.Data.UserFrontendModules)
	}
}
