package moduleruntime

import (
	"context"
	"encoding/json"
	"net/http"
	"net/http/httptest"
	"testing"

	"ojos-gateway/internal/moduleregistry"
)

type fakeReader struct {
	modules        []moduleregistry.Module
	permissions    []moduleregistry.Permission
	menus          []moduleregistry.Menu
	frontendRoutes []moduleregistry.FrontendRoute
	gatewayRoutes  []moduleregistry.GatewayRoute
	components     []moduleregistry.Component
	edges          []moduleregistry.Edge
}

func (f fakeReader) ListModules(context.Context) ([]moduleregistry.Module, error) {
	return f.modules, nil
}
func (f fakeReader) ListPermissions(context.Context) ([]moduleregistry.Permission, error) {
	return f.permissions, nil
}
func (f fakeReader) ListMenus(context.Context) ([]moduleregistry.Menu, error) {
	return f.menus, nil
}
func (f fakeReader) ListFrontendRoutes(context.Context) ([]moduleregistry.FrontendRoute, error) {
	return f.frontendRoutes, nil
}
func (f fakeReader) ListGatewayRoutes(context.Context) ([]moduleregistry.GatewayRoute, error) {
	return f.gatewayRoutes, nil
}
func (f fakeReader) ListComponents(context.Context) ([]moduleregistry.Component, error) {
	return f.components, nil
}
func (f fakeReader) ListEdges(context.Context) ([]moduleregistry.Edge, error) {
	return f.edges, nil
}

func TestBuildSnapshotContainsKernelPlatformAndJudgeCore(t *testing.T) {
	reader := fakeReader{
		modules: []moduleregistry.Module{
			{ModuleID: "ojos.kernel.installer", Status: "ENABLED", Kind: "kernel", Name: "Installer"},
			{ModuleID: "ojos.platform.gateway", Status: "ENABLED", Kind: "platform", Name: "Gateway"},
			{ModuleID: "ojos.judge-core", Status: "ENABLED", Kind: "feature", Name: "Judge Core", Manifest: rawManifest(map[string]any{
				"provides": map[string]any{
					"topology": map[string]any{
						"nodes": []map[string]any{{"id": "judge-api", "type": "service", "label": "Judge API"}},
					},
				},
			})},
			{ModuleID: "ojos.disabled", Status: "DISABLED", Kind: "feature", Name: "Disabled"},
		},
		permissions: []moduleregistry.Permission{
			{ModuleID: "ojos.judge-core", PermissionKey: "judge.submit"},
			{ModuleID: "ojos.disabled", PermissionKey: "disabled.view"},
		},
		menus: []moduleregistry.Menu{
			{ModuleID: "ojos.judge-core", MenuKey: "problems", Enabled: true},
			{ModuleID: "ojos.disabled", MenuKey: "disabled", Enabled: true},
		},
		frontendRoutes: []moduleregistry.FrontendRoute{{
			ModuleID: "ojos.judge-core", RoutePath: "/problems", Enabled: true,
		}},
		gatewayRoutes: []moduleregistry.GatewayRoute{{
			ModuleID: "ojos.judge-core", Prefix: "/api/judge", Enabled: true,
		}},
		components: []moduleregistry.Component{
			{ModuleID: "ojos.judge-core", ComponentID: "judge-api", ComponentType: "backend_service", Status: "ENABLED"},
			{ModuleID: "ojos.judge-core", ComponentID: "judge-worker", ComponentType: "worker_service", Status: "ENABLED"},
			{ModuleID: "ojos.judge-core", ComponentID: "judge-health", ComponentType: "health_check", Status: "ENABLED"},
		},
		edges: []moduleregistry.Edge{{FromModuleID: "ojos.judge-core", ToModuleID: "ojos.kernel.installer", EdgeType: "requires", Required: true}},
	}

	snapshot, err := BuildSnapshot(context.Background(), reader)
	if err != nil {
		t.Fatalf("BuildSnapshot failed: %v", err)
	}
	assertHasModule(t, snapshot.Modules, "ojos.kernel.installer")
	assertHasModule(t, snapshot.Modules, "ojos.platform.gateway")
	assertHasModule(t, snapshot.Modules, "ojos.judge-core")
	if len(snapshot.Modules) != 3 {
		t.Fatalf("disabled modules should not be runtime-enabled modules: %#v", snapshot.Modules)
	}
	if len(snapshot.Services) != 1 || len(snapshot.Workers) != 1 || len(snapshot.HealthChecks) != 1 {
		t.Fatalf("unexpected component grouping: services=%d workers=%d health=%d", len(snapshot.Services), len(snapshot.Workers), len(snapshot.HealthChecks))
	}
	if len(snapshot.Components) != 3 {
		t.Fatalf("runtime snapshot should retain active component surface")
	}
	if len(snapshot.Topology.ModuleNodes) != 3 || len(snapshot.Topology.DependencyEdges) != 1 {
		t.Fatalf("topology should retain active registry graph")
	}
	if hasPermission(snapshot.Permissions, "disabled.view") || hasMenu(snapshot.Menus, "disabled") {
		t.Fatalf("disabled module contributions should not appear in active snapshot")
	}
	if !hasTopologyNode(snapshot.Topology.Nodes, "ojos.judge-core:manifest:judge-api") {
		t.Fatalf("manifest topology node should enter runtime topology")
	}
	if !hasTopologyNode(snapshot.Topology.Nodes, "ojos.judge-core:service:judge-api") {
		t.Fatalf("runtime service node should enter topology")
	}
	if !hasTopologyNode(snapshot.Topology.Nodes, "ojos.judge-core:worker:judge-worker") {
		t.Fatalf("runtime worker node should enter topology")
	}
}

func TestBuildSnapshotParsesManifestServicesAndWorkers(t *testing.T) {
	reader := fakeReader{
		modules: []moduleregistry.Module{{
			ModuleID: "ojos.judge-core",
			Status:   "ENABLED",
			Kind:     "feature",
			Name:     "Judge Core",
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
		gatewayRoutes: []moduleregistry.GatewayRoute{{
			ModuleID:      "ojos.judge-core",
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
		modules: []moduleregistry.Module{
			{ModuleID: "ojos.kernel.installer", Status: "ENABLED", Kind: "kernel"},
			{ModuleID: "ojos.demo-module", Status: "DISABLED", Kind: "feature"},
		},
		permissions: []moduleregistry.Permission{{ModuleID: "ojos.demo-module", PermissionKey: "demo.view"}},
		menus:       []moduleregistry.Menu{{ModuleID: "ojos.demo-module", MenuKey: "demo", Enabled: false}},
		components:  []moduleregistry.Component{{ModuleID: "ojos.demo-module", ComponentID: "demo-health", ComponentType: "health_check", Status: "DISABLED"}},
	}
	active, err := BuildSnapshot(context.Background(), reader)
	if err != nil {
		t.Fatalf("BuildSnapshot failed: %v", err)
	}
	if hasModule(active.Modules, "ojos.demo-module") || hasPermission(active.Permissions, "demo.view") {
		t.Fatalf("disabled demo module should not appear in active snapshot")
	}

	all, err := BuildSnapshotWithOptions(context.Background(), reader, BuildOptions{IncludeDisabled: true})
	if err != nil {
		t.Fatalf("BuildSnapshotWithOptions failed: %v", err)
	}
	assertHasModule(t, all.Modules, "ojos.demo-module")
	if !hasPermission(all.Permissions, "demo.view") || !hasMenu(all.Menus, "demo") {
		t.Fatalf("include disabled snapshot should expose registry contributions")
	}
}

func TestBuildRouteTableDetectsPrefixConflicts(t *testing.T) {
	table := BuildRouteTableWithOptions(Snapshot{
		Version: "1",
		GatewayRoutes: []moduleregistry.GatewayRoute{
			{ModuleID: "a", Prefix: "/api/admin/modules", TargetService: "a", AuthMode: "admin", Enabled: true},
			{ModuleID: "b", Prefix: "/api/admin/modules/topology", TargetService: "b", AuthMode: "admin", Enabled: true},
			{ModuleID: "c", Prefix: "/api/problem", TargetService: "c", AuthMode: "required", Enabled: true},
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
		GatewayRoutes: []moduleregistry.GatewayRoute{
			{ModuleID: "a", Prefix: "/api/auth/shadow", TargetService: "known", AuthMode: "public", Enabled: true},
			{ModuleID: "b", Prefix: "/api/demo", TargetService: "missing", AuthMode: "user", Enabled: true},
			{ModuleID: "c", Prefix: "/api/ok", TargetService: "known", AuthMode: "user", Enabled: true},
			{ModuleID: "d", Prefix: "/api/disabled", TargetService: "known", AuthMode: "user", Enabled: false},
			{ModuleID: "e", Prefix: "/api/judge", TargetService: "known", AuthMode: "user", Enabled: true},
		},
	}, RouteTableOptions{
		TrustedServices: map[string]TrustedService{
			"known": {ServiceID: "known", UpstreamBase: "http://known:8080", StripPrefix: "/api"},
		},
	})

	routeByPrefix := map[string]RuntimeRoute{}
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
		GatewayRoutes: []moduleregistry.GatewayRoute{
			{ModuleID: "a", Prefix: "/api/demo", TargetService: "svc", AuthMode: "user", Enabled: true},
			{ModuleID: "b", Prefix: "/api/demo", TargetService: "svc", AuthMode: "user", Enabled: true},
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
		GatewayRoutes: []moduleregistry.GatewayRoute{{
			ModuleID:      "ojos.judge-core",
			Prefix:        "/api/problem",
			TargetService: "problem-api",
			AuthMode:      "user",
			Enabled:       true,
		}},
	}, RouteTableOptions{
		TrustedServices: map[string]TrustedService{
			"problem-api": {ServiceID: "problem-api", UpstreamBase: "http://problem-api:8080"},
		},
		ServiceStates: map[string]RuntimeService{
			"problem-api": {
				ServiceID: "problem-api",
				State:     ServiceStateStopped,
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
	if route.ServiceState != ServiceStateStopped || route.ServiceHealth != "error" {
		t.Fatalf("route should expose service state and health: %#v", route)
	}
	if !contains(route.BlockedBy, "service not running") {
		t.Fatalf("route should be blocked by service state: %#v", route)
	}
}

func TestComposeDriverPlansOnlyAllowedManagedServices(t *testing.T) {
	healthServer := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		w.WriteHeader(http.StatusNoContent)
	}))
	defer healthServer.Close()

	snapshot := Snapshot{
		Services: []RuntimeService{
			{
				ServiceID:      "problem-api",
				ModuleID:       "ojos.judge-core",
				Kind:           "http",
				Lifecycle:      LifecycleManaged,
				Runtime:        "compose",
				ComposeService: "problem-api",
				Required:       true,
			},
			{
				ServiceID: "demo-metadata-service",
				ModuleID:  "ojos.demo-module",
				Kind:      "metadata",
				Lifecycle: LifecycleMetadata,
				Runtime:   "metadata",
			},
			{
				ServiceID:      "unsafe-service",
				ModuleID:       "ojos.demo-module",
				Kind:           "http",
				Lifecycle:      LifecycleManaged,
				Runtime:        "compose",
				ComposeService: "not-allowed",
			},
		},
		Workers: []RuntimeService{{
			ServiceID:      "judge-worker",
			ModuleID:       "ojos.judge-core",
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
	if serviceState(services, "problem-api") != ServiceStateRunning {
		t.Fatalf("problem-api should be running when health returns 204: %#v", services)
	}
	if serviceState(services, "judge-worker") != ServiceStateUnknown {
		t.Fatalf("judge-worker should be unknown without HTTP health endpoint: %#v", services)
	}

	plan, err := driver.PlanRestart(context.Background(), snapshot, "problem-api")
	if err != nil {
		t.Fatalf("PlanRestart failed: %v", err)
	}
	if plan.CanApply || len(plan.BlockedBy) != 0 {
		t.Fatalf("plan should be valid but apply-disabled: %#v", plan)
	}
	if len(plan.Commands) != 1 || plan.Commands[0].Tool != "compose" || !contains(plan.Commands[0].Args, "problem-api") {
		t.Fatalf("plan command should be structured compose args: %#v", plan.Commands)
	}

	metadataPlan, err := driver.PlanStart(context.Background(), snapshot, "demo-metadata-service")
	if err != nil {
		t.Fatalf("PlanStart metadata failed: %v", err)
	}
	if !contains(metadataPlan.BlockedBy, "metadata lifecycle cannot start") {
		t.Fatalf("metadata lifecycle must block start: %#v", metadataPlan)
	}

	unsafePlan, err := driver.PlanStart(context.Background(), snapshot, "unsafe-service")
	if err != nil {
		t.Fatalf("PlanStart unsafe failed: %v", err)
	}
	if !contains(unsafePlan.BlockedBy, "service is not in trusted compose allowlist") {
		t.Fatalf("unknown compose service should be blocked: %#v", unsafePlan)
	}
}

func assertHasModule(t *testing.T, modules []moduleregistry.Module, id string) {
	t.Helper()
	for _, module := range modules {
		if module.ModuleID == id {
			return
		}
	}
	t.Fatalf("module %s not found in snapshot", id)
}

func hasModule(modules []moduleregistry.Module, id string) bool {
	for _, module := range modules {
		if module.ModuleID == id {
			return true
		}
	}
	return false
}

func hasPermission(items []moduleregistry.Permission, key string) bool {
	for _, item := range items {
		if item.PermissionKey == key {
			return true
		}
	}
	return false
}

func hasMenu(items []moduleregistry.Menu, key string) bool {
	for _, item := range items {
		if item.MenuKey == key {
			return true
		}
	}
	return false
}

func hasTopologyNode(items []RuntimeTopologyNode, id string) bool {
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

func serviceState(items []RuntimeService, serviceID string) string {
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
