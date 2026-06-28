CREATE TABLE IF NOT EXISTS service_sets (
    id BIGSERIAL PRIMARY KEY,
    set_id TEXT NOT NULL UNIQUE,
    name TEXT NOT NULL,
    description TEXT NOT NULL DEFAULT '',
    sort_order INT NOT NULL DEFAULT 0,
    manifest JSONB NOT NULL DEFAULT '{}'::jsonb,
    non_root_only BOOLEAN NOT NULL DEFAULT FALSE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE TABLE IF NOT EXISTS service_nodes (
    id BIGSERIAL PRIMARY KEY,
    service_id TEXT NOT NULL UNIQUE,
    set_id TEXT NOT NULL,
    name TEXT NOT NULL,
    version TEXT NOT NULL,
    status TEXT NOT NULL,
    kind TEXT NOT NULL,
    description TEXT NOT NULL DEFAULT '',
    manifest JSONB NOT NULL DEFAULT '{}'::jsonb,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE TABLE IF NOT EXISTS service_edges (
    id BIGSERIAL PRIMARY KEY,
    from_service_id TEXT NOT NULL,
    to_service_id TEXT NOT NULL,
    edge_type TEXT NOT NULL,
    version_constraint TEXT NOT NULL DEFAULT '',
    required BOOLEAN NOT NULL DEFAULT TRUE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    UNIQUE(from_service_id, to_service_id, edge_type)
);

CREATE TABLE IF NOT EXISTS service_components (
    id BIGSERIAL PRIMARY KEY,
    service_id TEXT NOT NULL,
    component_id TEXT NOT NULL,
    component_type TEXT NOT NULL,
    status TEXT NOT NULL,
    config JSONB NOT NULL DEFAULT '{}'::jsonb,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    UNIQUE(service_id, component_id)
);

CREATE TABLE IF NOT EXISTS service_installations (
    id BIGSERIAL PRIMARY KEY,
    service_id TEXT NOT NULL UNIQUE,
    name TEXT NOT NULL,
    version TEXT NOT NULL,
    status TEXT NOT NULL,
    manifest JSONB NOT NULL DEFAULT '{}'::jsonb,
    installed_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    enabled_at TIMESTAMPTZ,
    disabled_at TIMESTAMPTZ
);

CREATE TABLE IF NOT EXISTS service_migrations (
    id BIGSERIAL PRIMARY KEY,
    service_id TEXT NOT NULL,
    version TEXT NOT NULL,
    migration_name TEXT NOT NULL,
    checksum TEXT NOT NULL,
    applied_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    UNIQUE(service_id, migration_name)
);

CREATE TABLE IF NOT EXISTS service_permissions (
    id BIGSERIAL PRIMARY KEY,
    service_id TEXT NOT NULL,
    permission_key TEXT NOT NULL UNIQUE,
    description TEXT NOT NULL DEFAULT '',
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE TABLE IF NOT EXISTS service_menus (
    id BIGSERIAL PRIMARY KEY,
    service_id TEXT NOT NULL,
    menu_key TEXT NOT NULL UNIQUE,
    title TEXT NOT NULL,
    route_path TEXT NOT NULL,
    icon TEXT NOT NULL DEFAULT '',
    parent_key TEXT NOT NULL DEFAULT '',
    sort_order INT NOT NULL DEFAULT 0,
    required_permission TEXT NOT NULL DEFAULT '',
    enabled BOOLEAN NOT NULL DEFAULT TRUE
);

CREATE TABLE IF NOT EXISTS service_frontend_routes (
    id BIGSERIAL PRIMARY KEY,
    service_id TEXT NOT NULL,
    route_path TEXT NOT NULL,
    route_name TEXT NOT NULL,
    component_key TEXT NOT NULL,
    required_permission TEXT NOT NULL DEFAULT '',
    enabled BOOLEAN NOT NULL DEFAULT TRUE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    UNIQUE(service_id, route_path)
);

CREATE TABLE IF NOT EXISTS service_gateway_routes (
    id BIGSERIAL PRIMARY KEY,
    service_id TEXT NOT NULL,
    prefix TEXT NOT NULL UNIQUE,
    target_service TEXT NOT NULL,
    auth_mode TEXT NOT NULL,
    enabled BOOLEAN NOT NULL DEFAULT TRUE
);

CREATE INDEX IF NOT EXISTS idx_service_nodes_set_id ON service_nodes(set_id);
CREATE INDEX IF NOT EXISTS idx_service_edges_from ON service_edges(from_service_id);
CREATE INDEX IF NOT EXISTS idx_service_edges_to ON service_edges(to_service_id);
CREATE INDEX IF NOT EXISTS idx_service_components_service_id ON service_components(service_id);
CREATE INDEX IF NOT EXISTS idx_service_permissions_service_id ON service_permissions(service_id);
CREATE INDEX IF NOT EXISTS idx_service_menus_service_id ON service_menus(service_id);
CREATE INDEX IF NOT EXISTS idx_service_frontend_routes_service_id ON service_frontend_routes(service_id);
CREATE INDEX IF NOT EXISTS idx_service_gateway_routes_service_id ON service_gateway_routes(service_id);
