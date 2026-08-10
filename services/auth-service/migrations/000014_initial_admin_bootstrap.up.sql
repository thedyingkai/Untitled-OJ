CREATE TABLE IF NOT EXISTS auth_bootstrap_state (
    bootstrap_key TEXT PRIMARY KEY,
    completed_at TIMESTAMPTZ,
    user_id BIGINT REFERENCES users(id) ON DELETE RESTRICT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    CONSTRAINT chk_auth_bootstrap_completion
        CHECK (
            (completed_at IS NULL AND user_id IS NULL)
            OR (completed_at IS NOT NULL AND user_id IS NOT NULL)
        )
);

INSERT INTO auth_bootstrap_state(bootstrap_key)
VALUES('initial-super-admin')
ON CONFLICT (bootstrap_key) DO NOTHING;

-- An upgraded installation may already have a super administrator. Permanently
-- consume bootstrap during migration so deleting that administrator later does
-- not reopen the initial-identity path.
WITH existing_admin AS (
    SELECT u.id AS user_id
    FROM users u
    JOIN user_roles ur ON ur.user_id = u.id
    JOIN roles r ON r.id = ur.role_id
    WHERE r.name = 'super_admin'

    UNION

    SELECT u.id AS user_id
    FROM users u
    JOIN role_bindings rb
      ON rb.principal_type = 'user'
     AND rb.principal_id = u.id
    JOIN roles r ON r.id = rb.role_id
    WHERE r.name = 'super_admin'
      AND rb.scope_type = 'system'
      AND rb.scope_id = 0
      AND (rb.expires_at IS NULL OR rb.expires_at > NOW())

    ORDER BY user_id
    LIMIT 1
)
UPDATE auth_bootstrap_state state
SET completed_at = NOW(), user_id = existing_admin.user_id
FROM existing_admin
WHERE state.bootstrap_key = 'initial-super-admin'
  AND state.completed_at IS NULL;
