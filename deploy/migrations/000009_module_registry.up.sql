CREATE TABLE IF NOT EXISTS module_sets (
    id BIGSERIAL PRIMARY KEY,
    set_id TEXT NOT NULL UNIQUE,
    name TEXT NOT NULL,
    description TEXT NOT NULL DEFAULT '',
    sort_order INT NOT NULL DEFAULT 0,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE TABLE IF NOT EXISTS module_nodes (
    id BIGSERIAL PRIMARY KEY,
    module_id TEXT NOT NULL UNIQUE,
    set_id TEXT NOT NULL,
    name TEXT NOT NULL,
    version TEXT NOT NULL,
    status TEXT NOT NULL,
    kind TEXT NOT NULL,
    description TEXT NOT NULL DEFAULT '',
    manifest JSONB NOT NULL DEFAULT '{}',
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE TABLE IF NOT EXISTS module_edges (
    id BIGSERIAL PRIMARY KEY,
    from_module_id TEXT NOT NULL,
    to_module_id TEXT NOT NULL,
    edge_type TEXT NOT NULL,
    version_constraint TEXT NOT NULL DEFAULT '',
    required BOOLEAN NOT NULL DEFAULT TRUE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    UNIQUE(from_module_id, to_module_id, edge_type)
);

CREATE TABLE IF NOT EXISTS module_components (
    id BIGSERIAL PRIMARY KEY,
    module_id TEXT NOT NULL,
    component_id TEXT NOT NULL,
    component_type TEXT NOT NULL,
    status TEXT NOT NULL,
    config JSONB NOT NULL DEFAULT '{}',
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    UNIQUE(module_id, component_id)
);

CREATE TABLE IF NOT EXISTS module_installations (
    id BIGSERIAL PRIMARY KEY,
    module_id TEXT NOT NULL UNIQUE,
    name TEXT NOT NULL,
    version TEXT NOT NULL,
    status TEXT NOT NULL,
    manifest JSONB NOT NULL DEFAULT '{}',
    installed_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    enabled_at TIMESTAMPTZ,
    disabled_at TIMESTAMPTZ
);

CREATE TABLE IF NOT EXISTS module_migrations (
    id BIGSERIAL PRIMARY KEY,
    module_id TEXT NOT NULL,
    version TEXT NOT NULL,
    migration_name TEXT NOT NULL,
    checksum TEXT NOT NULL,
    applied_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    UNIQUE(module_id, migration_name)
);

CREATE TABLE IF NOT EXISTS module_permissions (
    id BIGSERIAL PRIMARY KEY,
    module_id TEXT NOT NULL,
    permission_key TEXT NOT NULL UNIQUE,
    description TEXT NOT NULL DEFAULT '',
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE TABLE IF NOT EXISTS module_menus (
    id BIGSERIAL PRIMARY KEY,
    module_id TEXT NOT NULL,
    menu_key TEXT NOT NULL UNIQUE,
    title TEXT NOT NULL,
    route_path TEXT NOT NULL,
    icon TEXT NOT NULL DEFAULT '',
    parent_key TEXT NOT NULL DEFAULT '',
    sort_order INT NOT NULL DEFAULT 0,
    required_permission TEXT NOT NULL DEFAULT '',
    enabled BOOLEAN NOT NULL DEFAULT TRUE
);

CREATE TABLE IF NOT EXISTS module_frontend_routes (
    id BIGSERIAL PRIMARY KEY,
    module_id TEXT NOT NULL,
    route_path TEXT NOT NULL,
    route_name TEXT NOT NULL,
    component_key TEXT NOT NULL,
    required_permission TEXT NOT NULL DEFAULT '',
    enabled BOOLEAN NOT NULL DEFAULT TRUE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    UNIQUE(module_id, route_path)
);

CREATE TABLE IF NOT EXISTS module_gateway_routes (
    id BIGSERIAL PRIMARY KEY,
    module_id TEXT NOT NULL,
    prefix TEXT NOT NULL UNIQUE,
    target_service TEXT NOT NULL,
    auth_mode TEXT NOT NULL,
    enabled BOOLEAN NOT NULL DEFAULT TRUE
);

CREATE INDEX IF NOT EXISTS idx_module_nodes_set_id ON module_nodes(set_id);
CREATE INDEX IF NOT EXISTS idx_module_edges_from ON module_edges(from_module_id);
CREATE INDEX IF NOT EXISTS idx_module_edges_to ON module_edges(to_module_id);
CREATE INDEX IF NOT EXISTS idx_module_components_module_id ON module_components(module_id);
CREATE INDEX IF NOT EXISTS idx_module_permissions_module_id ON module_permissions(module_id);
CREATE INDEX IF NOT EXISTS idx_module_menus_module_id ON module_menus(module_id);
CREATE INDEX IF NOT EXISTS idx_module_frontend_routes_module_id ON module_frontend_routes(module_id);
CREATE INDEX IF NOT EXISTS idx_module_gateway_routes_module_id ON module_gateway_routes(module_id);
