DELETE FROM service_permission_grants
WHERE api_id = 'auth.user.permission.check'
  AND permission_code = 'auth.permission.check';

DELETE FROM role_permissions
WHERE permission_code = 'auth.permission.check';

DELETE FROM permissions
WHERE code = 'auth.permission.check';
