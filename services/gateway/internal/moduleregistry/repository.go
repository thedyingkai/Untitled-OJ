package moduleregistry

import (
	"context"
	"encoding/json"
	"time"

	"github.com/jackc/pgx/v5"
	"github.com/jackc/pgx/v5/pgxpool"
)

type Repository struct {
	db *pgxpool.Pool
}

func NewRepository(db *pgxpool.Pool) *Repository {
	return &Repository{db: db}
}

func (r *Repository) UpsertSet(ctx context.Context, item Set) error {
	_, err := r.db.Exec(ctx, `
INSERT INTO module_sets(set_id, name, description, sort_order)
VALUES($1,$2,$3,$4)
ON CONFLICT(set_id) DO UPDATE SET
    name = EXCLUDED.name,
    description = EXCLUDED.description,
    sort_order = EXCLUDED.sort_order
`, item.SetID, item.Name, item.Description, item.SortOrder)
	return err
}

func (r *Repository) UpsertModule(ctx context.Context, item Module) error {
	manifest := item.Manifest
	if len(manifest) == 0 {
		manifest = json.RawMessage(`{}`)
	}
	_, err := r.db.Exec(ctx, `
INSERT INTO module_nodes(module_id, set_id, name, version, status, kind, description, manifest)
VALUES($1,$2,$3,$4,$5,$6,$7,$8)
ON CONFLICT(module_id) DO UPDATE SET
    set_id = EXCLUDED.set_id,
    name = EXCLUDED.name,
    version = EXCLUDED.version,
    status = EXCLUDED.status,
    kind = EXCLUDED.kind,
    description = EXCLUDED.description,
    manifest = EXCLUDED.manifest,
    updated_at = NOW()
`, item.ModuleID, item.SetID, item.Name, item.Version, item.Status, item.Kind, item.Description, []byte(manifest))
	return err
}

func (r *Repository) UpsertEdge(ctx context.Context, item Edge) error {
	_, err := r.db.Exec(ctx, `
INSERT INTO module_edges(from_module_id, to_module_id, edge_type, version_constraint, required)
VALUES($1,$2,$3,$4,$5)
ON CONFLICT(from_module_id, to_module_id, edge_type) DO UPDATE SET
    version_constraint = EXCLUDED.version_constraint,
    required = EXCLUDED.required
`, item.FromModuleID, item.ToModuleID, item.EdgeType, item.VersionConstraint, item.Required)
	return err
}

func (r *Repository) UpsertComponent(ctx context.Context, item Component) error {
	config := item.Config
	if len(config) == 0 {
		config = json.RawMessage(`{}`)
	}
	_, err := r.db.Exec(ctx, `
INSERT INTO module_components(module_id, component_id, component_type, status, config)
VALUES($1,$2,$3,$4,$5)
ON CONFLICT(module_id, component_id) DO UPDATE SET
    component_type = EXCLUDED.component_type,
    status = EXCLUDED.status,
    config = EXCLUDED.config,
    updated_at = NOW()
`, item.ModuleID, item.ComponentID, item.ComponentType, item.Status, []byte(config))
	return err
}

func (r *Repository) UpsertInstallation(ctx context.Context, item Installation) error {
	manifest := item.Manifest
	if len(manifest) == 0 {
		manifest = json.RawMessage(`{}`)
	}
	_, err := r.db.Exec(ctx, `
INSERT INTO module_installations(module_id, name, version, status, manifest, enabled_at)
VALUES($1,$2,$3,$4,$5,CASE WHEN $4 = 'ENABLED' THEN NOW() ELSE NULL END)
ON CONFLICT(module_id) DO UPDATE SET
    name = EXCLUDED.name,
    version = EXCLUDED.version,
    status = EXCLUDED.status,
    manifest = EXCLUDED.manifest,
    updated_at = NOW(),
    enabled_at = CASE WHEN EXCLUDED.status = 'ENABLED' THEN COALESCE(module_installations.enabled_at, NOW()) ELSE module_installations.enabled_at END,
    disabled_at = CASE WHEN EXCLUDED.status = 'DISABLED' THEN COALESCE(module_installations.disabled_at, NOW()) ELSE NULL END
`, item.ModuleID, item.Name, item.Version, item.Status, []byte(manifest))
	return err
}

func (r *Repository) UpsertPermission(ctx context.Context, item Permission) error {
	_, err := r.db.Exec(ctx, `
INSERT INTO module_permissions(module_id, permission_key, description)
VALUES($1,$2,$3)
ON CONFLICT(permission_key) DO UPDATE SET
    module_id = EXCLUDED.module_id,
    description = EXCLUDED.description
`, item.ModuleID, item.PermissionKey, item.Description)
	return err
}

func (r *Repository) UpsertMenu(ctx context.Context, item Menu) error {
	_, err := r.db.Exec(ctx, `
INSERT INTO module_menus(module_id, menu_key, title, route_path, icon, parent_key, sort_order, required_permission, enabled)
VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9)
ON CONFLICT(menu_key) DO UPDATE SET
    module_id = EXCLUDED.module_id,
    title = EXCLUDED.title,
    route_path = EXCLUDED.route_path,
    icon = EXCLUDED.icon,
    parent_key = EXCLUDED.parent_key,
    sort_order = EXCLUDED.sort_order,
    required_permission = EXCLUDED.required_permission,
    enabled = EXCLUDED.enabled
`, item.ModuleID, item.MenuKey, item.Title, item.RoutePath, item.Icon, item.ParentKey, item.SortOrder, item.RequiredPermission, item.Enabled)
	return err
}

func (r *Repository) UpsertFrontendRoute(ctx context.Context, item FrontendRoute) error {
	_, err := r.db.Exec(ctx, `
INSERT INTO module_frontend_routes(module_id, route_path, route_name, component_key, required_permission, enabled)
VALUES($1,$2,$3,$4,$5,$6)
ON CONFLICT(module_id, route_path) DO UPDATE SET
    route_name = EXCLUDED.route_name,
    component_key = EXCLUDED.component_key,
    required_permission = EXCLUDED.required_permission,
    enabled = EXCLUDED.enabled
`, item.ModuleID, item.RoutePath, item.RouteName, item.ComponentKey, item.RequiredPermission, item.Enabled)
	return err
}

func (r *Repository) UpsertGatewayRoute(ctx context.Context, item GatewayRoute) error {
	_, err := r.db.Exec(ctx, `
INSERT INTO module_gateway_routes(module_id, prefix, target_service, auth_mode, enabled)
VALUES($1,$2,$3,$4,$5)
ON CONFLICT(prefix) DO UPDATE SET
    module_id = EXCLUDED.module_id,
    target_service = EXCLUDED.target_service,
    auth_mode = EXCLUDED.auth_mode,
    enabled = EXCLUDED.enabled
`, item.ModuleID, item.Prefix, item.TargetService, item.AuthMode, item.Enabled)
	return err
}

func (r *Repository) UpsertMigration(ctx context.Context, item Migration) error {
	_, err := r.db.Exec(ctx, `
INSERT INTO module_migrations(module_id, version, migration_name, checksum)
VALUES($1,$2,$3,$4)
ON CONFLICT(module_id, migration_name) DO UPDATE SET
    version = EXCLUDED.version,
    checksum = EXCLUDED.checksum
`, item.ModuleID, item.Version, item.MigrationName, item.Checksum)
	return err
}

func (r *Repository) ListSets(ctx context.Context) ([]Set, error) {
	rows, err := r.db.Query(ctx, `
SELECT set_id, name, description, sort_order
FROM module_sets
ORDER BY sort_order, set_id
`)
	if err != nil {
		return nil, err
	}
	defer rows.Close()
	return scanSets(rows)
}

func (r *Repository) ListModules(ctx context.Context) ([]Module, error) {
	rows, err := r.db.Query(ctx, moduleSelectSQL+`
ORDER BY set_id, module_id
`)
	if err != nil {
		return nil, err
	}
	defer rows.Close()
	return scanModules(rows)
}

func (r *Repository) Topology(ctx context.Context) (Topology, error) {
	sets, err := r.ListSets(ctx)
	if err != nil {
		return Topology{}, err
	}
	nodes, err := r.ListModules(ctx)
	if err != nil {
		return Topology{}, err
	}
	edges, err := r.ListEdges(ctx)
	if err != nil {
		return Topology{}, err
	}
	components, err := r.ListComponents(ctx)
	if err != nil {
		return Topology{}, err
	}
	return Topology{Sets: sets, Nodes: nodes, Edges: edges, Components: components}, nil
}

func (r *Repository) Detail(ctx context.Context, moduleID string) (Detail, error) {
	module, err := r.GetModule(ctx, moduleID)
	if err != nil {
		return Detail{}, err
	}
	dependencies, err := r.ListDependencies(ctx, moduleID)
	if err != nil {
		return Detail{}, err
	}
	dependents, err := r.ListDependents(ctx, moduleID)
	if err != nil {
		return Detail{}, err
	}
	components, err := r.ListComponentsByModule(ctx, moduleID)
	if err != nil {
		return Detail{}, err
	}
	permissions, err := r.ListPermissionsByModule(ctx, moduleID)
	if err != nil {
		return Detail{}, err
	}
	menus, err := r.ListMenusByModule(ctx, moduleID)
	if err != nil {
		return Detail{}, err
	}
	frontendRoutes, err := r.ListFrontendRoutesByModule(ctx, moduleID)
	if err != nil {
		return Detail{}, err
	}
	gatewayRoutes, err := r.ListGatewayRoutesByModule(ctx, moduleID)
	if err != nil {
		return Detail{}, err
	}
	installations, err := r.ListInstallationsByModule(ctx, moduleID)
	if err != nil {
		return Detail{}, err
	}

	healthChecks := make([]Component, 0)
	for _, component := range components {
		if component.ComponentType == "health_check" {
			healthChecks = append(healthChecks, component)
		}
	}

	return Detail{
		Module:         module,
		Dependencies:   dependencies,
		Dependents:     dependents,
		Components:     components,
		Permissions:    permissions,
		Menus:          menus,
		FrontendRoutes: frontendRoutes,
		GatewayRoutes:  gatewayRoutes,
		Installations:  installations,
		HealthChecks:   healthChecks,
	}, nil
}

func (r *Repository) GetModule(ctx context.Context, moduleID string) (Module, error) {
	row := r.db.QueryRow(ctx, moduleSelectSQL+`
WHERE module_id = $1
`, moduleID)
	return scanModule(row)
}

func (r *Repository) ListEdges(ctx context.Context) ([]Edge, error) {
	rows, err := r.db.Query(ctx, `
SELECT from_module_id, to_module_id, edge_type, version_constraint, required
FROM module_edges
ORDER BY from_module_id, to_module_id, edge_type
`)
	if err != nil {
		return nil, err
	}
	defer rows.Close()
	return scanEdges(rows)
}

func (r *Repository) ListDependencies(ctx context.Context, moduleID string) ([]Edge, error) {
	rows, err := r.db.Query(ctx, `
SELECT from_module_id, to_module_id, edge_type, version_constraint, required
FROM module_edges
WHERE from_module_id = $1
ORDER BY to_module_id, edge_type
`, moduleID)
	if err != nil {
		return nil, err
	}
	defer rows.Close()
	return scanEdges(rows)
}

func (r *Repository) ListDependents(ctx context.Context, moduleID string) ([]Edge, error) {
	rows, err := r.db.Query(ctx, `
SELECT from_module_id, to_module_id, edge_type, version_constraint, required
FROM module_edges
WHERE to_module_id = $1
ORDER BY from_module_id, edge_type
`, moduleID)
	if err != nil {
		return nil, err
	}
	defer rows.Close()
	return scanEdges(rows)
}

func (r *Repository) ListComponents(ctx context.Context) ([]Component, error) {
	rows, err := r.db.Query(ctx, `
SELECT module_id, component_id, component_type, status, config
FROM module_components
ORDER BY module_id, component_type, component_id
`)
	if err != nil {
		return nil, err
	}
	defer rows.Close()
	return scanComponents(rows)
}

func (r *Repository) ListPermissions(ctx context.Context) ([]Permission, error) {
	rows, err := r.db.Query(ctx, `
SELECT module_id, permission_key, description
FROM module_permissions
ORDER BY module_id, permission_key
`)
	if err != nil {
		return nil, err
	}
	defer rows.Close()
	items := make([]Permission, 0)
	for rows.Next() {
		var item Permission
		if err := rows.Scan(&item.ModuleID, &item.PermissionKey, &item.Description); err != nil {
			return nil, err
		}
		items = append(items, item)
	}
	return items, rows.Err()
}

func (r *Repository) ListMenus(ctx context.Context) ([]Menu, error) {
	rows, err := r.db.Query(ctx, `
SELECT module_id, menu_key, title, route_path, icon, parent_key, sort_order, required_permission, enabled
FROM module_menus
ORDER BY sort_order, menu_key
`)
	if err != nil {
		return nil, err
	}
	defer rows.Close()
	return scanMenus(rows)
}

func (r *Repository) ListFrontendRoutes(ctx context.Context) ([]FrontendRoute, error) {
	rows, err := r.db.Query(ctx, `
SELECT module_id, route_path, route_name, component_key, required_permission, enabled
FROM module_frontend_routes
ORDER BY route_path
`)
	if err != nil {
		return nil, err
	}
	defer rows.Close()
	return scanFrontendRoutes(rows)
}

func (r *Repository) ListGatewayRoutes(ctx context.Context) ([]GatewayRoute, error) {
	rows, err := r.db.Query(ctx, `
SELECT module_id, prefix, target_service, auth_mode, enabled
FROM module_gateway_routes
ORDER BY prefix
`)
	if err != nil {
		return nil, err
	}
	defer rows.Close()
	return scanGatewayRoutes(rows)
}

func (r *Repository) ListComponentsByModule(ctx context.Context, moduleID string) ([]Component, error) {
	rows, err := r.db.Query(ctx, `
SELECT module_id, component_id, component_type, status, config
FROM module_components
WHERE module_id = $1
ORDER BY component_type, component_id
`, moduleID)
	if err != nil {
		return nil, err
	}
	defer rows.Close()
	return scanComponents(rows)
}

func (r *Repository) ListPermissionsByModule(ctx context.Context, moduleID string) ([]Permission, error) {
	rows, err := r.db.Query(ctx, `
SELECT module_id, permission_key, description
FROM module_permissions
WHERE module_id = $1
ORDER BY permission_key
`, moduleID)
	if err != nil {
		return nil, err
	}
	defer rows.Close()
	items := make([]Permission, 0)
	for rows.Next() {
		var item Permission
		if err := rows.Scan(&item.ModuleID, &item.PermissionKey, &item.Description); err != nil {
			return nil, err
		}
		items = append(items, item)
	}
	return items, rows.Err()
}

func (r *Repository) ListMenusByModule(ctx context.Context, moduleID string) ([]Menu, error) {
	rows, err := r.db.Query(ctx, `
SELECT module_id, menu_key, title, route_path, icon, parent_key, sort_order, required_permission, enabled
FROM module_menus
WHERE module_id = $1
ORDER BY sort_order, menu_key
`, moduleID)
	if err != nil {
		return nil, err
	}
	defer rows.Close()
	return scanMenus(rows)
}

func (r *Repository) ListFrontendRoutesByModule(ctx context.Context, moduleID string) ([]FrontendRoute, error) {
	rows, err := r.db.Query(ctx, `
SELECT module_id, route_path, route_name, component_key, required_permission, enabled
FROM module_frontend_routes
WHERE module_id = $1
ORDER BY route_path
`, moduleID)
	if err != nil {
		return nil, err
	}
	defer rows.Close()
	return scanFrontendRoutes(rows)
}

func (r *Repository) ListGatewayRoutesByModule(ctx context.Context, moduleID string) ([]GatewayRoute, error) {
	rows, err := r.db.Query(ctx, `
SELECT module_id, prefix, target_service, auth_mode, enabled
FROM module_gateway_routes
WHERE module_id = $1
ORDER BY prefix
`, moduleID)
	if err != nil {
		return nil, err
	}
	defer rows.Close()
	return scanGatewayRoutes(rows)
}

func (r *Repository) ListInstallationsByModule(ctx context.Context, moduleID string) ([]Installation, error) {
	rows, err := r.db.Query(ctx, `
SELECT module_id, name, version, status, manifest, enabled_at, disabled_at
FROM module_installations
WHERE module_id = $1
ORDER BY module_id
`, moduleID)
	if err != nil {
		return nil, err
	}
	defer rows.Close()
	items := make([]Installation, 0)
	for rows.Next() {
		var item Installation
		var manifest []byte
		var enabledAt, disabledAt *time.Time
		if err := rows.Scan(&item.ModuleID, &item.Name, &item.Version, &item.Status, &manifest, &enabledAt, &disabledAt); err != nil {
			return nil, err
		}
		item.Manifest = rawOrObject(manifest)
		if enabledAt != nil {
			item.EnabledAt = enabledAt.UTC().Format(time.RFC3339Nano)
		}
		if disabledAt != nil {
			item.DisabledAt = disabledAt.UTC().Format(time.RFC3339Nano)
		}
		items = append(items, item)
	}
	return items, rows.Err()
}

const moduleSelectSQL = `
SELECT module_id, set_id, name, version, status, kind, description, manifest
FROM module_nodes
`

type moduleRow interface {
	Scan(dest ...any) error
}

func scanModule(row moduleRow) (Module, error) {
	var item Module
	var manifest []byte
	if err := row.Scan(&item.ModuleID, &item.SetID, &item.Name, &item.Version, &item.Status, &item.Kind, &item.Description, &manifest); err != nil {
		return Module{}, err
	}
	item.Manifest = rawOrObject(manifest)
	return item, nil
}

func scanModules(rows pgx.Rows) ([]Module, error) {
	items := make([]Module, 0)
	for rows.Next() {
		item, err := scanModule(rows)
		if err != nil {
			return nil, err
		}
		items = append(items, item)
	}
	return items, rows.Err()
}

func scanSets(rows pgx.Rows) ([]Set, error) {
	items := make([]Set, 0)
	for rows.Next() {
		var item Set
		if err := rows.Scan(&item.SetID, &item.Name, &item.Description, &item.SortOrder); err != nil {
			return nil, err
		}
		items = append(items, item)
	}
	return items, rows.Err()
}

func scanEdges(rows pgx.Rows) ([]Edge, error) {
	items := make([]Edge, 0)
	for rows.Next() {
		var item Edge
		if err := rows.Scan(&item.FromModuleID, &item.ToModuleID, &item.EdgeType, &item.VersionConstraint, &item.Required); err != nil {
			return nil, err
		}
		items = append(items, item)
	}
	return items, rows.Err()
}

func scanComponents(rows pgx.Rows) ([]Component, error) {
	items := make([]Component, 0)
	for rows.Next() {
		var item Component
		var config []byte
		if err := rows.Scan(&item.ModuleID, &item.ComponentID, &item.ComponentType, &item.Status, &config); err != nil {
			return nil, err
		}
		item.Config = rawOrObject(config)
		items = append(items, item)
	}
	return items, rows.Err()
}

func scanMenus(rows pgx.Rows) ([]Menu, error) {
	items := make([]Menu, 0)
	for rows.Next() {
		var item Menu
		if err := rows.Scan(&item.ModuleID, &item.MenuKey, &item.Title, &item.RoutePath, &item.Icon, &item.ParentKey, &item.SortOrder, &item.RequiredPermission, &item.Enabled); err != nil {
			return nil, err
		}
		items = append(items, item)
	}
	return items, rows.Err()
}

func scanFrontendRoutes(rows pgx.Rows) ([]FrontendRoute, error) {
	items := make([]FrontendRoute, 0)
	for rows.Next() {
		var item FrontendRoute
		if err := rows.Scan(&item.ModuleID, &item.RoutePath, &item.RouteName, &item.ComponentKey, &item.RequiredPermission, &item.Enabled); err != nil {
			return nil, err
		}
		items = append(items, item)
	}
	return items, rows.Err()
}

func scanGatewayRoutes(rows pgx.Rows) ([]GatewayRoute, error) {
	items := make([]GatewayRoute, 0)
	for rows.Next() {
		var item GatewayRoute
		if err := rows.Scan(&item.ModuleID, &item.Prefix, &item.TargetService, &item.AuthMode, &item.Enabled); err != nil {
			return nil, err
		}
		items = append(items, item)
	}
	return items, rows.Err()
}

func rawOrObject(data []byte) json.RawMessage {
	if len(data) == 0 {
		return json.RawMessage(`{}`)
	}
	return json.RawMessage(data)
}
