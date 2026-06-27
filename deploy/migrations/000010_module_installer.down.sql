DELETE FROM role_permissions
WHERE permission_code IN ('module.rollback', 'module.uninstall');

DELETE FROM permissions
WHERE code IN ('module.rollback', 'module.uninstall');

DROP TABLE IF EXISTS module_operations;
DROP TABLE IF EXISTS module_operation_locks;
