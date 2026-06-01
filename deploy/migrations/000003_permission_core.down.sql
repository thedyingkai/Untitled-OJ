DROP TABLE IF EXISTS permission_audit_logs;
DROP TABLE IF EXISTS resource_edges;
DROP TABLE IF EXISTS permission_assignments;
DROP TABLE IF EXISTS role_bindings;
DROP TABLE IF EXISTS role_permissions;
DROP TABLE IF EXISTS permissions;
DROP TABLE IF EXISTS resource_types;

DROP INDEX IF EXISTS idx_roles_name_unique;

ALTER TABLE roles
    DROP COLUMN IF EXISTS module_code,
    DROP COLUMN IF EXISTS description,
    DROP COLUMN IF EXISTS is_system,
    DROP COLUMN IF EXISTS created_at;