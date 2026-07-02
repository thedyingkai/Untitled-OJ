INSERT INTO permissions(code, service_code, name, description)
VALUES
    ('auth.admin', 'auth-service', 'Auth Admin', 'Manage auth roles, permissions, users and service identities'),
    ('gateway.read', 'gateway', 'Gateway Read', 'Read gateway administration and service topology state'),
    ('storage.object.read', 'storage-service', 'Read Storage Object', 'Read object metadata and object content'),
    ('storage.object.write', 'storage-service', 'Write Storage Object', 'Create or replace object content'),
    ('storage.object.delete', 'storage-service', 'Delete Storage Object', 'Delete object content'),
    ('user.profile.read.self', 'user-service', 'Read Own User Profile', 'Read the current user profile'),
    ('user.profile.update.self', 'user-service', 'Update Own User Profile', 'Update the current user profile'),
    ('user.profile.read.any', 'user-service', 'Read Any User Profile', 'Read any user profile'),
    ('user.profile.update.any', 'user-service', 'Update Any User Profile', 'Update any user profile'),
    ('user.stats.read', 'user-service', 'Read User Stats', 'Read user statistics'),
    ('problem.testdata.read', 'problem-service', 'Read Problem Test Data', 'Read problem package test data'),
    ('problem.testdata.write', 'problem-service', 'Write Problem Test Data', 'Create, update or delete problem package test data'),
    ('judge.admin', 'judge-api', 'Judge Admin', 'Administer judge queue, tasks and workers'),
    ('judge.worker.status', 'judge-api', 'Read Judge Worker Status', 'Read judge worker status')
ON CONFLICT (code) DO UPDATE SET
    service_code = EXCLUDED.service_code,
    name = EXCLUDED.name,
    description = EXCLUDED.description;

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
    'auth.admin',
    'gateway.read',
    'storage.object.read',
    'storage.object.write',
    'storage.object.delete',
    'user.profile.read.self',
    'user.profile.update.self',
    'user.profile.read.any',
    'user.profile.update.any',
    'user.stats.read',
    'problem.testdata.read',
    'problem.testdata.write',
    'judge.admin',
    'judge.worker.status'
)
WHERE r.name = 'admin'
ON CONFLICT DO NOTHING;

INSERT INTO role_permissions(role_id, permission_code)
SELECT r.id, p.code
FROM roles r
JOIN permissions p ON p.code IN (
    'user.profile.read.self',
    'user.profile.update.self',
    'user.stats.read'
)
WHERE r.name = 'user'
ON CONFLICT DO NOTHING;

INSERT INTO role_permissions(role_id, permission_code)
SELECT r.id, p.code
FROM roles r
JOIN permissions p ON p.code IN (
    'problem.testdata.read',
    'problem.testdata.write'
)
WHERE r.name IN ('problem_owner', 'problem_setter', 'problem_data_manager')
ON CONFLICT DO NOTHING;

INSERT INTO service_identities(service_code, enabled, updated_at)
VALUES
    ('problem-service', TRUE, NOW()),
    ('judge-api', TRUE, NOW()),
    ('judge-worker', TRUE, NOW())
ON CONFLICT(service_code)
DO UPDATE SET enabled = TRUE, updated_at = NOW();

INSERT INTO service_credentials(service_code, token_hash, token_hint, enabled, expires_at, revoked_at, updated_at)
VALUES
    ('problem-service', 'sha256:36cb03969aef6dbdd862a09dbbef1ea81f4f1bacb4aa55bff4e286235b081ab4', 'ojos...rnal', TRUE, NULL, NULL, NOW()),
    ('judge-api', 'sha256:36cb03969aef6dbdd862a09dbbef1ea81f4f1bacb4aa55bff4e286235b081ab4', 'ojos...rnal', TRUE, NULL, NULL, NOW()),
    ('judge-worker', 'sha256:36cb03969aef6dbdd862a09dbbef1ea81f4f1bacb4aa55bff4e286235b081ab4', 'ojos...rnal', TRUE, NULL, NULL, NOW())
ON CONFLICT(service_code, token_hash)
DO UPDATE SET
    token_hint = EXCLUDED.token_hint,
    enabled = TRUE,
    expires_at = NULL,
    revoked_at = NULL,
    updated_at = NOW();

INSERT INTO service_permission_grants(caller_service_code, api_id, permission_code, provider_service_code, enabled, updated_at)
VALUES
    ('problem-service', 'storage.object.put', 'storage.object.write', 'storage-service', TRUE, NOW()),
    ('judge-api', 'storage.object.get', 'storage.object.read', 'storage-service', TRUE, NOW()),
    ('judge-api', 'storage.object.head', 'storage.object.read', 'storage-service', TRUE, NOW()),
    ('judge-api', 'storage.object.put', 'storage.object.write', 'storage-service', TRUE, NOW()),
    ('judge-worker', 'storage.object.get', 'storage.object.read', 'storage-service', TRUE, NOW()),
    ('judge-worker', 'storage.object.put', 'storage.object.write', 'storage-service', TRUE, NOW())
ON CONFLICT(caller_service_code, api_id, permission_code)
DO UPDATE SET
    provider_service_code = EXCLUDED.provider_service_code,
    enabled = TRUE,
    updated_at = NOW();
