CREATE TABLE IF NOT EXISTS service_sets (
    set_id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    version TEXT NOT NULL DEFAULT '',
    description TEXT NOT NULL DEFAULT '',
    sort_order INT NOT NULL DEFAULT 100,
    manifest JSONB NOT NULL DEFAULT '{}'::jsonb,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
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

CREATE TABLE IF NOT EXISTS service_endpoints (
    endpoint TEXT PRIMARY KEY,
    service_id TEXT NOT NULL,
    protocol TEXT NOT NULL,
    health_path TEXT NOT NULL DEFAULT '',
    health TEXT NOT NULL DEFAULT 'unknown',
    reachable BOOLEAN NOT NULL DEFAULT FALSE,
    display_name TEXT NOT NULL DEFAULT '',
    note TEXT NOT NULL DEFAULT '',
    config JSONB NOT NULL DEFAULT '{}'::jsonb,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE TABLE IF NOT EXISTS service_links (
    source_endpoint TEXT NOT NULL,
    target_endpoint TEXT NOT NULL,
    protocol TEXT NOT NULL,
    auth_mode TEXT NOT NULL DEFAULT 'internal',
    scope TEXT NOT NULL DEFAULT '',
    health TEXT NOT NULL DEFAULT 'unknown',
    latency_ms INT,
    config_ref TEXT NOT NULL DEFAULT '',
    secret_ref TEXT NOT NULL DEFAULT '',
    policy JSONB NOT NULL DEFAULT '{}'::jsonb,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    PRIMARY KEY(source_endpoint, target_endpoint)
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

CREATE INDEX IF NOT EXISTS idx_service_endpoints_service ON service_endpoints(service_id);
CREATE INDEX IF NOT EXISTS idx_service_links_source ON service_links(source_endpoint);
CREATE INDEX IF NOT EXISTS idx_service_links_target ON service_links(target_endpoint);
CREATE INDEX IF NOT EXISTS idx_orchestrator_operations_target
    ON orchestrator_operations(target_type, target_id, created_at DESC);
CREATE INDEX IF NOT EXISTS idx_orchestrator_operation_locks_expires
    ON orchestrator_operation_locks(expires_at);
CREATE INDEX IF NOT EXISTS idx_log_sources_endpoint ON log_sources(endpoint);
CREATE INDEX IF NOT EXISTS idx_log_sources_operation ON log_sources(operation_id);
CREATE INDEX IF NOT EXISTS idx_diagnostic_reports_target
    ON diagnostic_reports(target_type, target_id, created_at DESC);
