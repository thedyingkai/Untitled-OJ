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

CREATE INDEX IF NOT EXISTS idx_service_endpoints_service ON service_endpoints(service_id);
CREATE INDEX IF NOT EXISTS idx_service_endpoints_device ON service_endpoints(device_id);
CREATE INDEX IF NOT EXISTS idx_service_links_source ON service_links(source_endpoint);
CREATE INDEX IF NOT EXISTS idx_service_links_target ON service_links(target_endpoint);

INSERT INTO devices(device_id, name, kind, endpoint, health)
VALUES ('root-local', 'Root Local Device', 'root', '127.0.0.1:0', 'unknown')
ON CONFLICT(device_id) DO UPDATE SET
    name = EXCLUDED.name,
    kind = EXCLUDED.kind,
    updated_at = NOW();

INSERT INTO permissions(code, service_code, name, description) VALUES
    ('service.install', 'runtime', 'Install Service', '安装 Service'),
    ('service.enable', 'runtime', 'Enable Service', '启用 Service'),
    ('service.disable', 'runtime', 'Disable Service', '禁用 Service'),
    ('service.delete', 'runtime', 'Delete Service', '删除 Service'),
    ('endpoint.configure', 'runtime', 'Configure Endpoint', '配置 Endpoint'),
    ('link.configure', 'runtime', 'Configure Link', '配置 Link'),
    ('topology.read', 'runtime', 'Read Topology', '查看 Topology')
ON CONFLICT (code) DO UPDATE SET
    service_code = EXCLUDED.service_code,
    name = EXCLUDED.name,
    description = EXCLUDED.description;
