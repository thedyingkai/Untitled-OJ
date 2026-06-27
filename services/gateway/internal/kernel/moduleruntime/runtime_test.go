package moduleruntime

import (
	"context"
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
			{ModuleID: "ojos.kernel.installer", Status: "ENABLED", Kind: "kernel"},
			{ModuleID: "ojos.platform.gateway", Status: "ENABLED", Kind: "platform"},
			{ModuleID: "ojos.judge-core", Status: "ENABLED", Kind: "feature"},
			{ModuleID: "ojos.disabled", Status: "DISABLED", Kind: "feature"},
		},
		permissions: []moduleregistry.Permission{{ModuleID: "ojos.judge-core", PermissionKey: "judge.submit"}},
		menus:       []moduleregistry.Menu{{ModuleID: "ojos.judge-core", MenuKey: "problems", Enabled: true}},
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
		t.Fatalf("runtime snapshot should retain full component surface")
	}
	if len(snapshot.Topology.Nodes) != 4 || len(snapshot.Topology.Edges) != 1 {
		t.Fatalf("topology should retain full registry graph")
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
