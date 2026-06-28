CREATE TABLE IF NOT EXISTS service_sets (
    id BIGSERIAL PRIMARY KEY,
    set_id TEXT NOT NULL UNIQUE,
    name TEXT NOT NULL,
    description TEXT NOT NULL DEFAULT '',
    manifest JSONB NOT NULL DEFAULT '{}'::jsonb,
    non_root_only BOOLEAN NOT NULL DEFAULT FALSE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE TABLE IF NOT EXISTS services (
    id BIGSERIAL PRIMARY KEY,
    service_id TEXT NOT NULL UNIQUE,
    name TEXT NOT NULL,
    version TEXT NOT NULL,
    kind TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'INSTALLED',
    manifest JSONB NOT NULL DEFAULT '{}'::jsonb,
    legacy_module_id TEXT NOT NULL DEFAULT '',
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE TABLE IF NOT EXISTS service_installations (
    id BIGSERIAL PRIMARY KEY,
    service_id TEXT NOT NULL,
    device_id TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'INSTALLED',
    runtime_mode TEXT NOT NULL,
    config JSONB NOT NULL DEFAULT '{}'::jsonb,
    installed_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    UNIQUE(service_id, device_id)
);

CREATE TABLE IF NOT EXISTS devices (
    id BIGSERIAL PRIMARY KEY,
    device_id TEXT NOT NULL UNIQUE,
    name TEXT NOT NULL,
    kind TEXT NOT NULL,
    endpoint TEXT NOT NULL DEFAULT '',
    join_secret_ref TEXT NOT NULL DEFAULT '',
    health TEXT NOT NULL DEFAULT 'unknown',
    last_seen_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE TABLE IF NOT EXISTS service_endpoints (
    id BIGSERIAL PRIMARY KEY,
    endpoint TEXT NOT NULL UNIQUE,
    service_id TEXT NOT NULL,
    device_id TEXT NOT NULL,
    protocol TEXT NOT NULL,
    health_path TEXT NOT NULL DEFAULT '',
    display_name TEXT NOT NULL DEFAULT '',
    note TEXT NOT NULL DEFAULT '',
    health TEXT NOT NULL DEFAULT 'unknown',
    reachable BOOLEAN NOT NULL DEFAULT FALSE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE TABLE IF NOT EXISTS service_links (
    id BIGSERIAL PRIMARY KEY,
    source_endpoint TEXT NOT NULL,
    target_endpoint TEXT NOT NULL,
    protocol TEXT NOT NULL,
    auth_mode TEXT NOT NULL DEFAULT 'internal',
    scope TEXT NOT NULL DEFAULT '',
    health TEXT NOT NULL DEFAULT 'unknown',
    latency_ms INT,
    config_ref TEXT NOT NULL DEFAULT '',
    secret_ref TEXT NOT NULL DEFAULT '',
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    UNIQUE(source_endpoint, target_endpoint)
);

CREATE TABLE IF NOT EXISTS runtime_operations (
    id BIGSERIAL PRIMARY KEY,
    operation_id TEXT NOT NULL UNIQUE,
    object_type TEXT NOT NULL,
    object_id TEXT NOT NULL,
    action TEXT NOT NULL,
    status TEXT NOT NULL,
    actor_user_id BIGINT,
    actor_username TEXT NOT NULL DEFAULT '',
    request JSONB NOT NULL DEFAULT '{}'::jsonb,
    plan JSONB NOT NULL DEFAULT '{}'::jsonb,
    result JSONB NOT NULL DEFAULT '{}'::jsonb,
    error_message TEXT NOT NULL DEFAULT '',
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_services_status ON services(status);
CREATE INDEX IF NOT EXISTS idx_service_installations_device ON service_installations(device_id);
CREATE INDEX IF NOT EXISTS idx_service_endpoints_service ON service_endpoints(service_id);
CREATE INDEX IF NOT EXISTS idx_service_links_source ON service_links(source_endpoint);
CREATE INDEX IF NOT EXISTS idx_service_links_target ON service_links(target_endpoint);
CREATE INDEX IF NOT EXISTS idx_runtime_operations_object ON runtime_operations(object_type, object_id, created_at DESC);

INSERT INTO devices(device_id, name, kind, endpoint, health)
VALUES ('root-local', 'Root Local Device', 'root', '127.0.0.1:0', 'unknown')
ON CONFLICT(device_id) DO UPDATE SET
    name = EXCLUDED.name,
    kind = EXCLUDED.kind,
    updated_at = NOW();

INSERT INTO permissions(code, module_code, name, description) VALUES
    ('service.install', 'runtime', 'Install Service', '安装 Service'),
    ('service.enable', 'runtime', 'Enable Service', '启用 Service'),
    ('service.disable', 'runtime', 'Disable Service', '禁用 Service'),
    ('service.delete', 'runtime', 'Delete Service', '删除 Service'),
    ('endpoint.configure', 'runtime', 'Configure Endpoint', '配置 Endpoint'),
    ('link.configure', 'runtime', 'Configure Link', '配置 Link'),
    ('topology.read', 'runtime', 'Read Topology', '查看 Topology')
ON CONFLICT (code) DO UPDATE SET
    module_code = EXCLUDED.module_code,
    name = EXCLUDED.name,
    description = EXCLUDED.description;
