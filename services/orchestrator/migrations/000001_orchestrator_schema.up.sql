CREATE TABLE IF NOT EXISTS service_releases (
    service_name TEXT NOT NULL,
    version TEXT NOT NULL,
    release_url TEXT NOT NULL DEFAULT '',
    manifest JSONB NOT NULL DEFAULT '{}'::jsonb,
    checksum TEXT NOT NULL DEFAULT '',
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    PRIMARY KEY(service_name, version)
);

CREATE TABLE IF NOT EXISTS host_services (
    host_ip TEXT NOT NULL,
    service_name TEXT NOT NULL,
    version TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'unknown',
    config JSONB NOT NULL DEFAULT '{}'::jsonb,
    labels JSONB NOT NULL DEFAULT '{}'::jsonb,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    PRIMARY KEY(host_ip, service_name)
);

CREATE TABLE IF NOT EXISTS service_endpoints (
    endpoint TEXT NOT NULL,
    service_id TEXT NOT NULL,
    ip TEXT NOT NULL,
    port INT NOT NULL,
    service_name TEXT NOT NULL,
    host_ip TEXT NOT NULL,
    port_name TEXT NOT NULL DEFAULT 'default',
    protocol TEXT NOT NULL,
    visibility TEXT NOT NULL DEFAULT 'cluster',
    status TEXT NOT NULL DEFAULT 'unknown',
    health_path TEXT NOT NULL DEFAULT '',
    reachable BOOLEAN NOT NULL DEFAULT FALSE,
    display_name TEXT NOT NULL DEFAULT '',
    note TEXT NOT NULL DEFAULT '',
    config JSONB NOT NULL DEFAULT '{}'::jsonb,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    PRIMARY KEY(ip, port, service_name),
    UNIQUE(endpoint),
    UNIQUE(host_ip, service_name),
    UNIQUE(host_ip, service_name, port_name),
    CONSTRAINT service_endpoints_port_positive CHECK (port > 0),
    CONSTRAINT service_endpoints_identity_shape CHECK (endpoint = ip || ':' || port::TEXT || ':' || service_name),
    CONSTRAINT service_endpoints_service_id_matches_identity CHECK (service_id = service_name)
);

CREATE TABLE IF NOT EXISTS service_links (
    source_endpoint TEXT NOT NULL,
    target_endpoint TEXT NOT NULL,
    from_ip TEXT NOT NULL,
    from_port INT NOT NULL,
    from_service_name TEXT NOT NULL,
    to_type TEXT NOT NULL,
    to_ip TEXT,
    to_port INT,
    to_service_name TEXT NOT NULL,
    to_selector JSONB NOT NULL DEFAULT '{}'::jsonb,
    protocol TEXT NOT NULL,
    enabled BOOLEAN NOT NULL DEFAULT TRUE,
    auth_mode TEXT NOT NULL DEFAULT 'internal',
    scope TEXT NOT NULL DEFAULT '',
    health TEXT NOT NULL DEFAULT 'unknown',
    latency_ms INT,
    config JSONB NOT NULL DEFAULT '{}'::jsonb,
    config_ref TEXT NOT NULL DEFAULT '',
    secret_ref TEXT NOT NULL DEFAULT '',
    policy JSONB NOT NULL DEFAULT '{}'::jsonb,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    PRIMARY KEY(source_endpoint, target_endpoint),
    CONSTRAINT service_links_to_type CHECK (to_type IN ('endpoint', 'endpoint-group'))
);

CREATE TABLE IF NOT EXISTS service_routes (
    path TEXT NOT NULL,
    method TEXT NOT NULL DEFAULT '*',
    target_type TEXT NOT NULL,
    target_service_name TEXT NOT NULL,
    target_selector JSONB NOT NULL DEFAULT '{}'::jsonb,
    target_ip TEXT,
    target_port INT,
    permission TEXT NOT NULL DEFAULT '',
    enabled BOOLEAN NOT NULL DEFAULT TRUE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    PRIMARY KEY(path, method),
    CONSTRAINT service_routes_target_type CHECK (target_type IN ('endpoint', 'endpoint-group', 'frontend'))
);

CREATE TABLE IF NOT EXISTS service_migration_records (
    service_name TEXT NOT NULL,
    migration_version TEXT NOT NULL,
    checksum TEXT NOT NULL DEFAULT '',
    status TEXT NOT NULL,
    applied_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    PRIMARY KEY(service_name, migration_version)
);

CREATE TABLE IF NOT EXISTS service_permission_records (
    service_name TEXT NOT NULL,
    permission_key TEXT NOT NULL,
    source TEXT NOT NULL DEFAULT 'release',
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    PRIMARY KEY(service_name, permission_key)
);

CREATE TABLE IF NOT EXISTS service_frontend_entries (
    service_name TEXT PRIMARY KEY,
    enabled BOOLEAN NOT NULL DEFAULT FALSE,
    route_prefix TEXT NOT NULL DEFAULT '',
    remote_entry TEXT NOT NULL DEFAULT '',
    menu_items JSONB NOT NULL DEFAULT '[]'::jsonb,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE TABLE IF NOT EXISTS service_redis_resources (
    service_name TEXT NOT NULL,
    name TEXT NOT NULL,
    kind TEXT NOT NULL,
    usage TEXT NOT NULL DEFAULT '',
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    PRIMARY KEY(service_name, name)
);

CREATE TABLE IF NOT EXISTS service_storage_resources (
    service_name TEXT NOT NULL,
    object_type TEXT NOT NULL,
    bucket TEXT NOT NULL,
    path_prefix TEXT NOT NULL DEFAULT '',
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    PRIMARY KEY(service_name, object_type, bucket)
);

CREATE TABLE IF NOT EXISTS rendered_service_configs (
    service_name TEXT NOT NULL,
    version TEXT NOT NULL,
    config JSONB NOT NULL DEFAULT '{}'::jsonb,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    PRIMARY KEY(service_name, version)
);

CREATE TABLE IF NOT EXISTS nodes (
    node_id TEXT PRIMARY KEY,
    host_ip TEXT NOT NULL UNIQUE,
    parent_node_id TEXT NOT NULL DEFAULT '',
    role TEXT NOT NULL,
    labels JSONB NOT NULL DEFAULT '{}'::jsonb,
    status TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    CONSTRAINT nodes_role CHECK (role IN ('root', 'node', 'standalone')),
    CONSTRAINT nodes_root_parent CHECK (
        (role = 'root' AND parent_node_id = '')
        OR role <> 'root'
    ),
    CONSTRAINT nodes_standalone_parent CHECK (
        (role = 'standalone' AND parent_node_id = '')
        OR role <> 'standalone'
    ),
    CONSTRAINT nodes_parent_not_self CHECK (parent_node_id = '' OR parent_node_id <> node_id)
);

CREATE TABLE IF NOT EXISTS service_api_surfaces (
    service_name TEXT NOT NULL,
    version TEXT NOT NULL,
    api_id TEXT NOT NULL,
    protocol TEXT NOT NULL,
    port_name TEXT NOT NULL,
    path_prefix TEXT NOT NULL DEFAULT '',
    methods JSONB NOT NULL DEFAULT '[]'::jsonb,
    visibility TEXT NOT NULL,
    auth_mode TEXT NOT NULL,
    permission TEXT NOT NULL DEFAULT 'public',
    stability TEXT NOT NULL,
    api_version TEXT NOT NULL,
    rate_limit TEXT NOT NULL DEFAULT '',
    timeout TEXT NOT NULL DEFAULT '',
    config JSONB NOT NULL DEFAULT '{}'::jsonb,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    PRIMARY KEY(service_name, version, api_id),
    CONSTRAINT service_api_surfaces_visibility CHECK (visibility IN ('private', 'same-node', 'children', 'descendants', 'ancestors', 'global', 'explicit')),
    CONSTRAINT service_api_surfaces_auth CHECK (auth_mode IN ('public', 'user', 'service', 'internal')),
    CONSTRAINT service_api_surfaces_stability CHECK (stability IN ('stable', 'experimental', 'deprecated'))
);

CREATE TABLE IF NOT EXISTS deployed_service_apis (
    host_ip TEXT NOT NULL,
    service_name TEXT NOT NULL,
    version TEXT NOT NULL,
    endpoint TEXT NOT NULL,
    api_id TEXT NOT NULL,
    status TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    PRIMARY KEY(host_ip, service_name, api_id, endpoint)
);


CREATE TABLE IF NOT EXISTS services (
    service_id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    version TEXT NOT NULL,
    kind TEXT NOT NULL,
    description TEXT NOT NULL DEFAULT '',
    manifest JSONB NOT NULL DEFAULT '{}'::jsonb,
    health TEXT NOT NULL DEFAULT 'unknown',
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE TABLE IF NOT EXISTS orchestrator_operations (
    operation_id TEXT PRIMARY KEY,
    action TEXT NOT NULL,
    target_type TEXT NOT NULL,
    target_id TEXT NOT NULL,
    status TEXT NOT NULL,
    actor_user_id BIGINT,
    actor_username TEXT NOT NULL DEFAULT '',
    request JSONB NOT NULL DEFAULT '{}'::jsonb,
    plan JSONB NOT NULL DEFAULT '{}'::jsonb,
    rollback_plan JSONB NOT NULL DEFAULT '{}'::jsonb,
    result JSONB NOT NULL DEFAULT '{}'::jsonb,
    error_message TEXT NOT NULL DEFAULT '',
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    confirmed_at TIMESTAMPTZ,
    started_at TIMESTAMPTZ,
    finished_at TIMESTAMPTZ,
    rolled_back_at TIMESTAMPTZ
);

CREATE TABLE IF NOT EXISTS orchestrator_operation_logs (
    log_id BIGSERIAL PRIMARY KEY,
    operation_id TEXT NOT NULL,
    step_id TEXT NOT NULL DEFAULT '',
    level TEXT NOT NULL DEFAULT 'info',
    message TEXT NOT NULL,
    data JSONB NOT NULL DEFAULT '{}'::jsonb,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE TABLE IF NOT EXISTS orchestrator_operation_locks (
    lock_key TEXT PRIMARY KEY,
    operation_id TEXT NOT NULL,
    owner TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    expires_at TIMESTAMPTZ NOT NULL
);

CREATE TABLE IF NOT EXISTS topology_snapshots (
    snapshot_id TEXT PRIMARY KEY,
    topology JSONB NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE TABLE IF NOT EXISTS log_sources (
    source_id TEXT PRIMARY KEY,
    endpoint TEXT NOT NULL,
    service_id TEXT NOT NULL,
    operation_id TEXT NOT NULL DEFAULT '',
    kind TEXT NOT NULL,
    path TEXT NOT NULL,
    driver TEXT NOT NULL,
    read_policy TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE TABLE IF NOT EXISTS diagnostic_reports (
    report_id TEXT PRIMARY KEY,
    operation_id TEXT NOT NULL DEFAULT '',
    target_type TEXT NOT NULL,
    target_id TEXT NOT NULL,
    status TEXT NOT NULL,
    summary TEXT NOT NULL DEFAULT '',
    data JSONB NOT NULL DEFAULT '{}'::jsonb,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_service_endpoints_service ON service_endpoints(service_name);
CREATE INDEX IF NOT EXISTS idx_service_endpoints_service_id ON service_endpoints(service_id);
CREATE INDEX IF NOT EXISTS idx_service_endpoints_host ON service_endpoints(host_ip);
CREATE INDEX IF NOT EXISTS idx_service_links_source ON service_links(from_ip, from_port, from_service_name);
CREATE INDEX IF NOT EXISTS idx_service_links_target ON service_links(to_type, to_service_name);
CREATE INDEX IF NOT EXISTS idx_service_routes_target ON service_routes(target_type, target_service_name);
CREATE INDEX IF NOT EXISTS idx_service_migration_records_status ON service_migration_records(status);
CREATE INDEX IF NOT EXISTS idx_service_permission_records_permission ON service_permission_records(permission_key);
CREATE INDEX IF NOT EXISTS idx_service_redis_resources_kind ON service_redis_resources(kind);
CREATE INDEX IF NOT EXISTS idx_service_storage_resources_bucket ON service_storage_resources(bucket);
CREATE INDEX IF NOT EXISTS idx_nodes_parent ON nodes(parent_node_id);
CREATE INDEX IF NOT EXISTS idx_service_api_surfaces_api ON service_api_surfaces(api_id);
CREATE INDEX IF NOT EXISTS idx_deployed_service_apis_host ON deployed_service_apis(host_ip);
CREATE INDEX IF NOT EXISTS idx_deployed_service_apis_status ON deployed_service_apis(status);
CREATE INDEX IF NOT EXISTS idx_orchestrator_operations_target
    ON orchestrator_operations(target_type, target_id, created_at DESC);
CREATE INDEX IF NOT EXISTS idx_orchestrator_operation_locks_expires
    ON orchestrator_operation_locks(expires_at);
CREATE INDEX IF NOT EXISTS idx_log_sources_endpoint ON log_sources(endpoint);
CREATE INDEX IF NOT EXISTS idx_log_sources_operation ON log_sources(operation_id);
CREATE INDEX IF NOT EXISTS idx_diagnostic_reports_target
    ON diagnostic_reports(target_type, target_id, created_at DESC);
