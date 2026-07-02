CREATE TABLE IF NOT EXISTS service_credentials (
    service_code TEXT NOT NULL REFERENCES service_identities(service_code) ON DELETE CASCADE,
    token_hash TEXT NOT NULL,
    token_hint TEXT NOT NULL DEFAULT '',
    enabled BOOLEAN NOT NULL DEFAULT TRUE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    PRIMARY KEY(service_code, token_hash)
);

CREATE INDEX IF NOT EXISTS idx_service_credentials_token_hash
    ON service_credentials(token_hash);
