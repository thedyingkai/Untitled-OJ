ALTER TABLE roles
    ADD COLUMN IF NOT EXISTS service_code TEXT NOT NULL DEFAULT 'core',
    ADD COLUMN IF NOT EXISTS description TEXT NOT NULL DEFAULT '',
    ADD COLUMN IF NOT EXISTS is_system BOOLEAN NOT NULL DEFAULT false,
    ADD COLUMN IF NOT EXISTS created_at TIMESTAMPTZ NOT NULL DEFAULT NOW();

CREATE UNIQUE INDEX IF NOT EXISTS idx_roles_name_unique
    ON roles(name);

CREATE TABLE IF NOT EXISTS resource_types (
    code TEXT PRIMARY KEY,
    service_code TEXT NOT NULL DEFAULT 'core',
    name TEXT NOT NULL,
    description TEXT NOT NULL DEFAULT '',
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE TABLE IF NOT EXISTS permissions (
    code TEXT PRIMARY KEY,
    service_code TEXT NOT NULL DEFAULT 'core',
    name TEXT NOT NULL,
    description TEXT NOT NULL DEFAULT '',
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE TABLE IF NOT EXISTS role_permissions (
    role_id BIGINT NOT NULL REFERENCES roles(id) ON DELETE CASCADE,
    permission_code TEXT NOT NULL REFERENCES permissions(code) ON DELETE CASCADE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    PRIMARY KEY(role_id, permission_code)
);

CREATE TABLE IF NOT EXISTS role_bindings (
    id BIGSERIAL PRIMARY KEY,
    principal_type TEXT NOT NULL,
    principal_id BIGINT NOT NULL,
    role_id BIGINT NOT NULL REFERENCES roles(id) ON DELETE CASCADE,
    scope_type TEXT NOT NULL,
    scope_id BIGINT NOT NULL DEFAULT 0,
    granted_by_type TEXT NOT NULL DEFAULT 'system',
    granted_by_id BIGINT NOT NULL DEFAULT 0,
    expires_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    UNIQUE(principal_type, principal_id, role_id, scope_type, scope_id)
);

CREATE TABLE IF NOT EXISTS permission_assignments (
    id BIGSERIAL PRIMARY KEY,
    principal_type TEXT NOT NULL,
    principal_id BIGINT NOT NULL,
    permission_code TEXT NOT NULL REFERENCES permissions(code) ON DELETE CASCADE,
    scope_type TEXT NOT NULL,
    scope_id BIGINT NOT NULL DEFAULT 0,
    effect TEXT NOT NULL,
    granted_by_type TEXT NOT NULL DEFAULT 'system',
    granted_by_id BIGINT NOT NULL DEFAULT 0,
    reason TEXT NOT NULL DEFAULT '',
    expires_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    CONSTRAINT chk_permission_assignments_effect CHECK (effect IN ('allow', 'deny')),
    UNIQUE(principal_type, principal_id, permission_code, scope_type, scope_id)
);

CREATE TABLE IF NOT EXISTS resource_edges (
    id BIGSERIAL PRIMARY KEY,
    parent_type TEXT NOT NULL,
    parent_id BIGINT NOT NULL,
    child_type TEXT NOT NULL,
    child_id BIGINT NOT NULL,
    relation TEXT NOT NULL DEFAULT 'contains',
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    UNIQUE(parent_type, parent_id, child_type, child_id, relation)
);

CREATE TABLE IF NOT EXISTS permission_audit_logs (
    id BIGSERIAL PRIMARY KEY,
    actor_type TEXT NOT NULL DEFAULT 'system',
    actor_id BIGINT NOT NULL DEFAULT 0,
    action TEXT NOT NULL,
    target_type TEXT NOT NULL DEFAULT '',
    target_id BIGINT NOT NULL DEFAULT 0,
    permission_code TEXT NOT NULL DEFAULT '',
    role_id BIGINT,
    role_name TEXT NOT NULL DEFAULT '',
    scope_type TEXT NOT NULL DEFAULT '',
    scope_id BIGINT NOT NULL DEFAULT 0,
    effect TEXT NOT NULL DEFAULT '',
    metadata JSONB NOT NULL DEFAULT '{}'::jsonb,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_role_permissions_permission
    ON role_permissions(permission_code);
CREATE INDEX IF NOT EXISTS idx_role_bindings_principal
    ON role_bindings(principal_type, principal_id);
CREATE INDEX IF NOT EXISTS idx_role_bindings_scope
    ON role_bindings(scope_type, scope_id);
CREATE INDEX IF NOT EXISTS idx_role_bindings_role
    ON role_bindings(role_id);
CREATE INDEX IF NOT EXISTS idx_permission_assignments_principal
    ON permission_assignments(principal_type, principal_id);
CREATE INDEX IF NOT EXISTS idx_permission_assignments_permission
    ON permission_assignments(permission_code);
CREATE INDEX IF NOT EXISTS idx_permission_assignments_scope
    ON permission_assignments(scope_type, scope_id);
CREATE INDEX IF NOT EXISTS idx_resource_edges_child
    ON resource_edges(child_type, child_id);
CREATE INDEX IF NOT EXISTS idx_resource_edges_parent
    ON resource_edges(parent_type, parent_id);
CREATE INDEX IF NOT EXISTS idx_permission_audit_logs_actor
    ON permission_audit_logs(actor_type, actor_id);
CREATE INDEX IF NOT EXISTS idx_permission_audit_logs_scope
    ON permission_audit_logs(scope_type, scope_id);

INSERT INTO resource_types(code, service_code, name, description)
VALUES
    ('system', 'core', 'System', 'Global system scope'),
    ('problem', 'core', 'Problem', 'Problem resource'),
    ('group', 'core', 'Group', 'Organization or group resource'),
    ('submission', 'core', 'Submission', 'Submission resource'),
    ('post', 'core', 'Forum post resource')
ON CONFLICT (code) DO UPDATE SET
    service_code = EXCLUDED.service_code,
    name = EXCLUDED.name,
    description = EXCLUDED.description;

INSERT INTO permissions(code, service_code, name, description)
VALUES
    ('system.admin', 'core', 'System Admin', 'Full system administration permission'),
    ('problem.create', 'problem-service', 'Create Problem', 'Create new problems'),
    ('problem.view', 'problem-service', 'View Problem', 'View problems'),
    ('problem.view.private', 'problem-service', 'View Private Problem', 'View private problems'),
    ('problem.edit', 'problem-service', 'Edit Problem', 'Edit problem metadata and statement'),
    ('problem.delete', 'problem-service', 'Delete Problem', 'Delete problems'),
    ('problem.manage.data', 'problem-service', 'Manage Problem Data', 'Manage test cases'),
    ('problem.manage.asset', 'problem-service', 'Manage Problem Assets', 'Manage checker, scorer, interactor and other assets'),
    ('judge.submit', 'judge-api', 'Submit Code', 'Submit code to judge'),
    ('submission.view.own', 'judge-api', 'View Own Submission', 'View own submissions'),
    ('submission.view.all', 'judge-api', 'View All Submissions', 'View all submissions'),
    ('submission.rejudge', 'judge-api', 'Rejudge Submission', 'Rejudge submissions'),
    ('submission.delete', 'judge-api', 'Delete Submission', 'Delete submissions'),
    ('forum.post', 'core', 'Post Forum', 'Create forum posts'),
    ('forum.moderate', 'core', 'Moderate Forum', 'Moderate forum posts')
ON CONFLICT (code) DO UPDATE SET
    service_code = EXCLUDED.service_code,
    name = EXCLUDED.name,
    description = EXCLUDED.description;

INSERT INTO roles(name, service_code, description, is_system)
VALUES
    ('super_admin', 'core', 'Full system super administrator', true),
    ('admin', 'core', 'System administrator', true),
    ('user', 'core', 'Normal user', true),
    ('problem_owner', 'problem-service', 'Problem owner', true),
    ('problem_setter', 'problem-service', 'Problem setter', true),
    ('problem_viewer', 'problem-service', 'Problem viewer', true),
    ('problem_data_manager', 'problem-service', 'Problem data manager', true),
    ('forum_moderator', 'core', 'Forum moderator', true)
ON CONFLICT (name) DO UPDATE SET
    service_code = EXCLUDED.service_code,
    description = EXCLUDED.description,
    is_system = EXCLUDED.is_system;

INSERT INTO role_permissions(role_id, permission_code)
SELECT r.id, p.code
FROM roles r
CROSS JOIN permissions p
WHERE r.name = 'super_admin'
ON CONFLICT DO NOTHING;

INSERT INTO role_permissions(role_id, permission_code)
SELECT r.id, p.code
FROM roles r
JOIN permissions p ON p.code IN (
    'system.admin',
    'problem.create',
    'problem.view',
    'problem.view.private',
    'problem.edit',
    'problem.delete',
    'problem.manage.data',
    'problem.manage.asset',
    'judge.submit',
    'submission.view.own',
    'submission.view.all',
    'submission.rejudge',
    'submission.delete',
    'forum.post',
    'forum.moderate'
)
WHERE r.name = 'admin'
ON CONFLICT DO NOTHING;

INSERT INTO role_permissions(role_id, permission_code)
SELECT r.id, p.code
FROM roles r
JOIN permissions p ON p.code IN (
    'problem.view',
    'judge.submit',
    'submission.view.own',
    'forum.post'
)
WHERE r.name = 'user'
ON CONFLICT DO NOTHING;

INSERT INTO role_permissions(role_id, permission_code)
SELECT r.id, p.code
FROM roles r
JOIN permissions p ON p.code IN (
    'problem.view',
    'problem.view.private',
    'problem.edit',
    'problem.delete',
    'problem.manage.data',
    'problem.manage.asset',
    'submission.view.all',
    'submission.rejudge'
)
WHERE r.name = 'problem_owner'
ON CONFLICT DO NOTHING;

INSERT INTO role_permissions(role_id, permission_code)
SELECT r.id, p.code
FROM roles r
JOIN permissions p ON p.code IN (
    'problem.view',
    'problem.view.private',
    'problem.edit',
    'problem.manage.data',
    'problem.manage.asset'
)
WHERE r.name = 'problem_setter'
ON CONFLICT DO NOTHING;

INSERT INTO role_permissions(role_id, permission_code)
SELECT r.id, p.code
FROM roles r
JOIN permissions p ON p.code IN (
    'problem.view',
    'problem.view.private'
)
WHERE r.name = 'problem_viewer'
ON CONFLICT DO NOTHING;

INSERT INTO role_permissions(role_id, permission_code)
SELECT r.id, p.code
FROM roles r
JOIN permissions p ON p.code IN (
    'problem.manage.data',
    'problem.manage.asset'
)
WHERE r.name = 'problem_data_manager'
ON CONFLICT DO NOTHING;

INSERT INTO role_permissions(role_id, permission_code)
SELECT r.id, p.code
FROM roles r
JOIN permissions p ON p.code IN ('forum.moderate')
WHERE r.name = 'forum_moderator'
ON CONFLICT DO NOTHING;
