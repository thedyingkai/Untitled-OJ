package logic

import (
	"context"
	"errors"
	"fmt"
	"strings"
	"testing"

	"ojos-gateway/internal/config"
	"ojos-gateway/internal/kernel/moduleruntime"
	"ojos-gateway/internal/moduleregistry"
	"ojos-gateway/internal/svc"
	"ojos-gateway/internal/types"
	sharedjwt "ojos-shared/security/jwt"
)

type fakeModuleRegistry struct {
	data moduleregistry.BootstrapData
}

type fakeRuntimeDriver struct {
	services []moduleruntime.RuntimeService
}

func (f fakeRuntimeDriver) ListServices(context.Context, moduleruntime.Snapshot) ([]moduleruntime.RuntimeService, error) {
	return f.services, nil
}

func (f fakeRuntimeDriver) GetServiceState(_ context.Context, _ moduleruntime.Snapshot, serviceID string) (moduleruntime.RuntimeService, error) {
	for _, service := range f.services {
		if service.ServiceID == serviceID {
			return service, nil
		}
	}
	return moduleruntime.RuntimeService{}, errors.New("not found: runtime service")
}

func (f fakeRuntimeDriver) PlanStart(ctx context.Context, snapshot moduleruntime.Snapshot, serviceID string) (moduleruntime.RuntimePlan, error) {
	return f.plan(ctx, snapshot, serviceID, "start")
}

func (f fakeRuntimeDriver) PlanStop(ctx context.Context, snapshot moduleruntime.Snapshot, serviceID string) (moduleruntime.RuntimePlan, error) {
	return f.plan(ctx, snapshot, serviceID, "stop")
}

func (f fakeRuntimeDriver) PlanRestart(ctx context.Context, snapshot moduleruntime.Snapshot, serviceID string) (moduleruntime.RuntimePlan, error) {
	return f.plan(ctx, snapshot, serviceID, "restart")
}

func (f fakeRuntimeDriver) PlanReload(ctx context.Context, snapshot moduleruntime.Snapshot, serviceID string) (moduleruntime.RuntimePlan, error) {
	return f.plan(ctx, snapshot, serviceID, "reload")
}

func (f fakeRuntimeDriver) PlanHealth(ctx context.Context, snapshot moduleruntime.Snapshot, serviceID string) (moduleruntime.RuntimePlan, error) {
	return f.plan(ctx, snapshot, serviceID, "health")
}

func (f fakeRuntimeDriver) ApplyPlan(context.Context, moduleruntime.RuntimePlan) (moduleruntime.RuntimePlanResult, error) {
	return moduleruntime.RuntimePlanResult{}, errors.New("not implemented")
}

func (f fakeRuntimeDriver) plan(ctx context.Context, snapshot moduleruntime.Snapshot, serviceID string, action string) (moduleruntime.RuntimePlan, error) {
	service, _ := f.GetServiceState(ctx, snapshot, serviceID)
	plan := moduleruntime.RuntimePlan{
		PlanID:               fmt.Sprintf("test-%s-%s", action, serviceID),
		OperationID:          fmt.Sprintf("test-%s-%s-op", action, serviceID),
		Action:               action,
		ServiceID:            serviceID,
		ModuleID:             service.ModuleID,
		Driver:               "compose",
		CanApply:             false,
		ApplyEnabled:         false,
		RequiresConfirmation: true,
		AllowedTargets:       []string{service.ComposeService},
		Affected:             []string{serviceID},
		Warnings:             []string{"Gateway/Web apply disabled"},
		CreatedAt:            "2026-01-01T00:00:00Z",
		ExpiresAt:            "2026-01-01T00:05:00Z",
	}
	if service.Lifecycle == moduleruntime.LifecycleMetadata {
		plan.BlockedBy = append(plan.BlockedBy, "metadata lifecycle cannot "+action)
		return plan, nil
	}
	plan.Commands = []moduleruntime.RuntimePlanCommand{{Kind: "compose", Argv: []string{"docker", "compose", action, service.ComposeService}}}
	plan.CanApply = true
	return plan, nil
}

func (f fakeModuleRegistry) ListModules(context.Context) ([]moduleregistry.Module, error) {
	return f.data.Modules, nil
}

func (f fakeModuleRegistry) ListSets(context.Context) ([]moduleregistry.Set, error) {
	return f.data.Sets, nil
}

func (f fakeModuleRegistry) Topology(context.Context) (moduleregistry.Topology, error) {
	return moduleregistry.Topology{
		Sets:       f.data.Sets,
		Nodes:      f.data.Modules,
		Edges:      f.data.Edges,
		Components: f.data.Components,
	}, nil
}

func (f fakeModuleRegistry) ListPermissions(context.Context) ([]moduleregistry.Permission, error) {
	return f.data.Permissions, nil
}

func (f fakeModuleRegistry) ListMenus(context.Context) ([]moduleregistry.Menu, error) {
	return f.data.Menus, nil
}

func (f fakeModuleRegistry) ListFrontendRoutes(context.Context) ([]moduleregistry.FrontendRoute, error) {
	return f.data.FrontendRoutes, nil
}

func (f fakeModuleRegistry) ListGatewayRoutes(context.Context) ([]moduleregistry.GatewayRoute, error) {
	return f.data.GatewayRoutes, nil
}

func (f fakeModuleRegistry) ListComponents(context.Context) ([]moduleregistry.Component, error) {
	return f.data.Components, nil
}

func (f fakeModuleRegistry) ListEdges(context.Context) ([]moduleregistry.Edge, error) {
	return f.data.Edges, nil
}

func (f fakeModuleRegistry) Detail(_ context.Context, moduleID string) (moduleregistry.Detail, error) {
	var detail moduleregistry.Detail
	for _, module := range f.data.Modules {
		if module.ModuleID == moduleID {
			detail.Module = module
			break
		}
	}
	if detail.Module.ModuleID == "" {
		return moduleregistry.Detail{}, errors.New("module not found")
	}
	for _, edge := range f.data.Edges {
		if edge.FromModuleID == moduleID {
			detail.Dependencies = append(detail.Dependencies, edge)
		}
		if edge.ToModuleID == moduleID {
			detail.Dependents = append(detail.Dependents, edge)
		}
	}
	for _, component := range f.data.Components {
		if component.ModuleID == moduleID {
			detail.Components = append(detail.Components, component)
			if component.ComponentType == "health_check" {
				detail.HealthChecks = append(detail.HealthChecks, component)
			}
		}
	}
	for _, permission := range f.data.Permissions {
		if permission.ModuleID == moduleID {
			detail.Permissions = append(detail.Permissions, permission)
		}
	}
	for _, menu := range f.data.Menus {
		if menu.ModuleID == moduleID {
			detail.Menus = append(detail.Menus, menu)
		}
	}
	for _, route := range f.data.FrontendRoutes {
		if route.ModuleID == moduleID {
			detail.FrontendRoutes = append(detail.FrontendRoutes, route)
		}
	}
	for _, route := range f.data.GatewayRoutes {
		if route.ModuleID == moduleID {
			detail.GatewayRoutes = append(detail.GatewayRoutes, route)
		}
	}
	for _, installation := range f.data.Installations {
		if installation.ModuleID == moduleID {
			detail.Installations = append(detail.Installations, installation)
		}
	}
	return detail, nil
}

func TestListModulesRejectsOrdinaryUser(t *testing.T) {
	oldChecker := hasSystemAdminPermission
	hasSystemAdminPermission = func(context.Context, *svc.ServiceContext, int64) (bool, error) {
		return false, nil
	}
	defer func() {
		hasSystemAdminPermission = oldChecker
	}()

	token, err := sharedjwt.Generate("test-secret", 1001, "alice", []string{"user"}, 1)
	if err != nil {
		t.Fatal(err)
	}

	logic := NewAdminModulesLogic(context.Background(), &svc.ServiceContext{
		Config: config.Config{
			Jwt: config.JwtConfig{Secret: "test-secret"},
		},
	})
	_, err = logic.ListModules("Bearer " + token)
	if err == nil || !strings.Contains(err.Error(), "forbidden") {
		t.Fatalf("expected forbidden error, got %v", err)
	}
}

func TestTopologyReturnsBuiltinRegistryData(t *testing.T) {
	ctx := context.Background()
	token, err := sharedjwt.Generate("test-secret", 1, "root", []string{"admin"}, 1)
	if err != nil {
		t.Fatal(err)
	}

	logic := &AdminModulesLogic{
		ctx: ctx,
		svcCtx: &svc.ServiceContext{
			Config: config.Config{
				Jwt: config.JwtConfig{Secret: "test-secret"},
			},
		},
		repo: fakeModuleRegistry{data: moduleregistry.BuiltinData()},
	}

	resp, err := logic.Topology("Bearer " + token)
	if err != nil {
		t.Fatalf("topology failed: %v", err)
	}
	if len(resp.Sets) == 0 {
		t.Fatalf("topology sets should be non-empty")
	}
	if len(resp.Nodes) == 0 {
		t.Fatalf("topology nodes should be non-empty")
	}
	if len(resp.Edges) == 0 {
		t.Fatalf("topology edges should be non-empty")
	}
	if len(resp.Components) == 0 {
		t.Fatalf("topology components should be non-empty")
	}
	if !hasSet(resp.Sets, "runtime") || !hasSet(resp.Sets, "core-capability") {
		t.Fatalf("topology should include runtime and core-capability sets: %#v", resp.Sets)
	}
	if !hasNode(resp.ModuleNodes, "ojos.judge-core") {
		t.Fatalf("topology should include ojos.judge-core node")
	}
	for _, edge := range [][2]string{
		{"ojos.judge-core", "ojos.platform.web-shell"},
		{"ojos.judge-core", "ojos.platform.identity-access"},
		{"ojos.judge-core", "ojos.kernel.module-runtime"},
	} {
		if !hasEdge(resp.DependencyEdges, edge[0], edge[1]) {
			t.Fatalf("topology should include edge %s -> %s", edge[0], edge[1])
		}
	}
	for _, componentID := range []string{"problem-api", "judge-api", "judge-worker", "frontend-routes", "gateway-routes", "permissions"} {
		if !hasComponent(resp.Components, "ojos.judge-core", componentID) {
			t.Fatalf("topology should include component %s", componentID)
		}
	}
}

func TestRuntimeSnapshotReturnsKernelPlatformAndJudgeCore(t *testing.T) {
	ctx := context.Background()
	token, err := sharedjwt.Generate("test-secret", 1, "root", []string{"admin"}, 1)
	if err != nil {
		t.Fatal(err)
	}

	logic := &AdminModulesLogic{
		ctx: ctx,
		svcCtx: &svc.ServiceContext{
			Config: config.Config{
				Jwt: config.JwtConfig{Secret: "test-secret"},
			},
		},
		repo: fakeModuleRegistry{data: moduleregistry.BuiltinData()},
	}

	resp, err := logic.RuntimeSnapshot("Bearer "+token, false)
	if err != nil {
		t.Fatalf("runtime snapshot failed: %v", err)
	}
	for _, moduleID := range []string{"ojos.kernel.installer", "ojos.kernel.module-runtime", "ojos.platform.gateway", "ojos.platform.web-shell", "ojos.judge-core"} {
		if !hasNode(resp.Modules, moduleID) {
			t.Fatalf("runtime snapshot should include %s", moduleID)
		}
	}
	if len(resp.Topology.Nodes) == 0 || len(resp.Topology.Edges) == 0 {
		t.Fatalf("runtime snapshot topology should be non-empty")
	}
	if len(resp.Topology.ModuleNodes) == 0 || len(resp.Topology.DependencyEdges) == 0 {
		t.Fatalf("runtime snapshot should expose module graph compatibility fields")
	}
	if len(resp.Services) == 0 || len(resp.Workers) == 0 || len(resp.HealthChecks) == 0 {
		t.Fatalf("runtime snapshot should include services/workers/health checks")
	}
	if len(resp.Components) == 0 {
		t.Fatalf("runtime snapshot should include full component list")
	}
}

func TestRuntimeSnapshotIncludeDisabledControlsActiveContributions(t *testing.T) {
	ctx := context.Background()
	token, err := sharedjwt.Generate("test-secret", 1, "root", []string{"admin"}, 1)
	if err != nil {
		t.Fatal(err)
	}

	data := moduleregistry.BuiltinData()
	data.Modules = append(data.Modules, moduleregistry.Module{
		ModuleID: "ojos.demo-module",
		SetID:    "demo",
		Name:     "Demo Module",
		Version:  "0.1.0",
		Status:   "DISABLED",
		Kind:     "feature",
	})
	data.Permissions = append(data.Permissions, moduleregistry.Permission{
		ModuleID:      "ojos.demo-module",
		PermissionKey: "demo.view",
		Description:   "View demo module metadata.",
	})
	data.Menus = append(data.Menus, moduleregistry.Menu{
		ModuleID:  "ojos.demo-module",
		MenuKey:   "demo-module",
		Title:     "Demo Module",
		RoutePath: "/admin/modules/demo",
		Enabled:   false,
	})

	logic := &AdminModulesLogic{
		ctx: ctx,
		svcCtx: &svc.ServiceContext{
			Config: config.Config{
				Jwt: config.JwtConfig{Secret: "test-secret"},
			},
		},
		repo: fakeModuleRegistry{data: data},
	}

	active, err := logic.RuntimeSnapshot("Bearer "+token, false)
	if err != nil {
		t.Fatalf("active runtime snapshot failed: %v", err)
	}
	if hasNode(active.Modules, "ojos.demo-module") || hasPermissionItem(active.Permissions, "demo.view") {
		t.Fatalf("disabled demo module should not appear in active runtime snapshot")
	}

	all, err := logic.RuntimeSnapshot("Bearer "+token, true)
	if err != nil {
		t.Fatalf("include-disabled runtime snapshot failed: %v", err)
	}
	if !hasNode(all.Modules, "ojos.demo-module") || !hasPermissionItem(all.Permissions, "demo.view") {
		t.Fatalf("include-disabled runtime snapshot should expose disabled demo registry entries")
	}
}

func TestRuntimeRoutesReturnsRegistryRouteTable(t *testing.T) {
	ctx := context.Background()
	token, err := sharedjwt.Generate("test-secret", 1, "root", []string{"admin"}, 1)
	if err != nil {
		t.Fatal(err)
	}

	data := moduleregistry.BuiltinData()
	data.GatewayRoutes = append(data.GatewayRoutes, moduleregistry.GatewayRoute{
		ModuleID:      "ojos.demo-module",
		Prefix:        "/api/demo",
		TargetService: "demo-api",
		AuthMode:      "admin",
		Enabled:       false,
	})
	data.Modules = append(data.Modules, moduleregistry.Module{
		ModuleID: "ojos.demo-module",
		SetID:    "demo",
		Name:     "Demo Module",
		Version:  "0.1.0",
		Status:   "DISABLED",
		Kind:     "feature",
	})

	logic := &AdminModulesLogic{
		ctx: ctx,
		svcCtx: &svc.ServiceContext{
			Config: config.Config{
				Jwt: config.JwtConfig{Secret: "test-secret"},
			},
			RouteTableOptions: testRouteTableOptions(),
		},
		repo: fakeModuleRegistry{data: data},
	}

	active, err := logic.RuntimeRoutes("Bearer "+token, false, false, false)
	if err != nil {
		t.Fatalf("runtime routes failed: %v", err)
	}
	if hasRuntimeRoute(active.Routes, "/api/demo") {
		t.Fatalf("disabled demo route should not appear in active runtime route table")
	}
	all, err := logic.RuntimeRoutes("Bearer "+token, true, true, false)
	if err != nil {
		t.Fatalf("include-disabled runtime routes failed: %v", err)
	}
	if !all.Reloaded {
		t.Fatalf("reload response should be marked reloaded")
	}
	if !hasRuntimeRoute(all.Routes, "/api/demo") {
		t.Fatalf("include-disabled route table should expose demo metadata route")
	}
	for _, route := range all.Routes {
		if route.UpstreamBase != "" {
			t.Fatalf("admin route table should not expose upstream_base by default: %#v", route)
		}
	}
	debug, err := logic.RuntimeRoutes("Bearer "+token, true, false, true)
	if err != nil {
		t.Fatalf("debug runtime routes failed: %v", err)
	}
	if !hasRuntimeRouteUpstream(debug.Routes, "/api/demo") {
		t.Fatalf("debug runtime routes should expose upstream for trusted route")
	}
}

func TestRuntimeServicesAdminAPIUsesRuntimeDriver(t *testing.T) {
	ctx := context.Background()
	token, err := sharedjwt.Generate("test-secret", 1, "root", []string{"admin"}, 1)
	if err != nil {
		t.Fatal(err)
	}
	logic := &AdminRuntimeLogic{
		ctx: ctx,
		svcCtx: &svc.ServiceContext{
			Config: config.Config{Jwt: config.JwtConfig{Secret: "test-secret"}},
			RuntimeDriver: fakeRuntimeDriver{services: []moduleruntime.RuntimeService{
				{
					ServiceID:      "problem-api",
					ModuleID:       "ojos.judge-core",
					Kind:           "http",
					Lifecycle:      moduleruntime.LifecycleManaged,
					Runtime:        "compose",
					ComposeService: "problem-api",
					State:          moduleruntime.ServiceStateRunning,
					Health:         "ok",
					Routes:         []string{"/api/problem"},
					Required:       true,
				},
				{
					ServiceID: "demo-metadata-service",
					ModuleID:  "ojos.demo-module",
					Kind:      "metadata",
					Lifecycle: moduleruntime.LifecycleMetadata,
					Runtime:   "metadata",
					State:     moduleruntime.ServiceStateDeclared,
					Health:    "metadata",
				},
				{
					ServiceID: "judge-worker",
					ModuleID:  "ojos.judge-core",
					Kind:      "worker",
					Lifecycle: moduleruntime.LifecycleManaged,
					Runtime:   "compose",
					State:     moduleruntime.ServiceStateUnknown,
					Health:    "unknown",
				},
			}},
		},
		repo: fakeModuleRegistry{data: moduleregistry.BuiltinData()},
	}

	resp, err := logic.ListServices("Bearer " + token)
	if err != nil {
		t.Fatalf("runtime services failed: %v", err)
	}
	if len(resp.Services) != 2 || len(resp.Workers) != 1 {
		t.Fatalf("unexpected runtime service grouping: %#v", resp)
	}
	detail, err := logic.ServiceDetail("Bearer "+token, "problem-api")
	if err != nil {
		t.Fatalf("runtime service detail failed: %v", err)
	}
	if detail.Service.ServiceId != "problem-api" || detail.Service.State != moduleruntime.ServiceStateRunning {
		t.Fatalf("unexpected service detail: %#v", detail)
	}
	plan, err := logic.PlanRestart("Bearer "+token, "problem-api")
	if err != nil {
		t.Fatalf("plan restart failed: %v", err)
	}
	if !plan.Plan.CanApply || len(plan.Plan.Commands) != 1 || plan.Plan.Commands[0].Kind != "compose" {
		t.Fatalf("plan should be operator-applyable compose plan while Gateway apply remains disabled: %#v", plan)
	}
	metadataPlan, err := logic.PlanStart("Bearer "+token, "demo-metadata-service")
	if err != nil {
		t.Fatalf("metadata plan start failed: %v", err)
	}
	if !containsString(metadataPlan.Plan.BlockedBy, "metadata lifecycle cannot start") {
		t.Fatalf("metadata service should block start: %#v", metadataPlan)
	}
}

func TestRuntimeServicesRejectOrdinaryUser(t *testing.T) {
	oldChecker := hasSystemAdminPermission
	hasSystemAdminPermission = func(context.Context, *svc.ServiceContext, int64) (bool, error) {
		return false, nil
	}
	defer func() {
		hasSystemAdminPermission = oldChecker
	}()

	token, err := sharedjwt.Generate("test-secret", 1001, "alice", []string{"user"}, 1)
	if err != nil {
		t.Fatal(err)
	}
	logic := &AdminRuntimeLogic{
		ctx: context.Background(),
		svcCtx: &svc.ServiceContext{
			Config: config.Config{Jwt: config.JwtConfig{Secret: "test-secret"}},
		},
		repo: fakeModuleRegistry{data: moduleregistry.BuiltinData()},
	}

	_, err = logic.ListServices("Bearer " + token)
	if err == nil || !strings.Contains(err.Error(), "forbidden") {
		t.Fatalf("expected forbidden error, got %v", err)
	}
}

func TestIsAdminRole(t *testing.T) {
	if !isAdminRole([]string{"user", "admin"}) {
		t.Fatalf("admin role should be accepted")
	}
	if !isAdminRole([]string{"super_admin"}) {
		t.Fatalf("super_admin role should be accepted")
	}
	if isAdminRole([]string{"user"}) {
		t.Fatalf("ordinary user role should not be accepted")
	}
}

func hasSet(items []types.ModuleSetItem, setID string) bool {
	for _, item := range items {
		if item.SetId == setID {
			return true
		}
	}
	return false
}

func hasNode(items []types.ModuleNodeItem, moduleID string) bool {
	for _, item := range items {
		if item.ModuleId == moduleID {
			return true
		}
	}
	return false
}

func hasPermissionItem(items []types.ModulePermissionItem, key string) bool {
	for _, item := range items {
		if item.PermissionKey == key {
			return true
		}
	}
	return false
}

func hasRuntimeRoute(items []types.ModuleRuntimeRouteItem, prefix string) bool {
	for _, item := range items {
		if item.Prefix == prefix {
			return true
		}
	}
	return false
}

func hasRuntimeRouteUpstream(items []types.ModuleRuntimeRouteItem, prefix string) bool {
	for _, item := range items {
		if item.Prefix == prefix && item.UpstreamBase != "" {
			return true
		}
	}
	return false
}

func containsString(items []string, want string) bool {
	for _, item := range items {
		if item == want {
			return true
		}
	}
	return false
}

func hasEdge(items []types.ModuleEdgeItem, from string, to string) bool {
	for _, item := range items {
		if item.FromModuleId == from && item.ToModuleId == to {
			return true
		}
	}
	return false
}

func hasComponent(items []types.ModuleComponentItem, moduleID string, componentID string) bool {
	for _, item := range items {
		if item.ModuleId == moduleID && item.ComponentId == componentID {
			return true
		}
	}
	return false
}

func testRouteTableOptions() moduleruntime.RouteTableOptions {
	return moduleruntime.RouteTableOptions{
		TrustedServices: map[string]moduleruntime.TrustedService{
			"demo-api":    {ServiceID: "demo-api", UpstreamBase: "http://demo-api:8080"},
			"problem-api": {ServiceID: "problem-api", UpstreamBase: "http://problem-api:8083"},
			"judge-api":   {ServiceID: "judge-api", UpstreamBase: "http://judge-api:8082"},
		},
	}
}
