package serviceregistry

import (
	"context"
	"encoding/json"
	"fmt"
)

type BootstrapWriter interface {
	UpsertSet(context.Context, Set) error
	UpsertService(context.Context, Service) error
	UpsertEdge(context.Context, Edge) error
	UpsertComponent(context.Context, Component) error
	UpsertInstallation(context.Context, Installation) error
	UpsertPermission(context.Context, Permission) error
	UpsertMenu(context.Context, Menu) error
	UpsertFrontendRoute(context.Context, FrontendRoute) error
	UpsertGatewayRoute(context.Context, GatewayRoute) error
	UpsertMigration(context.Context, Migration) error
}

func BootstrapBuiltin(ctx context.Context, writer BootstrapWriter) error {
	data := BuiltinData()
	for _, item := range data.Sets {
		if err := writer.UpsertSet(ctx, item); err != nil {
			return fmt.Errorf("bootstrap service set %s: %w", item.SetID, err)
		}
	}
	for _, item := range data.Services {
		if err := writer.UpsertService(ctx, item); err != nil {
			return fmt.Errorf("bootstrap service %s: %w", item.ServiceID, err)
		}
	}
	for _, item := range data.Edges {
		if err := writer.UpsertEdge(ctx, item); err != nil {
			return fmt.Errorf("bootstrap service edge %s -> %s: %w", item.FromServiceID, item.ToServiceID, err)
		}
	}
	for _, item := range data.Components {
		if err := writer.UpsertComponent(ctx, item); err != nil {
			return fmt.Errorf("bootstrap service component %s/%s: %w", item.ServiceID, item.ComponentID, err)
		}
	}
	for _, item := range data.Installations {
		if err := writer.UpsertInstallation(ctx, item); err != nil {
			return fmt.Errorf("bootstrap service installation %s: %w", item.ServiceID, err)
		}
	}
	for _, item := range data.Permissions {
		if err := writer.UpsertPermission(ctx, item); err != nil {
			return fmt.Errorf("bootstrap service permission %s: %w", item.PermissionKey, err)
		}
	}
	for _, item := range data.Menus {
		if err := writer.UpsertMenu(ctx, item); err != nil {
			return fmt.Errorf("bootstrap service menu %s: %w", item.MenuKey, err)
		}
	}
	for _, item := range data.FrontendRoutes {
		if err := writer.UpsertFrontendRoute(ctx, item); err != nil {
			return fmt.Errorf("bootstrap service frontend route %s: %w", item.RoutePath, err)
		}
	}
	for _, item := range data.GatewayRoutes {
		if err := writer.UpsertGatewayRoute(ctx, item); err != nil {
			return fmt.Errorf("bootstrap service gateway route %s: %w", item.Prefix, err)
		}
	}
	for _, item := range data.Migrations {
		if err := writer.UpsertMigration(ctx, item); err != nil {
			return fmt.Errorf("bootstrap service migration %s: %w", item.MigrationName, err)
		}
	}
	return nil
}

func BuiltinData() BootstrapData {
	services := []Service{
		service("root-runtime-manager", "runtime", "Root Runtime Manager", KindKernel, "Root Installer / Runtime Manager control plane", runtimeManifest("root-runtime-manager", "kernel", "metadata", "", nil)),
		service("gateway", "single-node-oj", "Gateway", KindPlatform, "External HTTP entry, auth, policy checks, routing, audit and rate limits", runtimeManifest("gateway", "edge-http", "managed", "gateway", []string{"/api"})),
		service("web-shell", "single-node-oj", "Web Shell", KindPlatform, "Root-side hotpluggable Web UI with read-only runtime views", runtimeManifest("web-shell", "frontend", "metadata", "", []string{"/"})),
		service("problem-api", "single-node-oj", "Problem API", KindFeature, "Problem catalog, problem packages, file index and permissions", runtimeManifest("problem-api", "backend-http", "managed", "problem-api", []string{"/api/problem"})),
		service("judge-api", "single-node-oj", "Judge API", KindFeature, "Submission queue, worker list, task dispatch and result intake", runtimeManifest("judge-api", "backend-http", "managed", "judge-api", []string{"/api/judge", "/api/judge/worker"})),
		service("judge-worker", "judge-worker-node", "Judge Worker", KindFeature, "Root or non-root worker endpoint with local concurrency and sandbox slots", runtimeManifest("judge-worker", "worker", "managed", "judge-worker", nil)),
		service("storage", "single-node-oj", "Storage", KindPlatform, "Object storage or external storage endpoint", runtimeManifest("storage", "external-storage", "metadata", "", nil)),
		service("postgres", "single-node-oj", "PostgreSQL", KindPlatform, "Database service or external PostgreSQL endpoint", runtimeManifest("postgres", "external-database", "metadata", "", nil)),
	}

	data := BootstrapData{
		Sets: []Set{
			{SetID: "single-node-oj", Name: "单机 OJ", Description: "Root 设备运行基础 OJ 服务。", SortOrder: 10},
			{SetID: "distributed-root", Name: "分布式评测 Root", Description: "Root 设备运行控制面和后端服务，评测节点独立加入。", SortOrder: 20},
			{SetID: "judge-worker-node", Name: "评测节点", Description: "Non-root 设备只运行 judge-worker。", SortOrder: 30},
		},
		Services: services,
		Edges: []Edge{
			{FromServiceID: "gateway", ToServiceID: "web-shell", EdgeType: "routes", Required: true},
			{FromServiceID: "gateway", ToServiceID: "problem-api", EdgeType: "routes", Required: true},
			{FromServiceID: "gateway", ToServiceID: "judge-api", EdgeType: "routes", Required: true},
			{FromServiceID: "problem-api", ToServiceID: "postgres", EdgeType: "link", Required: true},
			{FromServiceID: "problem-api", ToServiceID: "storage", EdgeType: "link", Required: true},
			{FromServiceID: "judge-api", ToServiceID: "postgres", EdgeType: "link", Required: true},
			{FromServiceID: "judge-api", ToServiceID: "storage", EdgeType: "link", Required: true},
			{FromServiceID: "judge-worker", ToServiceID: "judge-api", EdgeType: "link", Required: true},
			{FromServiceID: "judge-worker", ToServiceID: "storage", EdgeType: "link", Required: true},
		},
	}

	for _, item := range services {
		data.Installations = append(data.Installations, Installation{
			ServiceID: item.ServiceID,
			Name:      item.Name,
			Version:   item.Version,
			Status:    StatusEnabled,
			Manifest:  item.Manifest,
		})
		data.Components = append(data.Components,
			Component{ServiceID: item.ServiceID, ComponentID: item.ServiceID + "-endpoint", ComponentType: "endpoint", Status: StatusEnabled, Config: mustJSON(map[string]any{"service_id": item.ServiceID})},
			Component{ServiceID: item.ServiceID, ComponentID: item.ServiceID + "-health", ComponentType: "health_check", Status: StatusEnabled, Config: mustJSON(map[string]any{"target": item.ServiceID})},
		)
	}
	data.Permissions = []Permission{
		{ServiceID: "root-runtime-manager", PermissionKey: "system.admin", Description: "系统管理员"},
		{ServiceID: "root-runtime-manager", PermissionKey: "service.install", Description: "安装 Service"},
		{ServiceID: "root-runtime-manager", PermissionKey: "service.enable", Description: "启用 Service"},
		{ServiceID: "root-runtime-manager", PermissionKey: "service.disable", Description: "禁用 Service"},
		{ServiceID: "root-runtime-manager", PermissionKey: "topology.read", Description: "读取 Topology"},
		{ServiceID: "problem-api", PermissionKey: "problem.view", Description: "查看题目"},
		{ServiceID: "problem-api", PermissionKey: "problem.create", Description: "创建题目"},
		{ServiceID: "problem-api", PermissionKey: "problem.edit", Description: "编辑题目"},
		{ServiceID: "judge-api", PermissionKey: "judge.submit", Description: "提交评测"},
		{ServiceID: "judge-api", PermissionKey: "submission.view.own", Description: "查看自己的提交"},
		{ServiceID: "judge-api", PermissionKey: "submission.view.all", Description: "查看全部提交"},
	}
	data.Menus = []Menu{
		{ServiceID: "problem-api", MenuKey: "problems", Title: "题库", RoutePath: "/problems", SortOrder: 10, RequiredPermission: "problem.view", Enabled: true},
		{ServiceID: "judge-api", MenuKey: "submissions", Title: "提交", RoutePath: "/submissions", SortOrder: 20, RequiredPermission: "submission.view.own", Enabled: true},
		{ServiceID: "judge-api", MenuKey: "admin-judge", Title: "评测状态", RoutePath: "/admin/judge", SortOrder: 100, RequiredPermission: "system.admin", Enabled: true},
		{ServiceID: "root-runtime-manager", MenuKey: "runtime-services", Title: "Service 状态", RoutePath: "/admin/runtime/services", SortOrder: 110, RequiredPermission: "system.admin", Enabled: true},
	}
	data.FrontendRoutes = []FrontendRoute{
		{ServiceID: "problem-api", RoutePath: "/problems", RouteName: "problems", ComponentKey: "ProblemListView", RequiredPermission: "problem.view", Enabled: true},
		{ServiceID: "problem-api", RoutePath: "/problems/new", RouteName: "problem-create", ComponentKey: "ProblemCreateView", RequiredPermission: "problem.create", Enabled: true},
		{ServiceID: "problem-api", RoutePath: "/problems/:id", RouteName: "problem-detail", ComponentKey: "ProblemDetailView", RequiredPermission: "problem.view", Enabled: true},
		{ServiceID: "judge-api", RoutePath: "/submissions", RouteName: "submissions", ComponentKey: "SubmissionsListView", RequiredPermission: "submission.view.own", Enabled: true},
		{ServiceID: "judge-api", RoutePath: "/submissions/:id", RouteName: "submission-detail", ComponentKey: "SubmissionDetailView", RequiredPermission: "submission.view.own", Enabled: true},
		{ServiceID: "judge-api", RoutePath: "/admin/judge", RouteName: "admin-judge", ComponentKey: "AdminJudgeView", RequiredPermission: "system.admin", Enabled: true},
	}
	data.GatewayRoutes = []GatewayRoute{
		{ServiceID: "problem-api", Prefix: "/api/problem", TargetService: "problem-api", AuthMode: "user", Enabled: true},
		{ServiceID: "judge-api", Prefix: "/api/judge", TargetService: "judge-api", AuthMode: "user", Enabled: true},
		{ServiceID: "judge-api", Prefix: "/api/judge/worker", TargetService: "judge-api", AuthMode: "worker", Enabled: true},
	}
	data.Migrations = []Migration{
		{ServiceID: "problem-api", Version: "0.1.0", MigrationName: "deploy/migrations/000004_problem_package_core.up.sql", Checksum: "declared-service-migration"},
		{ServiceID: "judge-api", Version: "0.1.0", MigrationName: "deploy/migrations/000002_judge_schema.up.sql", Checksum: "declared-service-migration"},
	}

	return data
}

func service(id string, setID string, name string, kind string, description string, manifest json.RawMessage) Service {
	return Service{
		ServiceID:   id,
		SetID:       setID,
		Name:        name,
		Version:     "0.1.0",
		Status:      StatusEnabled,
		Kind:        kind,
		Description: description,
		Manifest:    manifest,
	}
}

func runtimeManifest(id string, kind string, lifecycle string, composeService string, routes []string) json.RawMessage {
	provides := map[string]any{
		"roles":           []map[string]any{},
		"services":        []map[string]any{},
		"workers":         []map[string]any{},
		"storage_buckets": []map[string]any{},
		"scheduled_jobs":  []map[string]any{},
		"admin_panels":    []map[string]any{},
		"events":          map[string]any{"publishes": []string{}, "subscribes": []string{}},
		"topology":        map[string]any{"nodes": []map[string]any{}, "edges": []map[string]any{}},
	}
	if kind == "worker" {
		provides["workers"] = []map[string]any{runtimeDecl(id, "worker", lifecycle, composeService, routes)}
	} else if composeService != "" {
		provides["services"] = []map[string]any{runtimeDecl(id, kind, lifecycle, composeService, routes)}
	}
	return mustJSON(map[string]any{
		"schema_version": 1,
		"id":             id,
		"kind":           kind,
		"status":         "builtin",
		"provides":       provides,
	})
}

func runtimeDecl(id string, kind string, lifecycle string, composeService string, routes []string) map[string]any {
	return map[string]any{
		"id":              id,
		"name":            id,
		"kind":            kind,
		"lifecycle":       lifecycle,
		"trusted_runtime": "compose",
		"compose_service": composeService,
		"health_check_id": id + "-health",
		"routes":          routes,
		"required":        true,
	}
}

func mustJSON(value any) json.RawMessage {
	data, err := json.Marshal(value)
	if err != nil {
		panic(err)
	}
	return data
}
