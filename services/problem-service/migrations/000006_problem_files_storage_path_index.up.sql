-- Keep this migration to one statement and do not wrap it in BEGIN/COMMIT.
-- PostgreSQL requires CONCURRENTLY to run outside a transaction block.
CREATE INDEX CONCURRENTLY IF NOT EXISTS idx_problem_files_storage_path
    ON problem_files(storage_path)
    WHERE storage_path LIKE 'storage://%';
