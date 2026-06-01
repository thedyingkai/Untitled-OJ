ALTER TABLE roles
    ADD COLUMN IF NOT EXISTS module_code TEXT NOT NULL DEFAULT 'core',
    ADD COLUMN IF NOT EXISTS description TEXT NOT NULL DEFAULT '',
    ADD COLUMN IF NOT EXISTS is_system BOOLEAN NOT NULL DEFAULT false,
    ADD COLUMN IF NOT EXISTS created_at TIMESTAMPTZ NOT NULL DEFAULT NOW();

CREATE UNIQUE INDEX IF NOT EXISTS idx_roles_name_unique
    ON roles(name);

CREATE TABLE IF NOT EXISTS resource_types (
                                              code TEXT PRIMARY KEY,
                                              module_code TEXT NOT NULL DEFAULT 'core',
                                              name TEXT NOT NULL,
                                              description TEXT NOT NULL DEFAULT '',
                                              created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE TABLE IF NOT EXISTS permissions (
                                           code TEXT PRIMARY KEY,
                                           module_code TEXT NOT NULL DEFAULT 'core',
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

                                                      CONSTRAINT chk_permission_assignments_effect
                                                          CHECK (effect IN ('allow', 'deny')),

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

INSERT INTO resource_types(code, module_code, name, description) VALUES
                                                                     ('system', 'core', 'System', 'Global system scope'),
                                                                     ('module', 'core', 'Module', 'Platform module resource'),
                                                                     ('problem', 'core', 'Problem', 'Problem resource'),
                                                                     ('contest', 'core', 'Contest', 'Contest resource'),
                                                                     ('group', 'core', 'Group', 'Organization or group resource'),
                                                                     ('team', 'core', 'Team', 'Contest team resource'),
                                                                     ('submission', 'core', 'Submission', 'Submission resource'),
                                                                     ('post', 'core', 'Post', 'Forum post resource'),
                                                                     ('clarification', 'core', 'Clarification', 'Contest clarification resource'),
                                                                     ('balloon', 'core', 'Balloon', 'Contest balloon job resource'),
                                                                     ('print', 'core', 'Print', 'Contest print job resource')
ON CONFLICT (code) DO UPDATE SET
                                 module_code = EXCLUDED.module_code,
                                 name = EXCLUDED.name,
                                 description = EXCLUDED.description;

INSERT INTO permissions(code, module_code, name, description) VALUES
                                                                  ('system.admin', 'core', 'System Admin', 'Full system administration permission'),

                                                                  ('module.install', 'core', 'Install Module', 'Install platform modules'),
                                                                  ('module.enable', 'core', 'Enable Module', 'Enable platform modules'),
                                                                  ('module.disable', 'core', 'Disable Module', 'Disable platform modules'),
                                                                  ('module.configure', 'core', 'Configure Module', 'Configure platform modules'),

                                                                  ('launcher.view', 'core', 'View Launcher', 'View module launcher'),
                                                                  ('launcher.install', 'core', 'Install Through Launcher', 'Install modules through launcher'),
                                                                  ('launcher.uninstall', 'core', 'Uninstall Through Launcher', 'Uninstall modules through launcher'),
                                                                  ('launcher.enable', 'core', 'Enable Through Launcher', 'Enable modules through launcher'),
                                                                  ('launcher.disable', 'core', 'Disable Through Launcher', 'Disable modules through launcher'),

                                                                  ('problem.create', 'core', 'Create Problem', 'Create new problems'),
                                                                  ('problem.view', 'core', 'View Problem', 'View problems'),
                                                                  ('problem.view.private', 'core', 'View Private Problem', 'View private problems'),
                                                                  ('problem.edit', 'core', 'Edit Problem', 'Edit problem metadata and statement'),
                                                                  ('problem.delete', 'core', 'Delete Problem', 'Delete problems'),
                                                                  ('problem.manage.data', 'core', 'Manage Problem Data', 'Manage test cases'),
                                                                  ('problem.manage.asset', 'core', 'Manage Problem Assets', 'Manage checker, scorer, interactor and other assets'),

                                                                  ('judge.submit', 'core', 'Submit Code', 'Submit code to judge'),

                                                                  ('submission.view.own', 'core', 'View Own Submission', 'View own submissions'),
                                                                  ('submission.view.all', 'core', 'View All Submissions', 'View all submissions'),
                                                                  ('submission.rejudge', 'core', 'Rejudge Submission', 'Rejudge submissions'),
                                                                  ('submission.delete', 'core', 'Delete Submission', 'Delete submissions'),

                                                                  ('contest.create', 'core', 'Create Contest', 'Create contests'),
                                                                  ('contest.view', 'core', 'View Contest', 'View contests'),
                                                                  ('contest.manage', 'core', 'Manage Contest', 'Manage contest settings'),
                                                                  ('contest.manage.participant', 'core', 'Manage Contest Participants', 'Manage contest participants'),
                                                                  ('contest.manage.problem', 'core', 'Manage Contest Problems', 'Manage contest problems'),
                                                                  ('contest.freeze', 'core', 'Freeze Contest', 'Freeze contest scoreboard'),
                                                                  ('contest.roll', 'core', 'Roll Contest', 'Run rolling scoreboard'),
                                                                  ('contest.publish', 'core', 'Publish Contest', 'Publish contest'),

                                                                  ('scoreboard.view', 'core', 'View Scoreboard', 'View scoreboard'),
                                                                  ('scoreboard.view.admin', 'core', 'View Admin Scoreboard', 'View full admin scoreboard'),
                                                                  ('scoreboard.freeze', 'core', 'Freeze Scoreboard', 'Freeze scoreboard'),
                                                                  ('scoreboard.roll', 'core', 'Roll Scoreboard', 'Run rolling scoreboard'),
                                                                  ('scoreboard.export', 'core', 'Export Scoreboard', 'Export scoreboard'),

                                                                  ('balloon.manage', 'core', 'Manage Balloons', 'Manage contest balloons'),
                                                                  ('balloon.deliver', 'core', 'Deliver Balloons', 'Deliver contest balloons'),

                                                                  ('print.request', 'core', 'Request Printing', 'Request code printing'),
                                                                  ('print.manage', 'core', 'Manage Printing', 'Manage print jobs'),
                                                                  ('print.operate', 'core', 'Operate Printer', 'Operate printers'),

                                                                  ('forum.post', 'core', 'Post Forum', 'Create forum posts'),
                                                                  ('forum.moderate', 'core', 'Moderate Forum', 'Moderate forum posts'),

                                                                  ('clarification.ask', 'core', 'Ask Clarification', 'Ask contest clarification'),
                                                                  ('clarification.answer', 'core', 'Answer Clarification', 'Answer contest clarification'),
                                                                  ('clarification.publish', 'core', 'Publish Clarification', 'Publish clarification')
ON CONFLICT (code) DO UPDATE SET
                                 module_code = EXCLUDED.module_code,
                                 name = EXCLUDED.name,
                                 description = EXCLUDED.description;

INSERT INTO roles(name, module_code, description, is_system) VALUES
                                                                 ('super_admin', 'core', 'Full system super administrator', true),
                                                                 ('admin', 'core', 'System administrator', true),
                                                                 ('user', 'core', 'Normal user', true),

                                                                 ('module_manager', 'core', 'Module manager', true),

                                                                 ('problem_owner', 'core', 'Problem owner', true),
                                                                 ('problem_setter', 'core', 'Problem setter', true),
                                                                 ('problem_viewer', 'core', 'Problem viewer', true),
                                                                 ('problem_data_manager', 'core', 'Problem data manager', true),

                                                                 ('contest_owner', 'core', 'Contest owner', true),
                                                                 ('contest_manager', 'core', 'Contest manager', true),
                                                                 ('contest_judge', 'core', 'Contest judge', true),
                                                                 ('contest_participant', 'core', 'Contest participant', true),

                                                                 ('balloon_volunteer', 'core', 'Balloon volunteer', true),
                                                                 ('print_operator', 'core', 'Print operator', true),
                                                                 ('forum_moderator', 'core', 'Forum moderator', true)
ON CONFLICT (name) DO UPDATE SET
                                 module_code = EXCLUDED.module_code,
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

                                          'module.install',
                                          'module.enable',
                                          'module.disable',
                                          'module.configure',

                                          'launcher.view',
                                          'launcher.install',
                                          'launcher.uninstall',
                                          'launcher.enable',
                                          'launcher.disable',

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

                                          'contest.create',
                                          'contest.view',
                                          'contest.manage',
                                          'contest.manage.participant',
                                          'contest.manage.problem',
                                          'contest.freeze',
                                          'contest.roll',
                                          'contest.publish',

                                          'scoreboard.view',
                                          'scoreboard.view.admin',
                                          'scoreboard.freeze',
                                          'scoreboard.roll',
                                          'scoreboard.export',

                                          'balloon.manage',
                                          'balloon.deliver',

                                          'print.request',
                                          'print.manage',
                                          'print.operate',

                                          'forum.post',
                                          'forum.moderate',

                                          'clarification.ask',
                                          'clarification.answer',
                                          'clarification.publish'
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
                                          'contest.view',
                                          'scoreboard.view',
                                          'print.request',
                                          'forum.post',
                                          'clarification.ask'
    )
WHERE r.name = 'user'
ON CONFLICT DO NOTHING;

INSERT INTO role_permissions(role_id, permission_code)
SELECT r.id, p.code
FROM roles r
         JOIN permissions p ON p.code IN (
                                          'module.install',
                                          'module.enable',
                                          'module.disable',
                                          'module.configure',
                                          'launcher.view',
                                          'launcher.install',
                                          'launcher.uninstall',
                                          'launcher.enable',
                                          'launcher.disable'
    )
WHERE r.name = 'module_manager'
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
         JOIN permissions p ON p.code IN (
                                          'contest.view',
                                          'contest.manage',
                                          'contest.manage.participant',
                                          'contest.manage.problem',
                                          'contest.freeze',
                                          'contest.roll',
                                          'contest.publish',
                                          'scoreboard.view',
                                          'scoreboard.view.admin',
                                          'scoreboard.freeze',
                                          'scoreboard.roll',
                                          'scoreboard.export',
                                          'submission.view.all',
                                          'submission.rejudge',
                                          'balloon.manage',
                                          'print.manage',
                                          'clarification.answer',
                                          'clarification.publish'
    )
WHERE r.name = 'contest_owner'
ON CONFLICT DO NOTHING;

INSERT INTO role_permissions(role_id, permission_code)
SELECT r.id, p.code
FROM roles r
         JOIN permissions p ON p.code IN (
                                          'contest.view',
                                          'contest.manage',
                                          'contest.manage.participant',
                                          'contest.manage.problem',
                                          'contest.freeze',
                                          'contest.roll',
                                          'scoreboard.view',
                                          'scoreboard.view.admin',
                                          'submission.view.all',
                                          'submission.rejudge',
                                          'clarification.answer',
                                          'clarification.publish'
    )
WHERE r.name = 'contest_manager'
ON CONFLICT DO NOTHING;

INSERT INTO role_permissions(role_id, permission_code)
SELECT r.id, p.code
FROM roles r
         JOIN permissions p ON p.code IN (
                                          'contest.view',
                                          'scoreboard.view.admin',
                                          'submission.view.all',
                                          'submission.rejudge',
                                          'clarification.answer',
                                          'clarification.publish'
    )
WHERE r.name = 'contest_judge'
ON CONFLICT DO NOTHING;

INSERT INTO role_permissions(role_id, permission_code)
SELECT r.id, p.code
FROM roles r
         JOIN permissions p ON p.code IN (
                                          'contest.view',
                                          'scoreboard.view',
                                          'judge.submit',
                                          'submission.view.own',
                                          'print.request',
                                          'clarification.ask'
    )
WHERE r.name = 'contest_participant'
ON CONFLICT DO NOTHING;

INSERT INTO role_permissions(role_id, permission_code)
SELECT r.id, p.code
FROM roles r
         JOIN permissions p ON p.code IN (
                                          'balloon.manage',
                                          'balloon.deliver'
    )
WHERE r.name = 'balloon_volunteer'
ON CONFLICT DO NOTHING;

INSERT INTO role_permissions(role_id, permission_code)
SELECT r.id, p.code
FROM roles r
         JOIN permissions p ON p.code IN (
                                          'print.manage',
                                          'print.operate'
    )
WHERE r.name = 'print_operator'
ON CONFLICT DO NOTHING;

INSERT INTO role_permissions(role_id, permission_code)
SELECT r.id, p.code
FROM roles r
         JOIN permissions p ON p.code IN (
                                          'forum.moderate',
                                          'clarification.publish'
    )
WHERE r.name = 'forum_moderator'
ON CONFLICT DO NOTHING;