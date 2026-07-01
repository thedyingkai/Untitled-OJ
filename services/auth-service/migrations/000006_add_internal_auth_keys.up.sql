CREATE TABLE IF NOT EXISTS internal_auth_keys (
    id BIGSERIAL PRIMARY KEY,
    key_id TEXT NOT NULL UNIQUE,
    secret BYTEA NOT NULL,
    not_before TIMESTAMPTZ NOT NULL,
    not_after TIMESTAMPTZ NOT NULL,
    verify_until TIMESTAMPTZ NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    CONSTRAINT chk_internal_auth_secret_len CHECK (octet_length(secret) >= 32),
    CONSTRAINT chk_internal_auth_time_order CHECK (
        not_before < not_after
        AND not_after <= verify_until
    )
);

CREATE INDEX IF NOT EXISTS idx_internal_auth_keys_signing
    ON internal_auth_keys(not_before, not_after);

CREATE INDEX IF NOT EXISTS idx_internal_auth_keys_verify
    ON internal_auth_keys(key_id, not_before, verify_until);
