-- Permission check moves onto the orchestrator-resolved gateway route.
--
-- user-service, problem-service and judge-api no longer address auth-service
-- directly: they call the gateway with api_id auth.user.permission.check, and
-- the gateway authorises that call from the service permission grants below.

INSERT INTO permissions(code, service_code, name, description)
VALUES
    ('auth.permission.check', 'auth-service', 'Check User Permission',
     'Ask auth-service whether a user holds a permission inside a scope')
ON CONFLICT (code) DO UPDATE SET
    service_code = EXCLUDED.service_code,
    name = EXCLUDED.name,
    description = EXCLUDED.description;

INSERT INTO role_permissions(role_id, permission_code)
SELECT r.id, p.code
FROM roles r
JOIN permissions p ON p.code = 'auth.permission.check'
WHERE r.name IN ('super_admin', 'admin')
ON CONFLICT DO NOTHING;

INSERT INTO service_identities(service_code, enabled, updated_at)
VALUES
    ('user-service', TRUE, NOW())
ON CONFLICT(service_code)
DO UPDATE SET enabled = TRUE, updated_at = NOW();

-- Credentials are intentionally not seeded here. Issue one per caller through
-- POST /auth/admin/services/{service_code}/credentials before enabling
-- OJOS_AUTH_PERMISSION_GATEWAY_ENDPOINT for that service.
INSERT INTO service_permission_grants(caller_service_code, api_id, permission_code, provider_service_code, enabled, updated_at)
VALUES
    ('user-service', 'auth.user.permission.check', 'auth.permission.check', 'auth-service', TRUE, NOW()),
    ('problem-service', 'auth.user.permission.check', 'auth.permission.check', 'auth-service', TRUE, NOW()),
    ('judge-api', 'auth.user.permission.check', 'auth.permission.check', 'auth-service', TRUE, NOW())
ON CONFLICT(caller_service_code, api_id, permission_code)
DO UPDATE SET
    provider_service_code = EXCLUDED.provider_service_code,
    enabled = TRUE,
    updated_at = NOW();
