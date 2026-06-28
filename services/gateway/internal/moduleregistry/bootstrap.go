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
	builtinManifest := func(kind string, description string) json.RawMessage {
		return mustJSON(map[string]any{
			"schema_version": 1,
			"kind":           kind,
			"status":         "builtin",
			"description":    description,
			"note":           "Builtin module registered by Kernel Module Registry.",
		})
	}

	judgeManifest := mustJSON(judgeCoreManifest())
	kernelModules := []Module{
		{
			ModuleID:    "ojos.kernel.installer",
			SetID:       "kernel",
			Name:        "Kernel Installer",
			Version:     "0.1.0",
			Status:      StatusEnabled,
			Kind:        KindKernel,
			Description: "Module package validation, planning, install operations, lifecycle operations, and operation locks.",
			Manifest:    builtinManifest(KindKernel, "module package validation, planning, install operations, lifecycle operations, and operation locks"),
		},
		{
			ModuleID:    "ojos.kernel.module-runtime",
			SetID:       "kernel",
			Name:        "Module Runtime",
			Version:     "0.1.0",
			Status:      StatusEnabled,
			Kind:        KindKernel,
			Description: "Runtime snapshot, dynamic module surface aggregation, lifecycle state, and hotplug contracts.",
			Manifest:    builtinManifest(KindKernel, "runtime snapshot, dynamic module surface aggregation, lifecycle state, and hotplug contracts"),
		},
		{
			ModuleID:    "ojos.kernel.module-registry",
			SetID:       "kernel",
			Name:        "Module Registry",
			Version:     "0.1.0",
			Status:      StatusEnabled,
			Kind:        KindKernel,
			Description: "Registry tables for modules, dependencies, permissions, menus, routes, components, and installation state.",
			Manifest:    builtinManifest(KindKernel, "registry tables for modules, dependencies, permissions, menus, routes, components, and installation state"),
		},
		{
			ModuleID:    "ojos.kernel.topology",
			SetID:       "kernel",
			Name:        "Module Topology",
			Version:     "0.1.0",
			Status:      StatusEnabled,
			Kind:        KindKernel,
			Description: "Kernel-owned module, service, dependency, runtime, and deployment topology aggregation.",
			Manifest:    builtinManifest(KindKernel, "kernel-owned module, service, dependency, runtime, and deployment topology aggregation"),
		},
		{
			ModuleID:    "ojos.kernel.policy",
			SetID:       "kernel",
			Name:        "Kernel Policy",
			Version:     "0.1.0",
			Status:      StatusEnabled,
			Kind:        KindKernel,
			Description: "Kernel safety policy for protected modules, admin permissions, and lifecycle boundaries.",
			Manifest:    builtinManifest(KindKernel, "kernel safety policy for protected modules, admin permissions, and lifecycle boundaries"),
		},
		{
			ModuleID:    "ojos.kernel.audit",
			SetID:       "kernel",
			Name:        "Kernel Audit",
			Version:     "0.1.0",
			Status:      StatusEnabled,
			Kind:        KindKernel,
			Description: "Audit log contract for module operations and administrative security events.",
			Manifest:    builtinManifest(KindKernel, "audit log contract for module operations and administrative security events"),
		},
		{
			ModuleID:    "ojos.kernel.config",
			SetID:       "kernel",
			Name:        "Kernel Config",
			Version:     "0.1.0",
			Status:      StatusEnabled,
			Kind:        KindKernel,
			Description: "Configuration and secret boundary contract; secrets stay outside module manifests and packages.",
			Manifest:    builtinManifest(KindKernel, "configuration and secret boundary contract"),
		},
		{
			ModuleID:    "ojos.kernel.health",
			SetID:       "kernel",
			Name:        "Kernel Health",
			Version:     "0.1.0",
			Status:      StatusEnabled,
			Kind:        KindKernel,
			Description: "Health check aggregation contract for modules, services, workers, and runtime state.",
			Manifest:    builtinManifest(KindKernel, "health check aggregation contract for modules, services, workers, and runtime state"),
		},
	}

	platformModules := []Module{
		{
			ModuleID:    "ojos.platform.gateway",
			SetID:       "platform",
			Name:        "Gateway",
			Version:     "0.1.0",
			Status:      StatusEnabled,
			Kind:        KindPlatform,
			Description: "Public edge adapter for JWT, admin authorization, internal auth, route proxying, and error mapping.",
			Manifest:    builtinManifest(KindPlatform, "public edge adapter for JWT, admin authorization, internal auth, route proxying, and error mapping"),
		},
		{
			ModuleID:    "ojos.platform.web-shell",
			SetID:       "platform",
			Name:        "Web Shell",
			Version:     "0.1.0",
			Status:      StatusEnabled,
			Kind:        KindPlatform,
			Description: "Frontend shell for login state, layout, module menus, module entry points, and generic admin views.",
			Manifest:    builtinManifest(KindPlatform, "frontend shell for login state, layout, module menus, module entry points, and generic admin views"),
		},
		{
			ModuleID:    "ojos.platform.identity-access",
			SetID:       "platform",
			Name:        "Identity Access",
			Version:     "0.1.0",
			Status:      StatusEnabled,
			Kind:        KindPlatform,
			Description: "Authentication, users, roles, permissions, scoped bindings, and permission audit surfaces.",
			Manifest:    builtinManifest(KindPlatform, "authentication, users, roles, permissions, scoped bindings, and permission audit surfaces"),
		},
		{
			ModuleID:    "ojos.platform.storage",
			SetID:       "platform",
			Name:        "Platform Storage",
			Version:     "0.1.0",
			Status:      StatusEnabled,
			Kind:        KindPlatform,
			Description: "Storage buckets and persistence boundary used by feature modules.",
			Manifest:    builtinManifest(KindPlatform, "storage buckets and persistence boundary used by feature modules"),
		},
		{
			ModuleID:    "ojos.platform.observability",
			SetID:       "platform",
			Name:        "Platform Observability",
			Version:     "0.1.0",
			Status:      StatusEnabled,
			Kind:        KindPlatform,
			Description: "Operational health, metrics-ready state, and diagnostics surfaces.",
			Manifest:    builtinManifest(KindPlatform, "operational health, metrics-ready state, and diagnostics surfaces"),
		},
	}

	modules := append([]Module{}, kernelModules...)
	modules = append(modules, platformModules...)
	modules = append(modules, Module{
		ModuleID:    "ojos.judge-core",
		SetID:       "core-capability",
		Name:        "Judge Core",
		Version:     "0.1.0",
		Status:      StatusEnabled,
		Kind:        KindFeature,
		Description: "Problem catalog, packages, submissions, judging, Worker Link, result storage, and judge cluster administration.",
		Manifest:    judgeManifest,
	})

	data := BootstrapData{
		Sets: []Set{
			{SetID: "kernel", Name: "Kernel Set", Description: "OJOS Kernel 能力：installer、runtime、registry、topology、policy、audit、config 和 health。", SortOrder: 0},
			{SetID: "platform", Name: "Platform Set", Description: "平台 adapter 和基础服务：gateway、web shell、identity、storage 和 observability。", SortOrder: 5},
			{SetID: "core-capability", Name: "Core Capability Set", Description: "题库、提交、评测和在线评测基础能力模块。", SortOrder: 10},
			{SetID: "competition", Name: "Competition Set", Description: "竞赛能力规划集合；v0.1.0 未实现 Contest。", SortOrder: 20},
			{SetID: "education", Name: "Education Set", Description: "训练和教学能力规划集合；v0.1.0 未实现 Training。", SortOrder: 30},
			{SetID: "collaboration", Name: "Collaboration Set", Description: "协作能力规划集合；v0.1.0 未实现 group、discussion、clarification、print 或 balloon。", SortOrder: 40},
			{SetID: "integration", Name: "Integration Set", Description: "外部集成能力规划集合；v0.1.0 未实现 Remote OJ。", SortOrder: 50},
			{SetID: "operations", Name: "Operations Set", Description: "运维和可观测能力。", SortOrder: 60},
		},
		Modules: modules,
		Edges: []Edge{
			{FromModuleID: "ojos.kernel.installer", ToModuleID: "ojos.kernel.module-registry", EdgeType: "requires", VersionConstraint: ">=0.1.0", Required: true},
			{FromModuleID: "ojos.kernel.installer", ToModuleID: "ojos.kernel.policy", EdgeType: "requires", VersionConstraint: ">=0.1.0", Required: true},
			{FromModuleID: "ojos.kernel.installer", ToModuleID: "ojos.kernel.audit", EdgeType: "requires", VersionConstraint: ">=0.1.0", Required: true},
			{FromModuleID: "ojos.kernel.module-runtime", ToModuleID: "ojos.kernel.module-registry", EdgeType: "requires", VersionConstraint: ">=0.1.0", Required: true},
			{FromModuleID: "ojos.kernel.topology", ToModuleID: "ojos.kernel.module-runtime", EdgeType: "requires", VersionConstraint: ">=0.1.0", Required: true},
			{FromModuleID: "ojos.platform.gateway", ToModuleID: "ojos.kernel.module-runtime", EdgeType: "requires", VersionConstraint: ">=0.1.0", Required: true},
			{FromModuleID: "ojos.platform.gateway", ToModuleID: "ojos.kernel.policy", EdgeType: "requires", VersionConstraint: ">=0.1.0", Required: true},
			{FromModuleID: "ojos.platform.gateway", ToModuleID: "ojos.kernel.config", EdgeType: "requires", VersionConstraint: ">=0.1.0", Required: true},
			{FromModuleID: "ojos.platform.web-shell", ToModuleID: "ojos.platform.gateway", EdgeType: "requires", VersionConstraint: ">=0.1.0", Required: true},
			{FromModuleID: "ojos.platform.web-shell", ToModuleID: "ojos.kernel.module-runtime", EdgeType: "requires", VersionConstraint: ">=0.1.0", Required: true},
			{FromModuleID: "ojos.platform.identity-access", ToModuleID: "ojos.kernel.policy", EdgeType: "requires", VersionConstraint: ">=0.1.0", Required: true},
			{FromModuleID: "ojos.platform.identity-access", ToModuleID: "ojos.kernel.audit", EdgeType: "requires", VersionConstraint: ">=0.1.0", Required: true},
			{FromModuleID: "ojos.platform.storage", ToModuleID: "ojos.kernel.config", EdgeType: "requires", VersionConstraint: ">=0.1.0", Required: true},
			{FromModuleID: "ojos.platform.observability", ToModuleID: "ojos.kernel.health", EdgeType: "requires", VersionConstraint: ">=0.1.0", Required: true},
			{FromModuleID: "ojos.judge-core", ToModuleID: "ojos.platform.web-shell", EdgeType: "requires", VersionConstraint: ">=0.1.0", Required: true},
			{FromModuleID: "ojos.judge-core", ToModuleID: "ojos.platform.identity-access", EdgeType: "requires", VersionConstraint: ">=0.1.0", Required: true},
			{FromModuleID: "ojos.judge-core", ToModuleID: "ojos.platform.storage", EdgeType: "requires", VersionConstraint: ">=0.1.0", Required: true},
			{FromModuleID: "ojos.judge-core", ToModuleID: "ojos.platform.gateway", EdgeType: "requires", VersionConstraint: ">=0.1.0", Required: true},
			{FromModuleID: "ojos.judge-core", ToModuleID: "ojos.kernel.module-runtime", EdgeType: "requires", VersionConstraint: ">=0.1.0", Required: true},
			{FromModuleID: "ojos.judge-core", ToModuleID: "ojos.kernel.audit", EdgeType: "requires", VersionConstraint: ">=0.1.0", Required: true},
			{FromModuleID: "ojos.judge-core", ToModuleID: "ojos.kernel.health", EdgeType: "requires", VersionConstraint: ">=0.1.0", Required: true},
		},
	}

	data.Components = append(data.Components,
		kernelComponent("ojos.kernel.installer", "installer-core", "rust_crate", map[string]any{"path": "kernel/installer/core"}),
		kernelComponent("ojos.kernel.installer", "installer-service", "backend_service", map[string]any{"path": "kernel/installer/service", "health": "/health", "exposure": "internal"}),
		kernelComponent("ojos.kernel.installer", "installer-cli", "tool", map[string]any{"path": "kernel/installer/cli", "tool_name": "ojosctl"}),
		kernelComponent("ojos.kernel.module-runtime", "runtime-snapshot", "kernel_api", map[string]any{"path": "/api/admin/modules/runtime-snapshot"}),
		kernelComponent("ojos.kernel.module-runtime", "runtime-reader", "go_package", map[string]any{"path": "services/gateway/internal/kernel/moduleruntime"}),
		kernelComponent("ojos.kernel.module-registry", "module-registry-db", "database_schema", map[string]any{"migration": "deploy/migrations/000009_module_registry.up.sql"}),
		kernelComponent("ojos.kernel.module-registry", "module-admin-api", "admin_api", map[string]any{"prefix": "/api/admin/modules"}),
		kernelComponent("ojos.kernel.topology", "topology-snapshot", "kernel_api", map[string]any{"path": "/api/admin/modules/topology"}),
		kernelComponent("ojos.kernel.policy", "protected-module-policy", "policy", map[string]any{"kernel": "no_disable_no_uninstall", "platform": "protected_by_default", "judge_core": "protected"}),
		kernelComponent("ojos.kernel.audit", "permission-audit-log", "database_schema", map[string]any{"table": "permission_audit_logs"}),
		kernelComponent("ojos.kernel.audit", "module-operation-history", "database_schema", map[string]any{"table": "module_operations"}),
		kernelComponent("ojos.kernel.config", "internal-hmac", "security_component", map[string]any{"path": "services/shared/security/internalauth"}),
		kernelComponent("ojos.kernel.health", "health-aggregator", "kernel_api", map[string]any{"path": "/api/admin/health"}),
		kernelComponent("ojos.platform.gateway", "gateway-app", "backend_service", map[string]any{"path": "services/gateway", "exposure": "public"}),
		kernelComponent("ojos.platform.web-shell", "web-shell-app", "frontend_shell", map[string]any{"path": "frontend/src", "future_path": "apps/web-shell"}),
		kernelComponent("ojos.platform.web-shell", "router-guard", "router_guard", map[string]any{"path": "frontend/src/router"}),
		kernelComponent("ojos.platform.identity-access", "auth-service", "backend_service", map[string]any{"path": "services/auth", "exposure": "internal"}),
		kernelComponent("ojos.platform.identity-access", "permission-admin-pages", "frontend_route_group", map[string]any{"routes": []string{"/admin/users", "/admin/permissions", "/admin/permission-check"}}),
		kernelComponent("ojos.platform.storage", "storage-buckets", "storage_contract", map[string]any{"buckets": []string{"problems", "submissions", "judge-artifacts"}}),
		kernelComponent("ojos.platform.observability", "admin-health", "health_check", map[string]any{"path": "/api/admin/health"}),
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
		component("problem-api", "backend_service", map[string]any{"path": "services/problem-api", "future_path": "modules/judge-core/services/problem-api", "health": "/health", "exposure": "internal", "lifecycle": "managed", "trusted_runtime": "compose", "compose_service": "problem-api", "health_check_id": "problem-api-health", "routes": []string{"/api/problem"}, "required": true}),
		component("judge-api", "backend_service", map[string]any{"path": "services/judge-api", "future_path": "modules/judge-core/services/judge-api", "health": "/health", "exposure": "internal", "lifecycle": "managed", "trusted_runtime": "compose", "compose_service": "judge-api", "health_check_id": "judge-api-health", "routes": []string{"/api/judge", "/api/judge/worker"}, "required": true}),
		component("judge-worker", "worker_service", map[string]any{"path": "services/judge-worker", "future_path": "modules/judge-core/workers/judge-worker", "mode": "external-node", "lifecycle": "managed", "trusted_runtime": "compose", "compose_service": "judge-worker", "health_check_id": "worker-cluster-health", "required": false}),
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
	return []Permission{
		{ModuleID: "ojos.platform.identity-access", PermissionKey: "system.admin", Description: "Platform permission: system.admin"},
		{ModuleID: "ojos.kernel.installer", PermissionKey: "module.install", Description: "Kernel installer permission: module.install"},
		{ModuleID: "ojos.kernel.installer", PermissionKey: "module.enable", Description: "Kernel installer permission: module.enable"},
		{ModuleID: "ojos.kernel.installer", PermissionKey: "module.disable", Description: "Kernel installer permission: module.disable"},
		{ModuleID: "ojos.kernel.installer", PermissionKey: "module.configure", Description: "Kernel installer permission: module.configure"},
		{ModuleID: "ojos.kernel.module-runtime", PermissionKey: "module.runtime.read", Description: "Kernel module runtime permission: module.runtime.read"},
	}
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
		{ModuleID: "ojos.judge-core", MenuKey: "problems", Title: "Problems", RoutePath: "/problems", SortOrder: 10, Enabled: true},
		{ModuleID: "ojos.judge-core", MenuKey: "submissions", Title: "Submissions", RoutePath: "/submissions", SortOrder: 20, Enabled: true},
		{ModuleID: "ojos.judge-core", MenuKey: "admin-judge", Title: "Judge Cluster", RoutePath: "/admin/judge", SortOrder: 100, RequiredPermission: "system.admin", Enabled: true},
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
		"schema_version": 1,
		"id":             "ojos.judge-core",
		"name":           "Judge Core",
		"version":        "0.1.0",
		"set":            "core-capability",
		"kind":           "feature",
		"status":         "builtin",
		"description":    "Problem catalog, packages, submissions, judging, Worker Link, result storage, and judge cluster administration.",
		"compatibility": map[string]any{
			"platform":  ">=0.1.0",
			"installer": ">=0.1.0",
		},
		"requires": map[string]any{
			"modules": []map[string]any{
				{"id": "ojos.platform.web-shell", "version": ">=0.1.0"},
				{"id": "ojos.platform.identity-access", "version": ">=0.1.0"},
				{"id": "ojos.platform.storage", "version": ">=0.1.0"},
				{"id": "ojos.platform.gateway", "version": ">=0.1.0"},
				{"id": "ojos.kernel.module-runtime", "version": ">=0.1.0"},
				{"id": "ojos.kernel.audit", "version": ">=0.1.0"},
				{"id": "ojos.kernel.health", "version": ">=0.1.0"},
			},
		},
		"provides": map[string]any{
			"permissions": manifestPermissions(judgeCorePermissions()),
			"roles":       []map[string]any{},
			"components":  manifestComponents(judgeCoreComponents()),
			"services": []map[string]any{
				{"id": "problem-api", "name": "Problem API", "kind": "http", "lifecycle": "managed", "trusted_runtime": "compose", "compose_service": "problem-api", "health_check_id": "problem-api-health", "routes": []string{"/api/problem"}, "required": true, "path": "services/problem-api", "health": "/health", "exposure": "internal"},
				{"id": "judge-api", "name": "Judge API", "kind": "http", "lifecycle": "managed", "trusted_runtime": "compose", "compose_service": "judge-api", "health_check_id": "judge-api-health", "routes": []string{"/api/judge", "/api/judge/worker"}, "required": true, "path": "services/judge-api", "health": "/health", "exposure": "internal"},
			},
			"workers": []map[string]any{
				{"id": "judge-worker", "name": "Judge Worker", "kind": "worker", "lifecycle": "managed", "trusted_runtime": "compose", "compose_service": "judge-worker", "health_check_id": "worker-cluster-health", "required": false, "path": "services/judge-worker", "mode": "external-node"},
			},
			"frontend_routes": manifestFrontendRoutes(judgeCoreFrontendRoutes()),
			"menus":           manifestMenus(judgeCoreMenus()),
			"gateway_routes":  manifestGatewayRoutes(judgeCoreGatewayRoutes()),
			"storage":         map[string]any{"buckets": []string{"problems", "submissions", "judge-artifacts"}},
			"storage_buckets": []map[string]any{{"id": "problems"}, {"id": "submissions"}, {"id": "judge-artifacts"}},
			"health_checks": []map[string]any{
				{"id": "problem-api-health", "type": "http", "optional": false},
				{"id": "judge-api-health", "type": "http", "optional": false},
				{"id": "worker-cluster-health", "type": "metadata", "optional": false},
				{"id": "queue-health", "type": "metadata", "optional": false},
				{"id": "artifact-storage-health", "type": "metadata", "optional": false},
			},
			"migrations": []map[string]any{
				{"up": "deploy/migrations/000002_judge_schema.up.sql", "down": "deploy/migrations/000002_judge_schema.down.sql"},
				{"up": "deploy/migrations/000004_problem_package_core.up.sql", "down": "deploy/migrations/000004_problem_package_core.down.sql"},
				{"up": "deploy/migrations/000005_judge_sandbox_storage_cleanup.up.sql", "down": "deploy/migrations/000005_judge_sandbox_storage_cleanup.down.sql"},
				{"up": "deploy/migrations/000007_problem_catalog_fields.up.sql", "down": "deploy/migrations/000007_problem_catalog_fields.down.sql"},
				{"up": "deploy/migrations/000008_worker_link.up.sql", "down": "deploy/migrations/000008_worker_link.down.sql"},
			},
			"events":         map[string]any{"publishes": []string{"judge.submission.created", "judge.submission.finished"}, "subscribes": []string{}},
			"scheduled_jobs": []map[string]any{},
			"admin_panels":   []map[string]any{{"id": "judge-cluster", "route_path": "/admin/judge", "required_permission": "system.admin"}},
			"topology": map[string]any{
				"nodes": []map[string]any{
					{"id": "problem-api", "type": "service"},
					{"id": "judge-api", "type": "service"},
					{"id": "judge-worker", "type": "worker"},
				},
				"edges": []map[string]any{
					{"from": "gateway", "to": "problem-api", "type": "routes"},
					{"from": "gateway", "to": "judge-api", "type": "routes"},
					{"from": "judge-api", "to": "judge-worker", "type": "worker-link"},
				},
			},
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

func manifestPermissions(items []Permission) []map[string]any {
	out := make([]map[string]any, 0, len(items))
	for _, item := range items {
		out = append(out, map[string]any{"key": item.PermissionKey, "description": item.Description})
	}
	return out
}

func manifestComponents(items []Component) []map[string]any {
	out := make([]map[string]any, 0, len(items))
	for _, item := range items {
		out = append(out, map[string]any{"id": item.ComponentID, "type": item.ComponentType, "status": item.Status, "config": rawJSONMap(item.Config)})
	}
	return out
}

func manifestFrontendRoutes(items []FrontendRoute) []map[string]any {
	out := make([]map[string]any, 0, len(items))
	for _, item := range items {
		out = append(out, map[string]any{
			"path":                item.RoutePath,
			"name":                item.RouteName,
			"component_key":       item.ComponentKey,
			"required_permission": item.RequiredPermission,
			"enabled":             item.Enabled,
		})
	}
	return out
}

func manifestMenus(items []Menu) []map[string]any {
	out := make([]map[string]any, 0, len(items))
	for _, item := range items {
		out = append(out, map[string]any{
			"key":                 item.MenuKey,
			"title":               item.Title,
			"route_path":          item.RoutePath,
			"sort_order":          item.SortOrder,
			"required_permission": item.RequiredPermission,
			"enabled":             item.Enabled,
		})
	}
	return out
}

func manifestGatewayRoutes(items []GatewayRoute) []map[string]any {
	out := make([]map[string]any, 0, len(items))
	for _, item := range items {
		out = append(out, map[string]any{
			"prefix":     item.Prefix,
			"service_id": item.TargetService,
			"auth_mode":  item.AuthMode,
			"enabled":    item.Enabled,
		})
	}
	return out
}

func rawJSONMap(data json.RawMessage) map[string]any {
	var value map[string]any
	if err := json.Unmarshal(data, &value); err != nil {
		return map[string]any{}
	}
	return value
}

func mustJSON(value any) json.RawMessage {
	data, err := json.Marshal(value)
	if err != nil {
		panic(err)
	}
	return data
}
