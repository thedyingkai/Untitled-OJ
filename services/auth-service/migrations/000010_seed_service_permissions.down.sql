DELETE FROM service_permission_grants
WHERE (caller_service_code, api_id, permission_code) IN (
    ('problem-service', 'storage.object.put', 'storage.object.write'),
    ('judge-api', 'storage.object.get', 'storage.object.read'),
    ('judge-api', 'storage.object.head', 'storage.object.read'),
    ('judge-api', 'storage.object.put', 'storage.object.write'),
    ('judge-worker', 'storage.object.get', 'storage.object.read'),
    ('judge-worker', 'storage.object.put', 'storage.object.write')
);

DELETE FROM service_credentials
WHERE service_code IN ('problem-service', 'judge-api', 'judge-worker')
  AND token_hash = 'sha256:36cb03969aef6dbdd862a09dbbef1ea81f4f1bacb4aa55bff4e286235b081ab4';

DELETE FROM role_permissions
WHERE permission_code IN (
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
);

DELETE FROM permissions
WHERE code IN (
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
);
