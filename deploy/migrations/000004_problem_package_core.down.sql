DELETE FROM resource_types
WHERE code = 'problem_package';

DROP INDEX IF EXISTS idx_problem_files_sha256;
DROP INDEX IF EXISTS idx_problem_files_kind;
DROP INDEX IF EXISTS idx_problem_files_problem_id;

DROP TABLE IF EXISTS problem_files;

DO $$
    BEGIN
        IF EXISTS (
            SELECT 1
            FROM pg_constraint
            WHERE conname = 'chk_problems_status'
        ) THEN
            ALTER TABLE problems
                DROP CONSTRAINT chk_problems_status;
        END IF;
    END $$;

DO $$
    BEGIN
        IF EXISTS (
            SELECT 1
            FROM pg_constraint
            WHERE conname = 'chk_problems_visibility'
        ) THEN
            ALTER TABLE problems
                DROP CONSTRAINT chk_problems_visibility;
        END IF;
    END $$;

DO $$
    BEGIN
        IF EXISTS (
            SELECT 1
            FROM pg_constraint
            WHERE conname = 'chk_problems_problem_type'
        ) THEN
            ALTER TABLE problems
                DROP CONSTRAINT chk_problems_problem_type;
        END IF;
    END $$;

DROP INDEX IF EXISTS idx_problems_created_by;
DROP INDEX IF EXISTS idx_problems_status;
DROP INDEX IF EXISTS idx_problems_visibility;
DROP INDEX IF EXISTS idx_problems_problem_type;
DROP INDEX IF EXISTS uq_problems_slug;

ALTER TABLE problems
    DROP COLUMN IF EXISTS created_by,
    DROP COLUMN IF EXISTS status,
    DROP COLUMN IF EXISTS source_fingerprint,
    DROP COLUMN IF EXISTS source_format,
    DROP COLUMN IF EXISTS manifest_sha256,
    DROP COLUMN IF EXISTS manifest_path,
    DROP COLUMN IF EXISTS package_dir,
    DROP COLUMN IF EXISTS visibility,
    DROP COLUMN IF EXISTS problem_type,
    DROP COLUMN IF EXISTS statement,
    DROP COLUMN IF EXISTS slug;