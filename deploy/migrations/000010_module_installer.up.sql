CREATE TABLE IF NOT EXISTS module_operation_locks (
    id BIGSERIAL PRIMARY KEY,
    lock_key TEXT NOT NULL UNIQUE,
    owner TEXT NOT NULL,
    acquired_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    expires_at TIMESTAMPTZ NOT NULL
);

CREATE TABLE IF NOT EXISTS module_operations (
    id BIGSERIAL PRIMARY KEY,
    operation_id TEXT NOT NULL UNIQUE,
    module_id TEXT NOT NULL,
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

CREATE INDEX IF NOT EXISTS idx_module_operation_locks_expires_at
    ON module_operation_locks(expires_at);

CREATE INDEX IF NOT EXISTS idx_module_operations_module_id
    ON module_operations(module_id, created_at DESC);

CREATE INDEX IF NOT EXISTS idx_module_operations_actor
    ON module_operations(actor_user_id, created_at DESC);

INSERT INTO permissions(code, module_code, name, description) VALUES
    ('module.rollback', 'core', 'Rollback Module', 'Plan or apply module rollback operations'),
    ('module.uninstall', 'core', 'Uninstall Module', 'Plan or apply module uninstall operations')
ON CONFLICT (code) DO UPDATE SET
    module_code = EXCLUDED.module_code,
    name = EXCLUDED.name,
    description = EXCLUDED.description;

INSERT INTO role_permissions(role_id, permission_code)
SELECT r.id, p.code
FROM roles r
JOIN permissions p ON p.code IN ('module.rollback', 'module.uninstall')
WHERE r.name IN ('super_admin', 'admin', 'module_manager')
ON CONFLICT DO NOTHING;
