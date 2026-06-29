package logic

import (
	"context"
	"errors"
	"strings"
	"testing"

	"ojos-gateway/internal/config"
	"ojos-gateway/internal/orchestrator/servicestatus"
	orchestratorsnapshot "ojos-gateway/internal/orchestrator/snapshot"
	"ojos-gateway/internal/svc"
	"ojos-gateway/internal/types"
	sharedjwt "ojos-shared/security/jwt"
)

type fakeOrchestratorSnapshot struct {
	data orchestratorsnapshot.SnapshotData
}

type fakeServiceStatusDriver struct {
	services []servicestatus.ServiceStatus
}

func testSnapshotData() orchestratorsnapshot.SnapshotData {
	return orchestratorsnapshot.SnapshotData{
		Sets: []orchestratorsnapshot.Set{
			{SetID: "single-node-oj", Name: "单机 OJ", Description: "单机部署组合", SortOrder: 10},
			{SetID: "distributed-oj", Name: "分布式 OJ", Description: "分布式入口组合", SortOrder: 20},
		},
		Services: []orchestratorsnapshot.Service{
			{ServiceID: "gateway", SetID: "single-node-oj", Name: "Gateway", Version: "0.1.0", Status: orchestratorsnapshot.StatusEnabled, Kind: "gateway"},
			{ServiceID: "web-shell", SetID: "single-node-oj", Name: "Web Shell", Version: "0.1.0", Status: orchestratorsnapshot.StatusEnabled, Kind: "frontend"},
			{ServiceID: "problem-api", SetID: "single-node-oj", Name: "Problem API", Version: "0.1.0", Status: orchestratorsnapshot.StatusEnabled, Kind: "backend-api"},
			{ServiceID: "judge-api", SetID: "single-node-oj", Name: "Judge API", Version: "0.1.0", Status: orchestratorsnapshot.StatusEnabled, Kind: "backend-api"},
			{ServiceID: "judge-worker", SetID: "single-node-oj", Name: "Judge Worker", Version: "0.1.0", Status: orchestratorsnapshot.StatusEnabled, Kind: "backend-worker"},
			{ServiceID: "postgres", SetID: "single-node-oj", Name: "PostgreSQL", Version: "17.0.0", Status: orchestratorsnapshot.StatusEnabled, Kind: "database"},
			{ServiceID: "storage", SetID: "single-node-oj", Name: "Storage", Version: "0.1.0", Status: orchestratorsnapshot.StatusEnabled, Kind: "storage"},
		},
		Edges: []orchestratorsnapshot.Edge{
			{FromServiceID: "gateway", ToServiceID: "problem-api", EdgeType: "link", Required: true},
			{FromServiceID: "gateway", ToServiceID: "judge-api", EdgeType: "link", Required: true},
			{FromServiceID: "gateway", ToServiceID: "web-shell", EdgeType: "link", Required: true},
			{FromServiceID: "judge-worker", ToServiceID: "judge-api", EdgeType: "link", Required: true},
			{FromServiceID: "judge-api", ToServiceID: "postgres", EdgeType: "link", Required: true},
			{FromServiceID: "judge-api", ToServiceID: "storage", EdgeType: "link", Required: true},
		},
		Components: []orchestratorsnapshot.Component{
			{ServiceID: "gateway", ComponentID: "gateway-endpoint", ComponentType: "endpoint", Status: orchestratorsnapshot.StatusEnabled, Config: []byte(`{"endpoint":"127.0.0.1:8080","protocol":"http"}`)},
			{ServiceID: "gateway", ComponentID: "gateway-health", ComponentType: "health_check", Status: orchestratorsnapshot.StatusEnabled, Config: []byte(`{"type":"http","target":"/health"}`)},
			{ServiceID: "problem-api", ComponentID: "problem-api-endpoint", ComponentType: "endpoint", Status: orchestratorsnapshot.StatusEnabled, Config: []byte(`{"endpoint":"127.0.0.1:8083","protocol":"http"}`)},
			{ServiceID: "problem-api", ComponentID: "problem-api", ComponentType: "backend_service", Status: orchestratorsnapshot.StatusEnabled, Config: []byte(`{"service":"problem-api","trusted_runtime":"compose","compose_service":"problem-api","health_check_id":"problem-api-health","routes":["/api/problem"],"required":true}`)},
			{ServiceID: "problem-api", ComponentID: "problem-api-health", ComponentType: "health_check", Status: orchestratorsnapshot.StatusEnabled, Config: []byte(`{"type":"http","target":"/health"}`)},
			{ServiceID: "judge-api", ComponentID: "judge-api-endpoint", ComponentType: "endpoint", Status: orchestratorsnapshot.StatusEnabled, Config: []byte(`{"endpoint":"127.0.0.1:8082","protocol":"http"}`)},
			{ServiceID: "judge-api", ComponentID: "judge-api", ComponentType: "backend_service", Status: orchestratorsnapshot.StatusEnabled, Config: []byte(`{"service":"judge-api","trusted_runtime":"compose","compose_service":"judge-api","health_check_id":"judge-api-health","routes":["/api/judge"],"required":true}`)},
			{ServiceID: "judge-worker", ComponentID: "judge-worker-endpoint", ComponentType: "endpoint", Status: orchestratorsnapshot.StatusEnabled, Config: []byte(`{"endpoint":"127.0.0.1:9101","protocol":"http"}`)},
			{ServiceID: "judge-worker", ComponentID: "judge-worker", ComponentType: "worker_service", Status: orchestratorsnapshot.StatusEnabled, Config: []byte(`{"service":"judge-worker","trusted_runtime":"compose","compose_service":"judge-worker","required":true}`)},
		},
		Endpoints: []orchestratorsnapshot.Endpoint{
			{Endpoint: "127.0.0.1:8080", ServiceID: "gateway", Protocol: "http", HealthPath: "/health", Health: "ok", Reachable: true},
			{Endpoint: "127.0.0.1:8083", ServiceID: "problem-api", Protocol: "http", HealthPath: "/health", Health: "ok", Reachable: true},
			{Endpoint: "127.0.0.1:8082", ServiceID: "judge-api", Protocol: "http", HealthPath: "/health", Health: "ok", Reachable: true},
			{Endpoint: "127.0.0.1:9101", ServiceID: "judge-worker", Protocol: "http", HealthPath: "/health", Health: "ok", Reachable: true},
		},
		Permissions: []orchestratorsnapshot.Permission{
			{ServiceID: "problem-api", PermissionKey: "problem.read", Description: "读取题目"},
			{ServiceID: "judge-api", PermissionKey: "judge.submit", Description: "提交评测"},
		},
		Menus: []orchestratorsnapshot.Menu{
			{ServiceID: "web-shell", MenuKey: "problems", Title: "题库", RoutePath: "/problems", Enabled: true},
			{ServiceID: "web-shell", MenuKey: "submissions", Title: "提交", RoutePath: "/submissions", Enabled: true},
		},
		FrontendRoutes: []orchestratorsnapshot.FrontendRoute{
			{ServiceID: "web-shell", RoutePath: "/problems", RouteName: "problem-list", ComponentKey: "ProblemList", Enabled: true},
		},
		GatewayRoutes: []orchestratorsnapshot.GatewayRoute{
			{ServiceID: "problem-api", Prefix: "/api/problem", TargetService: "problem-api", AuthMode: "required", Enabled: true},
			{ServiceID: "judge-api", Prefix: "/api/judge", TargetService: "judge-api", AuthMode: "required", Enabled: true},
		},
	}
}

func (f fakeServiceStatusDriver) ListServices(context.Context, servicestatus.Snapshot) ([]servicestatus.ServiceStatus, error) {
	return f.services, nil
}

func (f fakeServiceStatusDriver) GetServiceStatus(_ context.Context, _ servicestatus.Snapshot, serviceID string) (servicestatus.ServiceStatus, error) {
	for _, service := range f.services {
		if service.ServiceID == serviceID {
			return service, nil
		}
	}
	return servicestatus.ServiceStatus{}, errors.New("not found: Service Status")
}

func (f fakeOrchestratorSnapshot) ListServices(context.Context) ([]orchestratorsnapshot.Service, error) {
	return f.data.Services, nil
}

func (f fakeOrchestratorSnapshot) ListSets(context.Context) ([]orchestratorsnapshot.Set, error) {
	return f.data.Sets, nil
}

func (f fakeOrchestratorSnapshot) Topology(context.Context) (orchestratorsnapshot.Topology, error) {
	return orchestratorsnapshot.Topology{
		Sets:       f.data.Sets,
		Nodes:      f.data.Services,
		Edges:      f.data.Edges,
		Components: f.data.Components,
	}, nil
}

func (f fakeOrchestratorSnapshot) ListPermissions(context.Context) ([]orchestratorsnapshot.Permission, error) {
	return f.data.Permissions, nil
}

func (f fakeOrchestratorSnapshot) ListMenus(context.Context) ([]orchestratorsnapshot.Menu, error) {
	return f.data.Menus, nil
}

func (f fakeOrchestratorSnapshot) ListFrontendRoutes(context.Context) ([]orchestratorsnapshot.FrontendRoute, error) {
	return f.data.FrontendRoutes, nil
}

func (f fakeOrchestratorSnapshot) ListGatewayRoutes(context.Context) ([]orchestratorsnapshot.GatewayRoute, error) {
	return f.data.GatewayRoutes, nil
}

func (f fakeOrchestratorSnapshot) ListComponents(context.Context) ([]orchestratorsnapshot.Component, error) {
	return f.data.Components, nil
}

func (f fakeOrchestratorSnapshot) ListEdges(context.Context) ([]orchestratorsnapshot.Edge, error) {
	return f.data.Edges, nil
}

func (f fakeOrchestratorSnapshot) Detail(_ context.Context, serviceID string) (orchestratorsnapshot.Detail, error) {
	var detail orchestratorsnapshot.Detail
	for _, service := range f.data.Services {
		if service.ServiceID == serviceID {
			detail.Service = service
			break
		}
	}
	if detail.Service.ServiceID == "" {
		return orchestratorsnapshot.Detail{}, errors.New("service not found")
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
	for _, endpoint := range f.data.Endpoints {
		if endpoint.ServiceID == serviceID {
			detail.Endpoints = append(detail.Endpoints, endpoint)
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

func TestTopologyReturnsBuiltinSnapshotData(t *testing.T) {
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
		repo: fakeOrchestratorSnapshot{data: testSnapshotData()},
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
	if !hasSet(resp.Sets, "single-node-oj") || !hasSet(resp.Sets, "distributed-oj") {
		t.Fatalf("topology should include distributed-oj and single-node-oj sets: %#v", resp.Sets)
	}
	if !hasNode(resp.ServiceDefinitions, "judge-api") {
		t.Fatalf("topology should include judge-api service definition")
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

func TestOrchestratorSnapshotReturnsServiceFirstBaseServices(t *testing.T) {
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
		repo: fakeOrchestratorSnapshot{data: testSnapshotData()},
	}

	resp, err := logic.OrchestratorSnapshot("Bearer "+token, false)
	if err != nil {
		t.Fatalf("orchestrator snapshot failed: %v", err)
	}
	for _, serviceID := range []string{"gateway", "web-shell", "problem-api", "judge-api", "judge-worker"} {
		if !hasNode(resp.ServiceDefinitions, serviceID) {
			t.Fatalf("orchestrator snapshot should include %s", serviceID)
		}
	}
	if len(resp.Topology.Nodes) == 0 || len(resp.Topology.Edges) == 0 {
		t.Fatalf("orchestrator snapshot topology should be non-empty")
	}
	if len(resp.Topology.ServiceDefinitions) == 0 || len(resp.Topology.DependencyEdges) == 0 {
		t.Fatalf("orchestrator snapshot should expose service definition graph fields")
	}
	if len(resp.Services) == 0 || len(resp.Workers) == 0 || len(resp.HealthChecks) == 0 {
		t.Fatalf("orchestrator snapshot should include services/workers/health checks")
	}
	if len(resp.Components) == 0 {
		t.Fatalf("orchestrator snapshot should include full component list")
	}
}

func TestOrchestratorSnapshotIncludeDisabledControlsActiveContributions(t *testing.T) {
	ctx := context.Background()
	token, err := sharedjwt.Generate("test-secret", 1, "root", []string{"admin"}, 1)
	if err != nil {
		t.Fatal(err)
	}

	data := testSnapshotData()
	data.Services = append(data.Services, orchestratorsnapshot.Service{
		ServiceID: "demo-service",
		SetID:     "demo",
		Name:      "Demo Service",
		Version:   "0.1.0",
		Status:    "DISABLED",
		Kind:      "feature",
	})
	data.Permissions = append(data.Permissions, orchestratorsnapshot.Permission{
		ServiceID:     "demo-service",
		PermissionKey: "demo.view",
		Description:   "View demo service metadata.",
	})
	data.Menus = append(data.Menus, orchestratorsnapshot.Menu{
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
		repo: fakeOrchestratorSnapshot{data: data},
	}

	active, err := logic.OrchestratorSnapshot("Bearer "+token, false)
	if err != nil {
		t.Fatalf("active orchestrator snapshot failed: %v", err)
	}
	if hasNode(active.ServiceDefinitions, "demo-service") || hasPermissionItem(active.Permissions, "demo.view") {
		t.Fatalf("disabled demo service should not appear in active orchestrator snapshot")
	}

	all, err := logic.OrchestratorSnapshot("Bearer "+token, true)
	if err != nil {
		t.Fatalf("include-disabled orchestrator snapshot failed: %v", err)
	}
	if !hasNode(all.ServiceDefinitions, "demo-service") || !hasPermissionItem(all.Permissions, "demo.view") {
		t.Fatalf("include-disabled orchestrator snapshot should expose disabled demo snapshot entries")
	}
}

func TestServiceRoutesReturnsSnapshotRouteTable(t *testing.T) {
	ctx := context.Background()
	token, err := sharedjwt.Generate("test-secret", 1, "root", []string{"admin"}, 1)
	if err != nil {
		t.Fatal(err)
	}

	data := testSnapshotData()
	data.GatewayRoutes = append(data.GatewayRoutes, orchestratorsnapshot.GatewayRoute{
		ServiceID:     "demo-service",
		Prefix:        "/api/demo",
		TargetService: "demo-api",
		AuthMode:      "admin",
		Enabled:       false,
	})
	data.Services = append(data.Services, orchestratorsnapshot.Service{
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
		repo: fakeOrchestratorSnapshot{data: data},
	}

	active, err := logic.OrchestratorRoutes("Bearer "+token, false, false)
	if err != nil {
		t.Fatalf("service routes failed: %v", err)
	}
	if hasServiceRoute(active.Routes, "/api/demo") {
		t.Fatalf("disabled demo route should not appear in active service route table")
	}
	all, err := logic.OrchestratorRoutes("Bearer "+token, true, false)
	if err != nil {
		t.Fatalf("include-disabled service routes failed: %v", err)
	}
	if !hasServiceRoute(all.Routes, "/api/demo") {
		t.Fatalf("include-disabled route table should expose demo metadata route")
	}
	for _, route := range all.Routes {
		if route.UpstreamBase != "" {
			t.Fatalf("admin route table should not expose upstream_base by default: %#v", route)
		}
	}
	debug, err := logic.OrchestratorRoutes("Bearer "+token, true, true)
	if err != nil {
		t.Fatalf("debug service routes failed: %v", err)
	}
	if !hasServiceRouteUpstream(debug.Routes, "/api/demo") {
		t.Fatalf("debug service routes should expose upstream for trusted route")
	}
}

func TestServiceStatusesAdminAPIUsesServiceStatusDriver(t *testing.T) {
	ctx := context.Background()
	token, err := sharedjwt.Generate("test-secret", 1, "root", []string{"admin"}, 1)
	if err != nil {
		t.Fatal(err)
	}
	logic := &AdminServiceStatusLogic{
		ctx: ctx,
		svcCtx: &svc.ServiceContext{
			Config: config.Config{Jwt: config.JwtConfig{Secret: "test-secret"}},
			ServiceStatusDriver: fakeServiceStatusDriver{services: []servicestatus.ServiceStatus{
				{
					ServiceID:      "problem-api",
					OwnerServiceID: "judge-api",
					Kind:           "http",
					Lifecycle:      servicestatus.LifecycleManaged,
					Runtime:        "compose",
					ComposeService: "problem-api",
					State:          servicestatus.ServiceStatusRunning,
					Health:         "ok",
					Routes:         []string{"/api/problem"},
					Required:       true,
				},
				{
					ServiceID:      "demo-metadata-service",
					OwnerServiceID: "demo-service",
					Kind:           "metadata",
					Lifecycle:      servicestatus.LifecycleMetadata,
					Runtime:        "metadata",
					State:          servicestatus.ServiceStatusDeclared,
					Health:         "metadata",
				},
				{
					ServiceID:      "judge-worker",
					OwnerServiceID: "judge-api",
					Kind:           "worker",
					Lifecycle:      servicestatus.LifecycleManaged,
					Runtime:        "compose",
					State:          servicestatus.ServiceStatusUnknown,
					Health:         "unknown",
				},
			}},
		},
		repo: fakeOrchestratorSnapshot{data: testSnapshotData()},
	}

	resp, err := logic.ListServices("Bearer " + token)
	if err != nil {
		t.Fatalf("Service Status services failed: %v", err)
	}
	if len(resp.Services) != 2 || len(resp.Workers) != 1 {
		t.Fatalf("unexpected Service Status grouping: %#v", resp)
	}
	detail, err := logic.ServiceDetail("Bearer "+token, "problem-api")
	if err != nil {
		t.Fatalf("Service Status detail failed: %v", err)
	}
	if detail.Service.ServiceId != "problem-api" || detail.Service.State != servicestatus.ServiceStatusRunning {
		t.Fatalf("unexpected service detail: %#v", detail)
	}
}

func TestServiceStatusesRejectOrdinaryUser(t *testing.T) {
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
	logic := &AdminServiceStatusLogic{
		ctx: context.Background(),
		svcCtx: &svc.ServiceContext{
			Config: config.Config{Jwt: config.JwtConfig{Secret: "test-secret"}},
		},
		repo: fakeOrchestratorSnapshot{data: testSnapshotData()},
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

func hasNode(items []types.ServiceDefinitionItem, serviceID string) bool {
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

func hasServiceRoute(items []types.OrchestratorRouteItem, prefix string) bool {
	for _, item := range items {
		if item.Prefix == prefix {
			return true
		}
	}
	return false
}

func hasServiceRouteUpstream(items []types.OrchestratorRouteItem, prefix string) bool {
	for _, item := range items {
		if item.Prefix == prefix && item.UpstreamBase != "" {
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

func testRouteTableOptions() servicestatus.RouteTableOptions {
	return servicestatus.RouteTableOptions{
		TrustedServices: map[string]servicestatus.TrustedService{
			"demo-api":    {ServiceID: "demo-api", UpstreamBase: "http://demo-api:8080"},
			"problem-api": {ServiceID: "problem-api", UpstreamBase: "http://problem-api:8083"},
			"judge-api":   {ServiceID: "judge-api", UpstreamBase: "http://judge-api:8082"},
		},
	}
}
