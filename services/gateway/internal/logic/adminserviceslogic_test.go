package logic

import (
	"context"
	"errors"
	"fmt"
	"strings"
	"testing"

	"ojos-gateway/internal/config"
	"ojos-gateway/internal/kernel/serviceruntime"
	"ojos-gateway/internal/serviceregistry"
	"ojos-gateway/internal/svc"
	"ojos-gateway/internal/types"
	sharedjwt "ojos-shared/security/jwt"
)

type fakeServiceRegistry struct {
	data serviceregistry.BootstrapData
}

type fakeRuntimeDriver struct {
	services []serviceruntime.RuntimeService
}

func (f fakeRuntimeDriver) ListServices(context.Context, serviceruntime.Snapshot) ([]serviceruntime.RuntimeService, error) {
	return f.services, nil
}

func (f fakeRuntimeDriver) GetServiceState(_ context.Context, _ serviceruntime.Snapshot, serviceID string) (serviceruntime.RuntimeService, error) {
	for _, service := range f.services {
		if service.ServiceID == serviceID {
			return service, nil
		}
	}
	return serviceruntime.RuntimeService{}, errors.New("not found: runtime service")
}

func (f fakeRuntimeDriver) PlanStart(ctx context.Context, snapshot serviceruntime.Snapshot, serviceID string) (serviceruntime.RuntimePlan, error) {
	return f.plan(ctx, snapshot, serviceID, "start")
}

func (f fakeRuntimeDriver) PlanStop(ctx context.Context, snapshot serviceruntime.Snapshot, serviceID string) (serviceruntime.RuntimePlan, error) {
	return f.plan(ctx, snapshot, serviceID, "stop")
}

func (f fakeRuntimeDriver) PlanRestart(ctx context.Context, snapshot serviceruntime.Snapshot, serviceID string) (serviceruntime.RuntimePlan, error) {
	return f.plan(ctx, snapshot, serviceID, "restart")
}

func (f fakeRuntimeDriver) PlanReload(ctx context.Context, snapshot serviceruntime.Snapshot, serviceID string) (serviceruntime.RuntimePlan, error) {
	return f.plan(ctx, snapshot, serviceID, "reload")
}

func (f fakeRuntimeDriver) PlanHealth(ctx context.Context, snapshot serviceruntime.Snapshot, serviceID string) (serviceruntime.RuntimePlan, error) {
	return f.plan(ctx, snapshot, serviceID, "health")
}

func (f fakeRuntimeDriver) ApplyPlan(context.Context, serviceruntime.RuntimePlan) (serviceruntime.RuntimePlanResult, error) {
	return serviceruntime.RuntimePlanResult{}, errors.New("not implemented")
}

func (f fakeRuntimeDriver) plan(ctx context.Context, snapshot serviceruntime.Snapshot, serviceID string, action string) (serviceruntime.RuntimePlan, error) {
	service, _ := f.GetServiceState(ctx, snapshot, serviceID)
	plan := serviceruntime.RuntimePlan{
		PlanID:               fmt.Sprintf("test-%s-%s", action, serviceID),
		OperationID:          fmt.Sprintf("test-%s-%s-op", action, serviceID),
		Action:               action,
		ServiceID:            serviceID,
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
	if service.Lifecycle == serviceruntime.LifecycleMetadata {
		plan.BlockedBy = append(plan.BlockedBy, "metadata lifecycle cannot "+action)
		return plan, nil
	}
	plan.Commands = []serviceruntime.RuntimePlanCommand{{Kind: "compose", Argv: []string{"docker", "compose", action, service.ComposeService}}}
	plan.CanApply = true
	return plan, nil
}

func (f fakeServiceRegistry) ListServices(context.Context) ([]serviceregistry.Service, error) {
	return f.data.Services, nil
}

func (f fakeServiceRegistry) ListSets(context.Context) ([]serviceregistry.Set, error) {
	return f.data.Sets, nil
}

func (f fakeServiceRegistry) Topology(context.Context) (serviceregistry.Topology, error) {
	return serviceregistry.Topology{
		Sets:       f.data.Sets,
		Nodes:      f.data.Services,
		Edges:      f.data.Edges,
		Components: f.data.Components,
	}, nil
}

func (f fakeServiceRegistry) ListPermissions(context.Context) ([]serviceregistry.Permission, error) {
	return f.data.Permissions, nil
}

func (f fakeServiceRegistry) ListMenus(context.Context) ([]serviceregistry.Menu, error) {
	return f.data.Menus, nil
}

func (f fakeServiceRegistry) ListFrontendRoutes(context.Context) ([]serviceregistry.FrontendRoute, error) {
	return f.data.FrontendRoutes, nil
}

func (f fakeServiceRegistry) ListGatewayRoutes(context.Context) ([]serviceregistry.GatewayRoute, error) {
	return f.data.GatewayRoutes, nil
}

func (f fakeServiceRegistry) ListComponents(context.Context) ([]serviceregistry.Component, error) {
	return f.data.Components, nil
}

func (f fakeServiceRegistry) ListEdges(context.Context) ([]serviceregistry.Edge, error) {
	return f.data.Edges, nil
}

func (f fakeServiceRegistry) Detail(_ context.Context, serviceID string) (serviceregistry.Detail, error) {
	var detail serviceregistry.Detail
	for _, service := range f.data.Services {
		if service.ServiceID == serviceID {
			detail.Service = service
			break
		}
	}
	if detail.Service.ServiceID == "" {
		return serviceregistry.Detail{}, errors.New("service not found")
	}
	for _, edge := range f.data.Edges {
		if edge.FromServiceID == serviceID {
			detail.Dependencies = append(detail.Dependencies, edge)
		}
		if edge.ToServiceID == serviceID {
			detail.Dependents = append(detail.Dependents, edge)
		}
	}
	for _, component := range f.data.Components {
		if component.ServiceID == serviceID {
			detail.Components = append(detail.Components, component)
			if component.ComponentType == "health_check" {
				detail.HealthChecks = append(detail.HealthChecks, component)
			}
		}
	}
	for _, permission := range f.data.Permissions {
		if permission.ServiceID == serviceID {
			detail.Permissions = append(detail.Permissions, permission)
		}
	}
	for _, menu := range f.data.Menus {
		if menu.ServiceID == serviceID {
			detail.Menus = append(detail.Menus, menu)
		}
	}
	for _, route := range f.data.FrontendRoutes {
		if route.ServiceID == serviceID {
			detail.FrontendRoutes = append(detail.FrontendRoutes, route)
		}
	}
	for _, route := range f.data.GatewayRoutes {
		if route.ServiceID == serviceID {
			detail.GatewayRoutes = append(detail.GatewayRoutes, route)
		}
	}
	for _, installation := range f.data.Installations {
		if installation.ServiceID == serviceID {
			detail.Installations = append(detail.Installations, installation)
		}
	}
	return detail, nil
}

func TestListServicesRejectsOrdinaryUser(t *testing.T) {
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

	logic := NewAdminServicesLogic(context.Background(), &svc.ServiceContext{
		Config: config.Config{
			Jwt: config.JwtConfig{Secret: "test-secret"},
		},
	})
	_, err = logic.ListServices("Bearer " + token)
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

	logic := &AdminServicesLogic{
		ctx: ctx,
		svcCtx: &svc.ServiceContext{
			Config: config.Config{
				Jwt: config.JwtConfig{Secret: "test-secret"},
			},
		},
		repo: fakeServiceRegistry{data: serviceregistry.BuiltinData()},
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
	if !hasSet(resp.Sets, "single-node-oj") || !hasSet(resp.Sets, "distributed-root") {
		t.Fatalf("topology should include runtime and single-node-oj sets: %#v", resp.Sets)
	}
	if !hasNode(resp.ServiceNodes, "judge-api") {
		t.Fatalf("topology should include judge-api node")
	}
	for _, edge := range [][2]string{
		{"gateway", "problem-api"},
		{"gateway", "judge-api"},
		{"judge-worker", "judge-api"},
	} {
		if !hasEdge(resp.DependencyEdges, edge[0], edge[1]) {
			t.Fatalf("topology should include edge %s -> %s", edge[0], edge[1])
		}
	}
	for _, componentID := range []string{"problem-api-endpoint", "problem-api-health"} {
		if !hasComponent(resp.Components, "problem-api", componentID) {
			t.Fatalf("topology should include component %s", componentID)
		}
	}
}

func TestRuntimeSnapshotReturnsServiceFirstBaseServices(t *testing.T) {
	ctx := context.Background()
	token, err := sharedjwt.Generate("test-secret", 1, "root", []string{"admin"}, 1)
	if err != nil {
		t.Fatal(err)
	}

	logic := &AdminServicesLogic{
		ctx: ctx,
		svcCtx: &svc.ServiceContext{
			Config: config.Config{
				Jwt: config.JwtConfig{Secret: "test-secret"},
			},
		},
		repo: fakeServiceRegistry{data: serviceregistry.BuiltinData()},
	}

	resp, err := logic.RuntimeSnapshot("Bearer "+token, false)
	if err != nil {
		t.Fatalf("runtime snapshot failed: %v", err)
	}
	for _, serviceID := range []string{"root-runtime-manager", "root-runtime-manager", "gateway", "web-shell", "judge-api"} {
		if !hasNode(resp.ServiceNodes, serviceID) {
			t.Fatalf("runtime snapshot should include %s", serviceID)
		}
	}
	if len(resp.Topology.Nodes) == 0 || len(resp.Topology.Edges) == 0 {
		t.Fatalf("runtime snapshot topology should be non-empty")
	}
	if len(resp.Topology.ServiceNodes) == 0 || len(resp.Topology.DependencyEdges) == 0 {
		t.Fatalf("runtime snapshot should expose service graph compatibility fields")
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

	data := serviceregistry.BuiltinData()
	data.Services = append(data.Services, serviceregistry.Service{
		ServiceID: "demo-service",
		SetID:     "demo",
		Name:      "Demo Service",
		Version:   "0.1.0",
		Status:    "DISABLED",
		Kind:      "feature",
	})
	data.Permissions = append(data.Permissions, serviceregistry.Permission{
		ServiceID:     "demo-service",
		PermissionKey: "demo.view",
		Description:   "View demo service metadata.",
	})
	data.Menus = append(data.Menus, serviceregistry.Menu{
		ServiceID: "demo-service",
		MenuKey:   "demo-service",
		Title:     "Demo Service",
		RoutePath: "/admin/services/demo",
		Enabled:   false,
	})

	logic := &AdminServicesLogic{
		ctx: ctx,
		svcCtx: &svc.ServiceContext{
			Config: config.Config{
				Jwt: config.JwtConfig{Secret: "test-secret"},
			},
		},
		repo: fakeServiceRegistry{data: data},
	}

	active, err := logic.RuntimeSnapshot("Bearer "+token, false)
	if err != nil {
		t.Fatalf("active runtime snapshot failed: %v", err)
	}
	if hasNode(active.ServiceNodes, "demo-service") || hasPermissionItem(active.Permissions, "demo.view") {
		t.Fatalf("disabled demo service should not appear in active runtime snapshot")
	}

	all, err := logic.RuntimeSnapshot("Bearer "+token, true)
	if err != nil {
		t.Fatalf("include-disabled runtime snapshot failed: %v", err)
	}
	if !hasNode(all.ServiceNodes, "demo-service") || !hasPermissionItem(all.Permissions, "demo.view") {
		t.Fatalf("include-disabled runtime snapshot should expose disabled demo registry entries")
	}
}

func TestRuntimeRoutesReturnsRegistryRouteTable(t *testing.T) {
	ctx := context.Background()
	token, err := sharedjwt.Generate("test-secret", 1, "root", []string{"admin"}, 1)
	if err != nil {
		t.Fatal(err)
	}

	data := serviceregistry.BuiltinData()
	data.GatewayRoutes = append(data.GatewayRoutes, serviceregistry.GatewayRoute{
		ServiceID:     "demo-service",
		Prefix:        "/api/demo",
		TargetService: "demo-api",
		AuthMode:      "admin",
		Enabled:       false,
	})
	data.Services = append(data.Services, serviceregistry.Service{
		ServiceID: "demo-service",
		SetID:     "demo",
		Name:      "Demo Service",
		Version:   "0.1.0",
		Status:    "DISABLED",
		Kind:      "feature",
	})

	logic := &AdminServicesLogic{
		ctx: ctx,
		svcCtx: &svc.ServiceContext{
			Config: config.Config{
				Jwt: config.JwtConfig{Secret: "test-secret"},
			},
			RouteTableOptions: testRouteTableOptions(),
		},
		repo: fakeServiceRegistry{data: data},
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
			RuntimeDriver: fakeRuntimeDriver{services: []serviceruntime.RuntimeService{
				{
					ServiceID:      "problem-api",
					OwnerServiceID: "judge-api",
					Kind:           "http",
					Lifecycle:      serviceruntime.LifecycleManaged,
					Runtime:        "compose",
					ComposeService: "problem-api",
					State:          serviceruntime.ServiceStateRunning,
					Health:         "ok",
					Routes:         []string{"/api/problem"},
					Required:       true,
				},
				{
					ServiceID:      "demo-metadata-service",
					OwnerServiceID: "demo-service",
					Kind:           "metadata",
					Lifecycle:      serviceruntime.LifecycleMetadata,
					Runtime:        "metadata",
					State:          serviceruntime.ServiceStateDeclared,
					Health:         "metadata",
				},
				{
					ServiceID:      "judge-worker",
					OwnerServiceID: "judge-api",
					Kind:           "worker",
					Lifecycle:      serviceruntime.LifecycleManaged,
					Runtime:        "compose",
					State:          serviceruntime.ServiceStateUnknown,
					Health:         "unknown",
				},
			}},
		},
		repo: fakeServiceRegistry{data: serviceregistry.BuiltinData()},
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
	if detail.Service.ServiceId != "problem-api" || detail.Service.State != serviceruntime.ServiceStateRunning {
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
		repo: fakeServiceRegistry{data: serviceregistry.BuiltinData()},
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

func hasSet(items []types.ServiceSetItem, setID string) bool {
	for _, item := range items {
		if item.SetId == setID {
			return true
		}
	}
	return false
}

func hasNode(items []types.ServiceNodeItem, serviceID string) bool {
	for _, item := range items {
		if item.ServiceId == serviceID {
			return true
		}
	}
	return false
}

func hasPermissionItem(items []types.ServicePermissionItem, key string) bool {
	for _, item := range items {
		if item.PermissionKey == key {
			return true
		}
	}
	return false
}

func hasRuntimeRoute(items []types.ServiceRuntimeRouteItem, prefix string) bool {
	for _, item := range items {
		if item.Prefix == prefix {
			return true
		}
	}
	return false
}

func hasRuntimeRouteUpstream(items []types.ServiceRuntimeRouteItem, prefix string) bool {
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

func hasEdge(items []types.ServiceEdgeItem, from string, to string) bool {
	for _, item := range items {
		if item.FromServiceId == from && item.ToServiceId == to {
			return true
		}
	}
	return false
}

func hasComponent(items []types.ServiceComponentItem, serviceID string, componentID string) bool {
	for _, item := range items {
		if item.ServiceId == serviceID && item.ComponentId == componentID {
			return true
		}
	}
	return false
}

func testRouteTableOptions() serviceruntime.RouteTableOptions {
	return serviceruntime.RouteTableOptions{
		TrustedServices: map[string]serviceruntime.TrustedService{
			"demo-api":    {ServiceID: "demo-api", UpstreamBase: "http://demo-api:8080"},
			"problem-api": {ServiceID: "problem-api", UpstreamBase: "http://problem-api:8083"},
			"judge-api":   {ServiceID: "judge-api", UpstreamBase: "http://judge-api:8082"},
		},
	}
}
