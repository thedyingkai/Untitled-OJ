INSERT INTO role_permissions(role_id, permission_code)
SELECT r.id, p.code
FROM roles r
JOIN permissions p ON p.code = 'problem.create'
WHERE r.name = 'problem_setter'
ON CONFLICT DO NOTHING;
