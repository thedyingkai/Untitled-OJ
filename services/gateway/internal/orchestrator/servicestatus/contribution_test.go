package servicestatus

import (
	"strings"
	"testing"

	orchestratorsnapshot "ojos-gateway/internal/orchestrator/snapshot"
)

func contributionRoute(operationID, method, path string) orchestratorsnapshot.ContributionGatewayRoute {
	return orchestratorsnapshot.ContributionGatewayRoute{
		ServiceID: "contest-service", DeploymentID: "dep-1", RevisionID: "rev-1",
		Generation: 2, Audience: "user", Method: method, Path: path,
		ApiID: "contest.api", OperationID: operationID,
		ProviderPath: strings.TrimPrefix(path, "/api"), Auth: "REQUIRED",
		Permission: "contest.read", UpstreamBase: "http://contest:8080", Enabled: true,
	}
}

func TestContributionRouteTableRejectsConflictingActiveOperations(t *testing.T) {
	first := contributionRoute("getContest", "GET", "/api/contests/{contestId}")
	second := contributionRoute("getContestAlias", "GET", "/api/contests/{id}")
	second.RevisionID = "rev-2"
	_, err := ContributionRouteTable(orchestratorsnapshot.ContributionSnapshot{
		SchemaVersion: "ojos.dev/contribution-snapshot/v1",
		GatewayRoutes: []orchestratorsnapshot.ContributionGatewayRoute{first, second},
	})
	if err == nil || !strings.Contains(err.Error(), "collide") {
		t.Fatalf("expected deterministic route collision, got %v", err)
	}
}

func TestContributionRouteTableAllowsSameTemplateForDifferentMethods(t *testing.T) {
	get := contributionRoute("getContest", "GET", "/api/contests/{contestId}")
	put := contributionRoute("updateContest", "PUT", "/api/contests/{contestId}")
	table, err := ContributionRouteTable(orchestratorsnapshot.ContributionSnapshot{
		Digest:        "sha256:routes",
		GatewayRoutes: []orchestratorsnapshot.ContributionGatewayRoute{get, put},
	})
	if err != nil {
		t.Fatal(err)
	}
	if len(table.Routes) != 2 || !table.Routes[0].ProxyEnabled || !table.Routes[1].ProxyEnabled {
		t.Fatalf("expected both method-isolated routes to activate: %#v", table.Routes)
	}
}

func TestContributionRouteTableDisablesUnhealthyRuntime(t *testing.T) {
	route := contributionRoute("getContest", "GET", "/api/contests/{contestId}")
	route.Enabled = false
	route.UpstreamBase = ""
	table, err := ContributionRouteTable(orchestratorsnapshot.ContributionSnapshot{GatewayRoutes: []orchestratorsnapshot.ContributionGatewayRoute{route}})
	if err != nil {
		t.Fatal(err)
	}
	if len(table.Routes) != 1 || table.Routes[0].ProxyEnabled || table.Routes[0].Status == "active" {
		t.Fatalf("unhealthy route must remain disabled: %#v", table.Routes)
	}
}

func TestContributionRouteTableRejectsPermissionScopeOutsideTemplates(t *testing.T) {
	route := contributionRoute("getContest", "GET", "/api/contests/{contestId}")
	route.PermissionScope = &orchestratorsnapshot.PermissionScope{Kind: "path_parameter", Type: "contest", PathParameter: "queryId"}
	table, err := ContributionRouteTable(orchestratorsnapshot.ContributionSnapshot{GatewayRoutes: []orchestratorsnapshot.ContributionGatewayRoute{route}})
	if err != nil {
		t.Fatal(err)
	}
	if len(table.Routes) != 1 || table.Routes[0].ProxyEnabled || !strings.Contains(strings.Join(table.Routes[0].BlockedBy, ";"), "invalid permission scope") {
		t.Fatalf("invalid permission scope must be blocked: %#v", table.Routes)
	}
}

func TestContributionRouteTableBlocksGatewaySelfProxyAndPlatformPrefixes(t *testing.T) {
	self := contributionRoute("getGatewayHealth", "GET", "/api/gateway/health")
	self.ServiceID = "gateway"
	reserved := contributionRoute("shadowReady", "GET", "/readyz")
	table, err := ContributionRouteTable(orchestratorsnapshot.ContributionSnapshot{GatewayRoutes: []orchestratorsnapshot.ContributionGatewayRoute{self, reserved}})
	if err != nil {
		t.Fatal(err)
	}
	if len(table.Routes) != 2 {
		t.Fatalf("route count=%d", len(table.Routes))
	}
	for _, route := range table.Routes {
		if route.ProxyEnabled || route.Status != "blocked" {
			t.Fatalf("self/reserved route became proxyable: %+v", route)
		}
	}
	if !strings.Contains(strings.Join(table.Routes[0].BlockedBy, ";"), "cannot contribute proxy routes") {
		t.Fatalf("Gateway self-proxy block is not explicit: %+v", table.Routes[0])
	}
	if !strings.Contains(strings.Join(table.Routes[1].BlockedBy, ";"), "reserved prefix") {
		t.Fatalf("platform prefix was not reserved: %+v", table.Routes[1])
	}
}
