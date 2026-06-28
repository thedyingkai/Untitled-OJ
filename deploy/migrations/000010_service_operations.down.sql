DELETE FROM role_permissions
WHERE permission_code IN ('service.rollback', 'service.uninstall');

DELETE FROM permissions
WHERE code IN ('service.rollback', 'service.uninstall');

DROP TABLE IF EXISTS service_runtime_operations;
DROP TABLE IF EXISTS service_operation_locks;
