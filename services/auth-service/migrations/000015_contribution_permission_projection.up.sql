CREATE TABLE IF NOT EXISTS contribution_permission_projection (
    singleton BOOLEAN PRIMARY KEY DEFAULT TRUE CHECK (singleton),
    snapshot_digest TEXT NOT NULL CHECK (snapshot_digest ~ '^sha256:[0-9a-f]{64}$'),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp()
);

CREATE TABLE IF NOT EXISTS contribution_permission_definitions (
    permission_code TEXT PRIMARY KEY REFERENCES permissions(code) ON DELETE RESTRICT,
    service_code TEXT NOT NULL,
    snapshot_digest TEXT NOT NULL CHECK (snapshot_digest ~ '^sha256:[0-9a-f]{64}$'),
    active BOOLEAN NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp()
);

CREATE INDEX IF NOT EXISTS idx_contribution_permission_definitions_active
    ON contribution_permission_definitions(active, service_code, permission_code);
