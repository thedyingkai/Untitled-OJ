DELETE FROM role_permissions
WHERE permission_code = 'problem.create'
  AND role_id IN (
      SELECT id FROM roles WHERE name = 'problem_setter'
  );
