CREATE TABLE IF NOT EXISTS service_identities (
    service_code TEXT PRIMARY KEY,
    enabled BOOLEAN NOT NULL DEFAULT TRUE,
    token_hint TEXT NOT NULL DEFAULT '',
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE TABLE IF NOT EXISTS service_permission_grants (
    caller_service_code TEXT NOT NULL REFERENCES service_identities(service_code) ON DELETE CASCADE,
    api_id TEXT NOT NULL,
    permission_code TEXT NOT NULL REFERENCES permissions(code) ON DELETE CASCADE,
    provider_service_code TEXT NOT NULL DEFAULT '',
    enabled BOOLEAN NOT NULL DEFAULT TRUE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    PRIMARY KEY(caller_service_code, api_id, permission_code)
);

CREATE INDEX IF NOT EXISTS idx_service_permission_grants_permission
    ON service_permission_grants(permission_code);

CREATE INDEX IF NOT EXISTS idx_service_permission_grants_api
    ON service_permission_grants(api_id);
