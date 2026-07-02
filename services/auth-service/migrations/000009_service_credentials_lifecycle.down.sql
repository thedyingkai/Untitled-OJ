DROP INDEX IF EXISTS idx_service_credentials_last_used;
DROP INDEX IF EXISTS idx_service_credentials_active;

ALTER TABLE service_credentials
    DROP COLUMN IF EXISTS last_used_at,
    DROP COLUMN IF EXISTS revoked_at,
    DROP COLUMN IF EXISTS expires_at;
