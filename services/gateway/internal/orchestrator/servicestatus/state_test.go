package servicestatus

import (
	"context"
	"encoding/json"
	"net/http"
	"net/http/httptest"
	"testing"

	orchestratorsnapshot "ojos-gateway/internal/orchestrator/snapshot"
)

type fakeReader struct {
	services       []orchestratorsnapshot.Service
	permissions    []orchestratorsnapshot.Permission
	menus          []orchestratorsnapshot.Menu
	frontendRoutes []orchestratorsnapshot.FrontendRoute
	gatewayRoutes  []orchestratorsnapshot.GatewayRoute
	components     []orchestratorsnapshot.Component
	edges          []orchestratorsnapshot.Edge
}

func (f fakeReader) ListServices(context.Context) ([]orchestratorsnapshot.Service, error) {
	return f.services, nil
}
func (f fakeReader) ListPermissions(context.Context) ([]orchestratorsnapshot.Permission, error) {
	return f.permissions, nil
}
func (f fakeReader) ListMenus(context.Context) ([]orchestratorsnapshot.Menu, error) {
	return f.menus, nil
}
func (f fakeReader) ListFrontendRoutes(context.Context) ([]orchestratorsnapshot.FrontendRoute, error) {
	return f.frontendRoutes, nil
}
func (f fakeReader) ListGatewayRoutes(context.Context) ([]orchestratorsnapshot.GatewayRoute, error) {
	return f.gatewayRoutes, nil
}
func (f fakeReader) ListComponents(context.Context) ([]orchestratorsnapshot.Component, error) {
	return f.components, nil
}
func (f fakeReader) ListEdges(context.Context) ([]orchestratorsnapshot.Edge, error) {
	return f.edges, nil
}

func TestBuildSnapshotContainsServiceFirstBaseServices(t *testing.T) {
	reader := fakeReader{
		services: []orchestratorsnapshot.Service{
			{ServiceID: "ojos-orchestrator", Status: "ENABLED", Kind: orchestratorsnapshot.KindAgent, Name: "OJOS Orchestrator"},
			{ServiceID: "gateway", Status: "ENABLED", Kind: orchestratorsnapshot.KindGateway, Name: "Gateway"},
			{ServiceID: "judge-api", Status: "ENABLED", Kind: orchestratorsnapshot.KindBackendAPI, Name: "Judge API", Manifest: rawManifest(map[string]any{
				"provides": map[string]any{
					"topology": map[string]any{
						"nodes": []map[string]any{{"id": "judge-api", "type": "service", "label": "Judge API"}},
					},
				},
			})},
			{ServiceID: "ojos.disabled", Status: "DISABLED", Kind: orchestratorsnapshot.KindBackendAPI, Name: "Disabled"},
		},
		permissions: []orchestratorsnapshot.Permission{
			{ServiceID: "judge-api", PermissionKey: "judge.submit"},
			{ServiceID: "ojos.disabled", PermissionKey: "disabled.view"},
		},
		menus: []orchestratorsnapshot.Menu{
			{ServiceID: "judge-api", MenuKey: "problems", Enabled: true},
			{ServiceID: "ojos.disabled", MenuKey: "disabled", Enabled: true},
		},
		frontendRoutes: []orchestratorsnapshot.FrontendRoute{{
			ServiceID: "judge-api", RoutePath: "/problems", Enabled: true,
		}},
		gatewayRoutes: []orchestratorsnapshot.GatewayRoute{{
			ServiceID: "judge-api", Prefix: "/api/judge", Enabled: true,
		}},
		components: []orchestratorsnapshot.Component{
			{ServiceID: "judge-api", ComponentID: "judge-api", ComponentType: "backend_service", Status: "ENABLED"},
			{ServiceID: "judge-api", ComponentID: "judge-worker", ComponentType: "worker_service", Status: "ENABLED"},
			{ServiceID: "judge-api", ComponentID: "judge-health", ComponentType: "health_check", Status: "ENABLED"},
		},
		edges: []orchestratorsnapshot.Edge{{FromServiceID: "judge-api", ToServiceID: "ojos-orchestrator", EdgeType: "requires", Required: true}},
	}

	snapshot, err := BuildSnapshot(context.Background(), reader)
	if err != nil {
		t.Fatalf("BuildSnapshot failed: %v", err)
	}
	assertHasService(t, snapshot.ServiceDefinitions, "ojos-orchestrator")
	assertHasService(t, snapshot.ServiceDefinitions, "gateway")
	assertHasService(t, snapshot.ServiceDefinitions, "judge-api")
	if len(snapshot.ServiceDefinitions) != 3 {
		t.Fatalf("disabled services should not be snapshot-enabled services: %#v", snapshot.ServiceDefinitions)
	}
	if len(snapshot.Services) != 1 || len(snapshot.Workers) != 1 || len(snapshot.HealthChecks) != 1 {
		t.Fatalf("unexpected component grouping: services=%d workers=%d health=%d", len(snapshot.Services), len(snapshot.Workers), len(snapshot.HealthChecks))
	}
	if len(snapshot.Components) != 3 {
		t.Fatalf("orchestrator snapshot should retain active component surface")
	}
	if len(snapshot.Topology.ServiceDefinitions) != 3 || len(snapshot.Topology.DependencyEdges) != 1 {
		t.Fatalf("topology should retain active snapshot graph")
	}
	if hasPermission(snapshot.Permissions, "disabled.view") || hasMenu(snapshot.Menus, "disabled") {
		t.Fatalf("disabled service contributions should not appear in active snapshot")
	}
	if !hasTopologyNode(snapshot.Topology.Nodes, "judge-api:manifest:judge-api") {
		t.Fatalf("manifest topology node should enter service topology")
	}
	if !hasTopologyNode(snapshot.Topology.Nodes, "judge-api:service:judge-api") {
		t.Fatalf("Service Status definition should enter topology")
	}
	if !hasTopologyNode(snapshot.Topology.Nodes, "judge-worker:worker:judge-worker") {
		t.Fatalf("service worker state node should enter topology")
	}
}

func TestBuildSnapshotParsesManifestServicesAndWorkers(t *testing.T) {
	reader := fakeReader{
		services: []orchestratorsnapshot.Service{{
			ServiceID: "judge-api",
			Status:    "ENABLED",
			Kind:      orchestratorsnapshot.KindBackendAPI,
			Name:      "Judge API",
			Manifest: rawManifest(map[string]any{
				"provides": map[string]any{
					"services": []map[string]any{{
						"id":              "problem-api",
						"name":            "Problem API",
						"kind":            "http",
						"lifecycle":       "managed",
						"trusted_runtime": "compose",
						"compose_service": "problem-api",
						"health_check_id": "problem-api-health",
						"routes":          []string{"/api/problem"},
						"required":        true,
					}},
					"workers": []map[string]any{{
						"id":              "judge-worker",
						"name":            "Judge Worker",
						"kind":            "worker",
						"lifecycle":       "managed",
						"trusted_runtime": "compose",
						"compose_service": "judge-worker",
						"health_check_id": "worker-cluster-health",
						"required":        false,
					}},
				},
			}),
		}},
		gatewayRoutes: []orchestratorsnapshot.GatewayRoute{{
			ServiceID:     "judge-api",
			Prefix:        "/api/problem",
			TargetService: "problem-api",
			AuthMode:      "user",
			Enabled:       true,
		}},
	}

	snapshot, err := BuildSnapshot(context.Background(), reader)
	if err != nil {
		t.Fatalf("BuildSnapshot failed: %v", err)
	}
	if len(snapshot.Services) != 1 {
		t.Fatalf("expected one manifest service, got %#v", snapshot.Services)
	}
	service := snapshot.Services[0]
	if service.ServiceID != "problem-api" || service.Lifecycle != LifecycleManaged || service.Runtime != "compose" {
		t.Fatalf("unexpected service contract: %#v", service)
	}
	if !contains(service.Routes, "/api/problem") || service.HealthCheckID != "problem-api-health" || !service.Required {
		t.Fatalf("service routes/health/required not populated: %#v", service)
	}
	if len(snapshot.Workers) != 1 || snapshot.Workers[0].ServiceID != "judge-worker" {
		t.Fatalf("expected judge-worker manifest worker, got %#v", snapshot.Workers)
	}
}

func TestBuildSnapshotIncludeDisabledReturnsDisabledContributions(t *testing.T) {
	reader := fakeReader{
		services: []orchestratorsnapshot.Service{
			{ServiceID: "ojos-orchestrator", Status: "ENABLED", Kind: orchestratorsnapshot.KindAgent},
			{ServiceID: "demo-service", Status: "DISABLED", Kind: orchestratorsnapshot.KindBackendAPI},
		},
		permissions: []orchestratorsnapshot.Permission{{ServiceID: "demo-service", PermissionKey: "demo.view"}},
		menus:       []orchestratorsnapshot.Menu{{ServiceID: "demo-service", MenuKey: "demo", Enabled: false}},
		components:  []orchestratorsnapshot.Component{{ServiceID: "demo-service", ComponentID: "demo-health", ComponentType: "health_check", Status: "DISABLED"}},
	}
	active, err := BuildSnapshot(context.Background(), reader)
	if err != nil {
		t.Fatalf("BuildSnapshot failed: %v", err)
	}
	if hasService(active.ServiceDefinitions, "demo-service") || hasPermission(active.Permissions, "demo.view") {
		t.Fatalf("disabled demo service should not appear in active snapshot")
	}

	all, err := BuildSnapshotWithOptions(context.Background(), reader, BuildOptions{IncludeDisabled: true})
	if err != nil {
		t.Fatalf("BuildSnapshotWithOptions failed: %v", err)
	}
	assertHasService(t, all.ServiceDefinitions, "demo-service")
	if !hasPermission(all.Permissions, "demo.view") || !hasMenu(all.Menus, "demo") {
		t.Fatalf("include disabled snapshot should expose snapshot contributions")
	}
}

func TestBuildRouteTableDetectsPrefixConflicts(t *testing.T) {
	table := BuildRouteTableWithOptions(Snapshot{
		Version: "1",
		GatewayRoutes: []orchestratorsnapshot.GatewayRoute{
			{ServiceID: "a", Prefix: "/api/admin/services", TargetService: "a", AuthMode: "admin", Enabled: true},
			{ServiceID: "b", Prefix: "/api/admin/services/topology", TargetService: "b", AuthMode: "admin", Enabled: true},
			{ServiceID: "c", Prefix: "/api/problem", TargetService: "c", AuthMode: "required", Enabled: true},
		},
	}, RouteTableOptions{
		TrustedServices: map[string]TrustedService{
			"a": {ServiceID: "a", UpstreamBase: "http://a:8080"},
			"b": {ServiceID: "b", UpstreamBase: "http://b:8080"},
			"c": {ServiceID: "c", UpstreamBase: "http://c:8080"},
		},
	})
	if len(table.Routes) != 3 {
		t.Fatalf("expected 3 routes, got %d", len(table.Routes))
	}
	if len(table.Warnings) == 0 {
		t.Fatalf("expected prefix overlap warning")
	}
	if table.Routes[2].AuthMode != "user" {
		t.Fatalf("required auth mode should normalize to user, got %q", table.Routes[2].AuthMode)
	}
}

func TestBuildRouteTableBlocksReservedPrefixAndUnknownService(t *testing.T) {
	table := BuildRouteTableWithOptions(Snapshot{
		Version: "1",
		GatewayRoutes: []orchestratorsnapshot.GatewayRoute{
			{ServiceID: "a", Prefix: "/api/auth/shadow", TargetService: "known", AuthMode: "public", Enabled: true},
			{ServiceID: "b", Prefix: "/api/demo", TargetService: "missing", AuthMode: "user", Enabled: true},
			{ServiceID: "c", Prefix: "/api/ok", TargetService: "known", AuthMode: "user", Enabled: true},
			{ServiceID: "d", Prefix: "/api/disabled", TargetService: "known", AuthMode: "user", Enabled: false},
			{ServiceID: "e", Prefix: "/api/judge", TargetService: "known", AuthMode: "user", Enabled: true},
		},
	}, RouteTableOptions{
		TrustedServices: map[string]TrustedService{
			"known": {ServiceID: "known", UpstreamBase: "http://known:8080", StripPrefix: "/api"},
		},
	})

	routeByPrefix := map[string]ServiceRoute{}
	for _, route := range table.Routes {
		routeByPrefix[route.Prefix] = route
	}
	if routeByPrefix["/api/auth/shadow"].ProxyEnabled {
		t.Fatalf("reserved prefix route must not be proxy-enabled")
	}
	if !contains(routeByPrefix["/api/auth/shadow"].BlockedBy, "reserved prefix") {
		t.Fatalf("reserved prefix should be recorded in blocked_by: %#v", routeByPrefix["/api/auth/shadow"])
	}
	if routeByPrefix["/api/judge"].ProxyEnabled || !contains(routeByPrefix["/api/judge"].BlockedBy, "reserved prefix") {
		t.Fatalf("parent prefix must not cover reserved worker route: %#v", routeByPrefix["/api/judge"])
	}
	if routeByPrefix["/api/demo"].ProxyEnabled || !contains(routeByPrefix["/api/demo"].BlockedBy, "unknown trusted service") {
		t.Fatalf("unknown service should be blocked: %#v", routeByPrefix["/api/demo"])
	}
	if !routeByPrefix["/api/ok"].ProxyEnabled || routeByPrefix["/api/ok"].UpstreamBase == "" {
		t.Fatalf("trusted active route should be proxy-enabled with upstream: %#v", routeByPrefix["/api/ok"])
	}
	if routeByPrefix["/api/disabled"].ProxyEnabled {
		t.Fatalf("disabled route must not be proxy-enabled")
	}
}

func TestBuildRouteTableBlocksDuplicatePrefix(t *testing.T) {
	table := BuildRouteTableWithOptions(Snapshot{
		Version: "1",
		GatewayRoutes: []orchestratorsnapshot.GatewayRoute{
			{ServiceID: "a", Prefix: "/api/demo", TargetService: "svc", AuthMode: "user", Enabled: true},
			{ServiceID: "b", Prefix: "/api/demo", TargetService: "svc", AuthMode: "user", Enabled: true},
		},
	}, RouteTableOptions{
		TrustedServices: map[string]TrustedService{
			"svc": {ServiceID: "svc", UpstreamBase: "http://svc:8080"},
		},
	})
	for _, route := range table.Routes {
		if route.ProxyEnabled {
			t.Fatalf("duplicate route must not be proxy-enabled: %#v", route)
		}
		if !contains(route.BlockedBy, "duplicate prefix") {
			t.Fatalf("duplicate prefix should be blocked: %#v", route)
		}
	}
}

func TestBuildRouteTableBindsServiceHealth(t *testing.T) {
	table := BuildRouteTableWithOptions(Snapshot{
		Version: "1",
		GatewayRoutes: []orchestratorsnapshot.GatewayRoute{{
			ServiceID:     "judge-api",
			Prefix:        "/api/problem",
			TargetService: "problem-api",
			AuthMode:      "user",
			Enabled:       true,
		}},
	}, RouteTableOptions{
		TrustedServices: map[string]TrustedService{
			"problem-api": {ServiceID: "problem-api", UpstreamBase: "http://problem-api:8080"},
		},
		ServiceStatuses: map[string]ServiceStatus{
			"problem-api": {
				ServiceID: "problem-api",
				State:     ServiceStatusStopped,
				Health:    "error",
			},
		},
	})
	if len(table.Routes) != 1 {
		t.Fatalf("expected one route, got %d", len(table.Routes))
	}
	route := table.Routes[0]
	if route.ProxyEnabled || route.Status != "unavailable" {
		t.Fatalf("stopped service route should be unavailable and not proxied: %#v", route)
	}
	if route.ServiceStatus != ServiceStatusStopped || route.ServiceHealth != "error" {
		t.Fatalf("route should expose Service Status and health: %#v", route)
	}
	if !contains(route.BlockedBy, "service not running") {
		t.Fatalf("route should be blocked by Service Status: %#v", route)
	}
}

func TestComposeDriverPlansOnlyAllowedManagedServices(t *testing.T) {
	healthServer := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		w.WriteHeader(http.StatusNoContent)
	}))
	defer healthServer.Close()

	snapshot := Snapshot{
		Services: []ServiceStatus{
			{
				ServiceID:      "problem-api",
				OwnerServiceID: "judge-api",
				Kind:           "http",
				Lifecycle:      LifecycleManaged,
				Runtime:        "compose",
				ComposeService: "problem-api",
				Required:       true,
			},
			{
				ServiceID:      "demo-metadata-service",
				OwnerServiceID: "demo-service",
				Kind:           "metadata",
				Lifecycle:      LifecycleMetadata,
				Runtime:        "metadata",
			},
			{
				ServiceID:      "unsafe-service",
				OwnerServiceID: "demo-service",
				Kind:           "http",
				Lifecycle:      LifecycleManaged,
				Runtime:        "compose",
				ComposeService: "not-allowed",
			},
		},
		Workers: []ServiceStatus{{
			ServiceID:      "judge-worker",
			OwnerServiceID: "judge-api",
			Kind:           "worker",
			Lifecycle:      LifecycleManaged,
			Runtime:        "compose",
			ComposeService: "judge-worker",
		}},
	}
	driver := NewComposeDriver(map[string]TrustedService{
		"problem-api": {ServiceID: "problem-api", UpstreamBase: healthServer.URL},
	}, "problem-api", "judge-worker")

	services, err := driver.ListServices(context.Background(), snapshot)
	if err != nil {
		t.Fatalf("ListServices failed: %v", err)
	}
	if serviceStatusOf(services, "problem-api") != ServiceStatusRunning {
		t.Fatalf("problem-api should be running when health returns 204: %#v", services)
	}
	if serviceStatusOf(services, "judge-worker") != ServiceStatusUnknown {
		t.Fatalf("judge-worker should be unknown without HTTP health endpoint: %#v", services)
	}

	unsafe, err := driver.GetServiceStatus(context.Background(), snapshot, "unsafe-service")
	if err != nil {
		t.Fatalf("GetServiceStatus unsafe failed: %v", err)
	}
	if !contains(unsafe.BlockedBy, "service is not in trusted compose allowlist") {
		t.Fatalf("unknown compose service should be blocked in read-only state: %#v", unsafe)
	}
}

func assertHasService(t *testing.T, services []orchestratorsnapshot.Service, id string) {
	t.Helper()
	for _, service := range services {
		if service.ServiceID == id {
			return
		}
	}
	t.Fatalf("service %s not found in snapshot", id)
}

func hasService(services []orchestratorsnapshot.Service, id string) bool {
	for _, service := range services {
		if service.ServiceID == id {
			return true
		}
	}
	return false
}

func hasPermission(items []orchestratorsnapshot.Permission, key string) bool {
	for _, item := range items {
		if item.PermissionKey == key {
			return true
		}
	}
	return false
}

func hasMenu(items []orchestratorsnapshot.Menu, key string) bool {
	for _, item := range items {
		if item.MenuKey == key {
			return true
		}
	}
	return false
}

func hasTopologyNode(items []ServiceTopologyNode, id string) bool {
	for _, item := range items {
		if item.ID == id {
			return true
		}
	}
	return false
}

func contains(items []string, want string) bool {
	for _, item := range items {
		if item == want {
			return true
		}
	}
	return false
}

func serviceStatusOf(items []ServiceStatus, serviceID string) string {
	for _, item := range items {
		if item.ServiceID == serviceID {
			return item.State
		}
	}
	return ""
}

func rawManifest(value map[string]any) json.RawMessage {
	data, err := json.Marshal(value)
	if err != nil {
		panic(err)
	}
	return data
}
