package moduleruntime

import (
	"context"
	"encoding/json"
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
	table := BuildRouteTable(Snapshot{
		Version: "1",
		GatewayRoutes: []moduleregistry.GatewayRoute{
			{ModuleID: "a", Prefix: "/api/admin/modules", TargetService: "a", AuthMode: "admin", Enabled: true},
			{ModuleID: "b", Prefix: "/api/admin/modules/topology", TargetService: "b", AuthMode: "admin", Enabled: true},
			{ModuleID: "c", Prefix: "/api/problem", TargetService: "c", AuthMode: "required", Enabled: true},
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

func rawManifest(value map[string]any) json.RawMessage {
	data, err := json.Marshal(value)
	if err != nil {
		panic(err)
	}
	return data
}
