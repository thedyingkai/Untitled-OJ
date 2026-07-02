ALTER TABLE service_credentials
    ADD COLUMN IF NOT EXISTS expires_at TIMESTAMPTZ,
    ADD COLUMN IF NOT EXISTS revoked_at TIMESTAMPTZ,
    ADD COLUMN IF NOT EXISTS last_used_at TIMESTAMPTZ;

CREATE INDEX IF NOT EXISTS idx_service_credentials_active
    ON service_credentials(service_code, token_hash)
    WHERE enabled AND revoked_at IS NULL;

CREATE INDEX IF NOT EXISTS idx_service_credentials_last_used
    ON service_credentials(service_code, last_used_at);
