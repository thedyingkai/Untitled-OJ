-- Keep this migration to one statement and do not wrap it in BEGIN/COMMIT.
-- PostgreSQL requires CONCURRENTLY to run outside a transaction block.
DROP INDEX CONCURRENTLY IF EXISTS idx_problem_files_storage_path;
