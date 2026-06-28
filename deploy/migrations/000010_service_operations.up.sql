CREATE TABLE IF NOT EXISTS service_operation_locks (
    id BIGSERIAL PRIMARY KEY,
    lock_key TEXT NOT NULL UNIQUE,
    owner TEXT NOT NULL,
    acquired_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    expires_at TIMESTAMPTZ NOT NULL
);

CREATE TABLE IF NOT EXISTS service_runtime_operations (
    id BIGSERIAL PRIMARY KEY,
    operation_id TEXT NOT NULL UNIQUE,
    object_type TEXT NOT NULL DEFAULT 'service',
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

CREATE INDEX IF NOT EXISTS idx_service_operation_locks_expires_at
    ON service_operation_locks(expires_at);

CREATE INDEX IF NOT EXISTS idx_service_runtime_operations_object
    ON service_runtime_operations(object_type, object_id, created_at DESC);

CREATE INDEX IF NOT EXISTS idx_service_runtime_operations_actor
    ON service_runtime_operations(actor_user_id, created_at DESC);

INSERT INTO permissions(code, service_code, name, description) VALUES
    ('service.rollback', 'runtime', 'Rollback Service', 'Plan service rollback operations'),
    ('service.uninstall', 'runtime', 'Uninstall Service', 'Plan service uninstall operations')
ON CONFLICT (code) DO UPDATE SET
    service_code = EXCLUDED.service_code,
    name = EXCLUDED.name,
    description = EXCLUDED.description;

INSERT INTO role_permissions(role_id, permission_code)
SELECT r.id, p.code
FROM roles r
JOIN permissions p ON p.code IN ('service.rollback', 'service.uninstall')
WHERE r.name IN ('super_admin', 'admin', 'service_manager')
ON CONFLICT DO NOTHING;
