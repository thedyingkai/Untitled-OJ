package moduleregistry

import (
	"context"
	"encoding/json"
	"fmt"
)

type BootstrapWriter interface {
	UpsertSet(context.Context, Set) error
	UpsertModule(context.Context, Module) error
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
			return fmt.Errorf("bootstrap module set %s: %w", item.SetID, err)
		}
	}
	for _, item := range data.Modules {
		if err := writer.UpsertModule(ctx, item); err != nil {
			return fmt.Errorf("bootstrap module node %s: %w", item.ModuleID, err)
		}
	}
	for _, item := range data.Edges {
		if err := writer.UpsertEdge(ctx, item); err != nil {
			return fmt.Errorf("bootstrap module edge %s -> %s: %w", item.FromModuleID, item.ToModuleID, err)
		}
	}
	for _, item := range data.Components {
		if err := writer.UpsertComponent(ctx, item); err != nil {
			return fmt.Errorf("bootstrap module component %s/%s: %w", item.ModuleID, item.ComponentID, err)
		}
	}
	for _, item := range data.Installations {
		if err := writer.UpsertInstallation(ctx, item); err != nil {
			return fmt.Errorf("bootstrap module installation %s: %w", item.ModuleID, err)
		}
	}
	for _, item := range data.Permissions {
		if err := writer.UpsertPermission(ctx, item); err != nil {
			return fmt.Errorf("bootstrap module permission %s: %w", item.PermissionKey, err)
		}
	}
	for _, item := range data.Menus {
		if err := writer.UpsertMenu(ctx, item); err != nil {
			return fmt.Errorf("bootstrap module menu %s: %w", item.MenuKey, err)
		}
	}
	for _, item := range data.FrontendRoutes {
		if err := writer.UpsertFrontendRoute(ctx, item); err != nil {
			return fmt.Errorf("bootstrap module frontend route %s: %w", item.RoutePath, err)
		}
	}
	for _, item := range data.GatewayRoutes {
		if err := writer.UpsertGatewayRoute(ctx, item); err != nil {
			return fmt.Errorf("bootstrap module gateway route %s: %w", item.Prefix, err)
		}
	}
	for _, item := range data.Migrations {
		if err := writer.UpsertMigration(ctx, item); err != nil {
			return fmt.Errorf("bootstrap module migration %s: %w", item.MigrationName, err)
		}
	}
	return nil
}

func BuiltinData() BootstrapData {
	kernelManifest := func(description string) json.RawMessage {
		return mustJSON(map[string]any{
			"status":      "builtin",
			"description": description,
			"note":        "Kernel builtin module registered by Module Registry v0.",
		})
	}

	judgeManifest := mustJSON(judgeCoreManifest())
	kernelModules := []Module{
		{
			ModuleID:    "ojos.kernel.edge-ui-shell",
			SetID:       "kernel",
			Name:        "Edge UI Shell",
			Version:     "0.1.0",
			Status:      StatusEnabled,
			Kind:        KindKernel,
			Description: "前端 shell、基础布局、路由守卫和统一 API client。",
			Manifest:    kernelManifest("frontend shell, layout, router guard and API client"),
		},
		{
			ModuleID:    "ojos.kernel.identity-access",
			SetID:       "kernel",
			Name:        "Identity Access",
			Version:     "0.1.0",
			Status:      StatusEnabled,
			Kind:        KindKernel,
			Description: "认证、用户、角色、权限和资源级授权能力。",
			Manifest:    kernelManifest("auth, users, roles, permissions and resource bindings"),
		},
		{
			ModuleID:    "ojos.kernel.module-runtime",
			SetID:       "kernel",
			Name:        "Module Runtime",
			Version:     "0.1.0",
			Status:      StatusEnabled,
			Kind:        KindKernel,
			Description: "Module Registry v0，只读模块拓扑和后续 installer 的基础。",
			Manifest:    kernelManifest("read-only module registry v0"),
		},
		{
			ModuleID:    "ojos.kernel.config-secret",
			SetID:       "kernel",
			Name:        "Config Secret",
			Version:     "0.1.0",
			Status:      StatusEnabled,
			Kind:        KindKernel,
			Description: "环境配置、secret 注入边界和内部 HMAC 配置。",
			Manifest:    kernelManifest("configuration, secret boundary and internal HMAC"),
		},
		{
			ModuleID:    "ojos.kernel.audit-policy",
			SetID:       "kernel",
			Name:        "Audit Policy",
			Version:     "0.1.0",
			Status:      StatusEnabled,
			Kind:        KindKernel,
			Description: "权限变更和管理操作审计策略。",
			Manifest:    kernelManifest("permission and admin audit policy"),
		},
	}

	modules := append(kernelModules, Module{
		ModuleID:    "ojos.judge-core",
		SetID:       "core-capability",
		Name:        "Judge Core",
		Version:     "0.1.0",
		Status:      StatusEnabled,
		Kind:        KindFeature,
		Description: "题目、题目包、提交、评测、Worker Link、结果查询与评测集群管理模块。",
		Manifest:    judgeManifest,
	})

	data := BootstrapData{
		Sets: []Set{
			{SetID: "kernel", Name: "Kernel Set", Description: "OJOS 平台内核集合。", SortOrder: 0},
			{SetID: "core-capability", Name: "Core Capability Set", Description: "题目、提交、评测和基础业务能力集合。", SortOrder: 10},
			{SetID: "competition", Name: "Competition Set", Description: "竞赛能力集合，当前仅为目标架构。", SortOrder: 20},
			{SetID: "education", Name: "Education Set", Description: "教学训练能力集合，当前仅为目标架构。", SortOrder: 30},
			{SetID: "collaboration", Name: "Collaboration Set", Description: "协作社区能力集合，当前仅为目标架构。", SortOrder: 40},
			{SetID: "integration", Name: "Integration Set", Description: "第三方集成能力集合，当前仅为目标架构。", SortOrder: 50},
			{SetID: "operations", Name: "Operations Set", Description: "运维和可观测能力集合。", SortOrder: 60},
		},
		Modules: modules,
		Edges: []Edge{
			{FromModuleID: "ojos.judge-core", ToModuleID: "ojos.kernel.edge-ui-shell", EdgeType: "requires", VersionConstraint: ">=0.1.0", Required: true},
			{FromModuleID: "ojos.judge-core", ToModuleID: "ojos.kernel.identity-access", EdgeType: "requires", VersionConstraint: ">=0.1.0", Required: true},
			{FromModuleID: "ojos.judge-core", ToModuleID: "ojos.kernel.config-secret", EdgeType: "requires", VersionConstraint: ">=0.1.0", Required: true},
			{FromModuleID: "ojos.judge-core", ToModuleID: "ojos.kernel.audit-policy", EdgeType: "requires", VersionConstraint: ">=0.1.0", Required: true},
			{FromModuleID: "ojos.kernel.module-runtime", ToModuleID: "ojos.kernel.identity-access", EdgeType: "requires", VersionConstraint: ">=0.1.0", Required: true},
			{FromModuleID: "ojos.kernel.module-runtime", ToModuleID: "ojos.kernel.audit-policy", EdgeType: "requires", VersionConstraint: ">=0.1.0", Required: true},
		},
	}

	data.Components = append(data.Components,
		kernelComponent("ojos.kernel.edge-ui-shell", "frontend-shell", "frontend_shell", map[string]any{"path": "frontend/src"}),
		kernelComponent("ojos.kernel.edge-ui-shell", "router-guard", "router_guard", map[string]any{"path": "frontend/src/router"}),
		kernelComponent("ojos.kernel.identity-access", "auth-service", "backend_service", map[string]any{"path": "services/auth", "exposure": "internal"}),
		kernelComponent("ojos.kernel.identity-access", "permission-admin-pages", "frontend_route_group", map[string]any{"routes": []string{"/admin/users", "/admin/permissions", "/admin/permission-check"}}),
		kernelComponent("ojos.kernel.module-runtime", "module-registry-db", "database_schema", map[string]any{"migration": "deploy/migrations/000009_module_registry.up.sql"}),
		kernelComponent("ojos.kernel.module-runtime", "module-admin-api", "admin_api", map[string]any{"prefix": "/api/admin/modules"}),
		kernelComponent("ojos.kernel.config-secret", "internal-hmac", "security_component", map[string]any{"path": "services/shared/security/internalauth"}),
		kernelComponent("ojos.kernel.audit-policy", "permission-audit-log", "database_schema", map[string]any{"table": "permission_audit_logs"}),
	)

	data.Components = append(data.Components, judgeCoreComponents()...)
	data.Installations = append(data.Installations, installationForModules(modules, judgeManifest)...)
	data.Permissions = append(data.Permissions, kernelPermissions()...)
	data.Permissions = append(data.Permissions, judgeCorePermissions()...)
	data.Menus = append(data.Menus, judgeCoreMenus()...)
	data.FrontendRoutes = append(data.FrontendRoutes, judgeCoreFrontendRoutes()...)
	data.GatewayRoutes = append(data.GatewayRoutes, judgeCoreGatewayRoutes()...)
	data.Migrations = append(data.Migrations, judgeCoreMigrations()...)

	return data
}

func kernelComponent(moduleID, componentID, componentType string, config map[string]any) Component {
	return Component{
		ModuleID:      moduleID,
		ComponentID:   componentID,
		ComponentType: componentType,
		Status:        StatusEnabled,
		Config:        mustJSON(config),
	}
}

func installationForModules(modules []Module, judgeManifest json.RawMessage) []Installation {
	items := make([]Installation, 0, len(modules))
	for _, module := range modules {
		manifest := module.Manifest
		if module.ModuleID == "ojos.judge-core" {
			manifest = judgeManifest
		}
		items = append(items, Installation{
			ModuleID: module.ModuleID,
			Name:     module.Name,
			Version:  module.Version,
			Status:   StatusEnabled,
			Manifest: manifest,
		})
	}
	return items
}

func judgeCoreComponents() []Component {
	return []Component{
		component("problem-api", "backend_service", map[string]any{"path": "services/problem-api", "health": "/health", "exposure": "internal"}),
		component("judge-api", "backend_service", map[string]any{"path": "services/judge-api", "health": "/health", "exposure": "internal"}),
		component("judge-worker", "worker_service", map[string]any{"path": "services/judge-worker", "mode": "external-node"}),
		component("problems-storage", "storage_bucket", map[string]any{"bucket": "problems"}),
		component("submissions-storage", "storage_bucket", map[string]any{"bucket": "submissions"}),
		component("judge-artifacts-storage", "storage_bucket", map[string]any{"bucket": "judge-artifacts"}),
		component("problem-api-health", "health_check", map[string]any{"target": "problem-api"}),
		component("judge-api-health", "health_check", map[string]any{"target": "judge-api"}),
		component("worker-cluster-health", "health_check", map[string]any{"target": "worker-cluster"}),
		component("queue-health", "health_check", map[string]any{"target": "queue"}),
		component("artifact-storage-health", "health_check", map[string]any{"target": "artifact-storage"}),
		component("admin-judge-page", "frontend_route", map[string]any{"route": "/admin/judge"}),
		component("frontend-routes", "frontend_route_group", map[string]any{"routes": []string{"/problems", "/problems/new", "/problems/:id", "/problems/:id/edit", "/problems/:id/package", "/problems/:id/submit", "/submissions", "/submissions/:id", "/admin/judge"}}),
		component("gateway-routes", "gateway_route_group", map[string]any{"prefixes": []string{"/api/problem", "/api/judge", "/api/judge/worker"}}),
		component("permissions", "permission_group", map[string]any{"permissions": permissionKeys(judgeCorePermissions())}),
	}
}

func component(componentID, componentType string, config map[string]any) Component {
	return Component{
		ModuleID:      "ojos.judge-core",
		ComponentID:   componentID,
		ComponentType: componentType,
		Status:        StatusEnabled,
		Config:        mustJSON(config),
	}
}

func kernelPermissions() []Permission {
	items := []Permission{
		{ModuleID: "ojos.kernel.identity-access", PermissionKey: "system.admin", Description: "Kernel permission: system.admin"},
		{ModuleID: "ojos.kernel.module-runtime", PermissionKey: "module.install", Description: "Kernel module runtime permission: module.install"},
		{ModuleID: "ojos.kernel.module-runtime", PermissionKey: "module.enable", Description: "Kernel module runtime permission: module.enable"},
		{ModuleID: "ojos.kernel.module-runtime", PermissionKey: "module.disable", Description: "Kernel module runtime permission: module.disable"},
		{ModuleID: "ojos.kernel.module-runtime", PermissionKey: "module.configure", Description: "Kernel module runtime permission: module.configure"},
	}
	return items
}

func judgeCorePermissions() []Permission {
	keys := []string{
		"problem.view",
		"problem.view.private",
		"problem.create",
		"problem.edit",
		"problem.delete",
		"problem.manage.data",
		"problem.manage.asset",
		"judge.submit",
		"submission.view.own",
		"submission.view.all",
	}
	items := make([]Permission, 0, len(keys))
	for _, key := range keys {
		items = append(items, Permission{
			ModuleID:      "ojos.judge-core",
			PermissionKey: key,
			Description:   "Judge Core permission: " + key,
		})
	}
	return items
}

func judgeCoreMenus() []Menu {
	return []Menu{
		{ModuleID: "ojos.judge-core", MenuKey: "problems", Title: "题目", RoutePath: "/problems", SortOrder: 10, Enabled: true},
		{ModuleID: "ojos.judge-core", MenuKey: "submissions", Title: "提交", RoutePath: "/submissions", SortOrder: 20, Enabled: true},
		{ModuleID: "ojos.judge-core", MenuKey: "admin-judge", Title: "评测集群", RoutePath: "/admin/judge", SortOrder: 100, RequiredPermission: "system.admin", Enabled: true},
	}
}

func judgeCoreFrontendRoutes() []FrontendRoute {
	routes := []struct {
		path       string
		name       string
		component  string
		permission string
	}{
		{"/problems", "problems", "ProblemListView", "problem.view"},
		{"/problems/new", "problem-create", "ProblemCreateView", "problem.create"},
		{"/problems/:id", "problem-detail", "ProblemDetailView", "problem.view"},
		{"/problems/:id/edit", "problem-edit", "ProblemEditView", "problem.edit"},
		{"/problems/:id/package", "problem-package", "ProblemPackageView", "problem.manage.data"},
		{"/problems/:id/submit", "problem-submit", "ProblemSubmitView", "judge.submit"},
		{"/submissions", "submissions", "SubmissionsListView", "submission.view.own"},
		{"/submissions/:id", "submission-detail", "SubmissionDetailView", "submission.view.own"},
		{"/admin/judge", "admin-judge", "AdminJudgeView", "system.admin"},
	}
	items := make([]FrontendRoute, 0, len(routes))
	for _, route := range routes {
		items = append(items, FrontendRoute{
			ModuleID:           "ojos.judge-core",
			RoutePath:          route.path,
			RouteName:          route.name,
			ComponentKey:       route.component,
			RequiredPermission: route.permission,
			Enabled:            true,
		})
	}
	return items
}

func judgeCoreGatewayRoutes() []GatewayRoute {
	return []GatewayRoute{
		{ModuleID: "ojos.judge-core", Prefix: "/api/problem", TargetService: "problem-api", AuthMode: "user", Enabled: true},
		{ModuleID: "ojos.judge-core", Prefix: "/api/judge", TargetService: "judge-api", AuthMode: "user", Enabled: true},
		{ModuleID: "ojos.judge-core", Prefix: "/api/judge/worker", TargetService: "judge-api", AuthMode: "worker", Enabled: true},
	}
}

func judgeCoreMigrations() []Migration {
	names := []string{
		"deploy/migrations/000002_judge_schema.up.sql",
		"deploy/migrations/000004_problem_package_core.up.sql",
		"deploy/migrations/000005_judge_sandbox_storage_cleanup.up.sql",
		"deploy/migrations/000007_problem_catalog_fields.up.sql",
		"deploy/migrations/000008_worker_link.up.sql",
	}
	items := make([]Migration, 0, len(names))
	for _, name := range names {
		items = append(items, Migration{
			ModuleID:      "ojos.judge-core",
			Version:       "0.1.0",
			MigrationName: name,
			Checksum:      "declared-in-module-manifest",
		})
	}
	return items
}

func judgeCoreManifest() map[string]any {
	return map[string]any{
		"id":          "ojos.judge-core",
		"name":        "Judge Core",
		"version":     "0.1.0",
		"set":         "core-capability",
		"kind":        "feature",
		"status":      "builtin",
		"description": "题目、题目包、提交、评测、Worker Link、结果查询与评测集群管理模块。",
		"requires": map[string]any{
			"platform": ">=0.1.0",
			"modules": []string{
				"ojos.kernel.edge-ui-shell >= 0.1.0",
				"ojos.kernel.identity-access >= 0.1.0",
				"ojos.kernel.config-secret >= 0.1.0",
				"ojos.kernel.audit-policy >= 0.1.0",
			},
		},
		"provides": map[string]any{
			"permissions":      permissionKeys(judgeCorePermissions()),
			"backend_services": []map[string]any{{"id": "problem-api", "path": "services/problem-api", "health": "/health", "exposure": "internal"}, {"id": "judge-api", "path": "services/judge-api", "health": "/health", "exposure": "internal"}},
			"worker_services":  []map[string]any{{"id": "judge-worker", "path": "services/judge-worker", "mode": "external-node"}},
			"frontend": map[string]any{
				"routes": []string{"/problems", "/problems/new", "/problems/:id", "/problems/:id/edit", "/problems/:id/package", "/problems/:id/submit", "/submissions", "/submissions/:id", "/admin/judge"},
				"menus":  []map[string]any{{"key": "problems", "title": "题目", "route": "/problems"}, {"key": "submissions", "title": "提交", "route": "/submissions"}, {"key": "admin-judge", "title": "评测集群", "route": "/admin/judge", "permission": "system.admin"}},
			},
			"gateway_routes": []map[string]any{{"prefix": "/api/problem", "service": "problem-api", "auth": "user"}, {"prefix": "/api/judge", "service": "judge-api", "auth": "user"}, {"prefix": "/api/judge/worker", "service": "judge-api", "auth": "worker"}},
			"storage":        map[string]any{"buckets": []string{"problems", "submissions", "judge-artifacts"}},
			"health_checks":  []string{"problem-api", "judge-api", "worker-cluster", "queue", "artifact-storage"},
			"migrations":     []string{"deploy/migrations/000002_judge_schema.up.sql", "deploy/migrations/000004_problem_package_core.up.sql", "deploy/migrations/000005_judge_sandbox_storage_cleanup.up.sql", "deploy/migrations/000007_problem_catalog_fields.up.sql", "deploy/migrations/000008_worker_link.up.sql"},
		},
	}
}

func permissionKeys(items []Permission) []string {
	keys := make([]string, 0, len(items))
	for _, item := range items {
		keys = append(keys, item.PermissionKey)
	}
	return keys
}

func mustJSON(value any) json.RawMessage {
	data, err := json.Marshal(value)
	if err != nil {
		panic(err)
	}
	return data
}
