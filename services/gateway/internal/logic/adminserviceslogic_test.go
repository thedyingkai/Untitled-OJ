package logic

import (
	"context"
	"errors"
	"net/http"
	"net/http/httptest"
	"strings"
	"testing"

	"ojos-gateway/internal/config"
	"ojos-gateway/internal/orchestrator/servicestatus"
	orchestratorsnapshot "ojos-gateway/internal/orchestrator/snapshot"
	"ojos-gateway/internal/proxy"
	"ojos-gateway/internal/svc"
	"ojos-gateway/internal/types"
	sharedjwt "ojos-shared/security/jwt"

	"go.uber.org/zap"
)

type fakeOrchestratorSnapshot struct {
	data orchestratorsnapshot.SnapshotData
}

type fakeServiceStatusDriver struct {
	services []servicestatus.ServiceStatus
}

func testSnapshotData() orchestratorsnapshot.SnapshotData {
	return orchestratorsnapshot.SnapshotData{
		Services: []orchestratorsnapshot.Service{
			{ServiceID: "gateway", Name: "Gateway", Version: "0.1.0", Status: orchestratorsnapshot.StatusEnabled, Kind: "gateway"},
			{ServiceID: "auth-service", Name: "Auth Service", Version: "0.1.0", Status: orchestratorsnapshot.StatusEnabled, Kind: "backend-api"},
			{ServiceID: "problem-service", Name: "Problem Service", Version: "0.1.0", Status: orchestratorsnapshot.StatusEnabled, Kind: "backend-api"},
			{ServiceID: "judge-api", Name: "Judge API", Version: "0.1.0", Status: orchestratorsnapshot.StatusEnabled, Kind: "backend-api"},
			{ServiceID: "judge-worker", Name: "Judge Worker", Version: "0.1.0", Status: orchestratorsnapshot.StatusEnabled, Kind: "backend-worker"},
			{ServiceID: "postgresql", Name: "PostgreSQL", Version: "17.0.0", Status: orchestratorsnapshot.StatusEnabled, Kind: "database"},
			{ServiceID: "redis", Name: "Redis", Version: "8.0.0", Status: orchestratorsnapshot.StatusEnabled, Kind: "cache"},
			{ServiceID: "storage-service", Name: "Storage Service", Version: "0.1.0", Status: orchestratorsnapshot.StatusEnabled, Kind: "storage"},
		},
		Edges: []orchestratorsnapshot.Edge{
			{FromServiceID: "gateway", ToServiceID: "auth-service", EdgeType: "link", Required: true},
			{FromServiceID: "gateway", ToServiceID: "problem-service", EdgeType: "link", Required: true},
			{FromServiceID: "gateway", ToServiceID: "judge-api", EdgeType: "link", Required: true},
			{FromServiceID: "auth-service", ToServiceID: "postgresql", EdgeType: "link", Required: true},
			{FromServiceID: "auth-service", ToServiceID: "redis", EdgeType: "link", Required: true},
			{FromServiceID: "problem-service", ToServiceID: "postgresql", EdgeType: "link", Required: true},
			{FromServiceID: "problem-service", ToServiceID: "storage-service", EdgeType: "link", Required: true},
			{FromServiceID: "judge-worker", ToServiceID: "judge-api", EdgeType: "link", Required: true},
			{FromServiceID: "judge-api", ToServiceID: "postgresql", EdgeType: "link", Required: true},
			{FromServiceID: "judge-api", ToServiceID: "redis", EdgeType: "link", Required: true},
			{FromServiceID: "judge-api", ToServiceID: "storage-service", EdgeType: "link", Required: true},
		},
		Components: []orchestratorsnapshot.Component{
			{ServiceID: "gateway", ComponentID: "gateway-endpoint", ComponentType: "endpoint", Status: orchestratorsnapshot.StatusEnabled, Config: []byte(`{"socket_addr":"127.0.0.1:8080","protocol":"http"}`)},
			{ServiceID: "gateway", ComponentID: "gateway-health", ComponentType: "health_check", Status: orchestratorsnapshot.StatusEnabled, Config: []byte(`{"type":"http","target":"/health"}`)},
			{ServiceID: "problem-service", ComponentID: "problem-service-endpoint", ComponentType: "endpoint", Status: orchestratorsnapshot.StatusEnabled, Config: []byte(`{"socket_addr":"127.0.0.1:8083","protocol":"http"}`)},
			{ServiceID: "problem-service", ComponentID: "problem-service", ComponentType: "backend_service", Status: orchestratorsnapshot.StatusEnabled, Config: []byte(`{"service":"problem-service","trusted_runtime":"compose","compose_service":"problem-service","health_check_id":"problem-service-health","routes":["/api/problem"],"required":true}`)},
			{ServiceID: "problem-service", ComponentID: "problem-service-health", ComponentType: "health_check", Status: orchestratorsnapshot.StatusEnabled, Config: []byte(`{"type":"http","target":"/health"}`)},
			{ServiceID: "judge-api", ComponentID: "judge-api-endpoint", ComponentType: "endpoint", Status: orchestratorsnapshot.StatusEnabled, Config: []byte(`{"socket_addr":"127.0.0.1:8082","protocol":"http"}`)},
			{ServiceID: "judge-api", ComponentID: "judge-api", ComponentType: "backend_service", Status: orchestratorsnapshot.StatusEnabled, Config: []byte(`{"service":"judge-api","trusted_runtime":"compose","compose_service":"judge-api","health_check_id":"judge-api-health","routes":["/api/judge"],"required":true}`)},
			{ServiceID: "judge-worker", ComponentID: "judge-worker-endpoint", ComponentType: "endpoint", Status: orchestratorsnapshot.StatusEnabled, Config: []byte(`{"socket_addr":"127.0.0.1:9101","protocol":"http"}`)},
			{ServiceID: "judge-worker", ComponentID: "judge-worker", ComponentType: "worker_service", Status: orchestratorsnapshot.StatusEnabled, Config: []byte(`{"service":"judge-worker","trusted_runtime":"compose","compose_service":"judge-worker","required":true}`)},
		},
		Endpoints: []orchestratorsnapshot.Endpoint{
			{Endpoint: "127.0.0.1:8080:gateway", ServiceID: "gateway", Protocol: "http", HealthPath: "/health", Health: "ok", Reachable: true},
			{Endpoint: "127.0.0.1:8083:problem-service", ServiceID: "problem-service", Protocol: "http", HealthPath: "/health", Health: "ok", Reachable: true},
			{Endpoint: "127.0.0.1:8082:judge-api", ServiceID: "judge-api", Protocol: "http", HealthPath: "/health", Health: "ok", Reachable: true},
			{Endpoint: "127.0.0.1:9101:judge-worker", ServiceID: "judge-worker", Protocol: "http", HealthPath: "/health", Health: "ok", Reachable: true},
			{Endpoint: "127.0.0.2:9101:judge-worker", ServiceID: "judge-worker", Protocol: "http", HealthPath: "/health", Health: "ok", Reachable: true},
		},
		Permissions: []orchestratorsnapshot.Permission{
			{ServiceID: "problem-service", PermissionKey: "problem.read", Description: "读取题目"},
			{ServiceID: "judge-api", PermissionKey: "judge.submit", Description: "提交评测"},
		},
		Menus: []orchestratorsnapshot.Menu{
			{ServiceID: "gateway", MenuKey: "problems", Title: "题库", RoutePath: "/problems", Enabled: true},
			{ServiceID: "gateway", MenuKey: "submissions", Title: "提交", RoutePath: "/submissions", Enabled: true},
		},
		FrontendRoutes: []orchestratorsnapshot.FrontendRoute{
			{ServiceID: "gateway", RoutePath: "/problems", RouteName: "problem-list", ComponentKey: "ProblemList", Enabled: true},
		},
		GatewayRoutes: []orchestratorsnapshot.GatewayRoute{
			{ServiceID: "gateway", Prefix: "/api/auth", TargetService: "auth-service", AuthMode: "optional", Enabled: true},
			{ServiceID: "gateway", Prefix: "/api/problem", TargetService: "problem-service", AuthMode: "required", Enabled: true},
			{ServiceID: "gateway", Prefix: "/api/judge", TargetService: "judge-api", AuthMode: "required", Enabled: true},
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

func (f fakeOrchestratorSnapshot) ListEndpointGroups(context.Context) ([]orchestratorsnapshot.EndpointGroup, error) {
	return orchestratorsnapshot.EndpointGroups(f.data.Endpoints), nil
}

func (f fakeOrchestratorSnapshot) Topology(context.Context) (orchestratorsnapshot.Topology, error) {
	return orchestratorsnapshot.Topology{
		EndpointGroups: orchestratorsnapshot.EndpointGroups(f.data.Endpoints),
		Nodes:          f.data.Services,
		Edges:          f.data.Edges,
		Components:     f.data.Components,
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
	hasSystemAdminPermission = func(context.Context, *svc.ServiceContext, string, int64) (bool, error) {
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
	if len(resp.EndpointGroups) == 0 {
		t.Fatalf("topology endpoint groups should be non-empty")
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
	if !hasEndpointGroup(resp.EndpointGroups, "judge-worker", 2) {
		t.Fatalf("topology should include derived judge-worker endpoint group: %#v", resp.EndpointGroups)
	}
	if !hasNode(resp.ServiceDefinitions, "judge-api") {
		t.Fatalf("topology should include judge-api service definition")
	}
	for _, edge := range [][2]string{
		{"gateway", "problem-service"},
		{"gateway", "judge-api"},
		{"judge-worker", "judge-api"},
	} {
		if !hasEdge(resp.DependencyEdges, edge[0], edge[1]) {
			t.Fatalf("topology should include edge %s -> %s", edge[0], edge[1])
		}
	}
	for _, componentID := range []string{"problem-service-endpoint", "problem-service-health"} {
		if !hasComponent(resp.Components, "problem-service", componentID) {
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
	for _, serviceID := range []string{"gateway", "auth-service", "problem-service", "judge-api", "judge-worker", "postgresql", "redis", "storage-service"} {
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

func TestOrchestratorRoutesReloadUpdatesGatewayProxyTable(t *testing.T) {
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
		AuthMode:      "public",
		Enabled:       true,
	})
	data.Services = append(data.Services, orchestratorsnapshot.Service{
		ServiceID: "demo-service",
		Name:      "Demo Service",
		Version:   "0.1.0",
		Status:    orchestratorsnapshot.StatusEnabled,
		Kind:      "feature",
	})
	serviceProxy, err := proxy.NewServiceProxy(nil, nil, "test-secret", nil, zap.NewNop())
	if err != nil {
		t.Fatal(err)
	}
	logic := &AdminOrchestratorRoutesReloadLogic{
		ctx: ctx,
		svcCtx: &svc.ServiceContext{
			Config: config.Config{
				Jwt: config.JwtConfig{Secret: "test-secret"},
			},
			ServiceProxy: serviceProxy,
			RouteTableOptions: servicestatus.RouteTableOptions{
				TrustedServices: map[string]servicestatus.TrustedService{
					"demo-api": {ServiceID: "demo-api", UpstreamBase: "http://127.0.0.1:1"},
				},
			},
		},
	}
	logic.routeReader = &AdminServicesLogic{
		ctx:    ctx,
		svcCtx: logic.svcCtx,
		repo:   fakeOrchestratorSnapshot{data: data},
	}

	resp, err := logic.AdminOrchestratorRoutesReload(&types.AdminRoutesReloadReq{
		Authorization: "Bearer " + token,
		OperationId:   "op-release-gateway-publish",
		ServiceName:   "demo-service",
	})
	if err != nil {
		t.Fatalf("route reload failed: %v", err)
	}
	if resp.Status != "reloaded" || resp.RouteCount == 0 {
		t.Fatalf("unexpected reload response: %#v", resp)
	}
	rr := httptest.NewRecorder()
	req := httptest.NewRequest(http.MethodGet, "/api/demo/hello", nil)
	serviceProxy.ServeHTTP(rr, req)
	if rr.Code != http.StatusBadGateway {
		t.Fatalf("expected proxy to use reloaded route table and hit demo upstream, got %d body=%s", rr.Code, rr.Body.String())
	}
}

func TestOrchestratorRoutesReloadAcceptsPushedRoutes(t *testing.T) {
	ctx := context.Background()
	token, err := sharedjwt.Generate("test-secret", 1, "root", []string{"admin"}, 1)
	if err != nil {
		t.Fatal(err)
	}
	serviceProxy, err := proxy.NewServiceProxy(nil, nil, "test-secret", nil, zap.NewNop())
	if err != nil {
		t.Fatal(err)
	}
	serviceProxy.SetPermissionChecker(func(context.Context, string, proxy.PermissionCheckCaller, string) (bool, error) {
		return true, nil
	})
	logic := &AdminOrchestratorRoutesReloadLogic{
		ctx: ctx,
		svcCtx: &svc.ServiceContext{
			Config:       config.Config{Jwt: config.JwtConfig{Secret: "test-secret"}},
			ServiceProxy: serviceProxy,
		},
	}

	resp, err := logic.AdminOrchestratorRoutesReload(&types.AdminRoutesReloadReq{
		Authorization: "Bearer " + token,
		OperationId:   "op-release-storage-install",
		ServiceName:   "storage-service",
		Version:       "1",
		CanProxy:      true,
		Routes: []types.OrchestratorRouteItem{
			{
				RouteId:            "storage-service:storage.object.get",
				ApiId:              "storage.object.get",
				NodeId:             "child-node",
				ProviderNodeId:     "root-node",
				ProviderService:    "storage-service",
				ProviderEndpoint:   "127.0.0.1:8085:storage-service",
				OwnerServiceId:     "storage-service",
				Prefix:             "/api/storage/objects",
				ServiceId:          "storage-service",
				TargetService:      "storage-service",
				UpstreamBase:       "http://127.0.0.1:1",
				AuthMode:           "service",
				RequiredPermission: "storage.object.read",
				Methods:            []string{http.MethodGet},
				Enabled:            true,
				ProxyEnabled:       true,
				Priority:           len("/api/storage/objects"),
				CreatedFrom:        "orchestrator_effective_api_view",
				Status:             "active",
			},
		},
	})
	if err != nil {
		t.Fatalf("pushed route reload failed: %v", err)
	}
	if resp.Status != "reloaded" || resp.RouteCount != 1 {
		t.Fatalf("unexpected pushed reload response: %#v", resp)
	}
	rr := httptest.NewRecorder()
	req := httptest.NewRequest(http.MethodGet, "/internal/apis/storage.object.get/submissions/a.cpp", nil)
	req.Header.Set("Authorization", "Bearer service-token")
	req.Header.Set("X-OJOS-Caller-Service", "judge-api")
	req.Header.Set("X-OJOS-Node-Id", "child-node")
	serviceProxy.ServeHTTP(rr, req)
	if rr.Code != http.StatusBadGateway {
		t.Fatalf("expected pushed proxy route to be active and hit upstream, got %d body=%s", rr.Code, rr.Body.String())
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
					ServiceID:      "problem-service",
					OwnerServiceID: "judge-api",
					Kind:           "http",
					Lifecycle:      servicestatus.LifecycleManaged,
					Runtime:        "compose",
					ComposeService: "problem-service",
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
	detail, err := logic.ServiceDetail("Bearer "+token, "problem-service")
	if err != nil {
		t.Fatalf("Service Status detail failed: %v", err)
	}
	if detail.Service.ServiceId != "problem-service" || detail.Service.State != servicestatus.ServiceStatusRunning {
		t.Fatalf("unexpected service detail: %#v", detail)
	}
}

func TestServiceStatusesRejectOrdinaryUser(t *testing.T) {
	oldChecker := hasSystemAdminPermission
	hasSystemAdminPermission = func(context.Context, *svc.ServiceContext, string, int64) (bool, error) {
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

func hasEndpointGroup(items []types.EndpointGroupItem, serviceName string, endpointCount int) bool {
	for _, item := range items {
		if item.ServiceName == serviceName && item.EndpointCount == endpointCount && item.Selector == serviceName+"[*]" {
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
			"auth-service":    {ServiceID: "auth-service", UpstreamBase: "http://auth-service:8081"},
			"demo-api":        {ServiceID: "demo-api", UpstreamBase: "http://demo-api:8080"},
			"problem-service": {ServiceID: "problem-service", UpstreamBase: "http://problem-service:8083"},
			"judge-api":       {ServiceID: "judge-api", UpstreamBase: "http://judge-api:8082"},
		},
	}
}
