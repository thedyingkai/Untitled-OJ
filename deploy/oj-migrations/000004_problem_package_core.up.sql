ALTER TABLE problems
    ADD COLUMN IF NOT EXISTS slug TEXT,
    ADD COLUMN IF NOT EXISTS statement TEXT NOT NULL DEFAULT '',
    ADD COLUMN IF NOT EXISTS problem_type TEXT NOT NULL DEFAULT 'traditional',
    ADD COLUMN IF NOT EXISTS visibility TEXT NOT NULL DEFAULT 'private',
    ADD COLUMN IF NOT EXISTS package_dir TEXT NOT NULL DEFAULT '',
    ADD COLUMN IF NOT EXISTS manifest_path TEXT NOT NULL DEFAULT 'problem.yaml',
    ADD COLUMN IF NOT EXISTS manifest_sha256 TEXT NOT NULL DEFAULT '',
    ADD COLUMN IF NOT EXISTS source_format TEXT NOT NULL DEFAULT 'ojos',
    ADD COLUMN IF NOT EXISTS source_fingerprint TEXT NOT NULL DEFAULT '',
    ADD COLUMN IF NOT EXISTS status TEXT NOT NULL DEFAULT 'draft',
    ADD COLUMN IF NOT EXISTS created_by BIGINT REFERENCES users(id) ON DELETE SET NULL;

CREATE UNIQUE INDEX IF NOT EXISTS uq_problems_slug
    ON problems(slug)
    WHERE slug IS NOT NULL AND slug <> '';

CREATE INDEX IF NOT EXISTS idx_problems_problem_type
    ON problems(problem_type);

CREATE INDEX IF NOT EXISTS idx_problems_visibility
    ON problems(visibility);

CREATE INDEX IF NOT EXISTS idx_problems_status
    ON problems(status);

CREATE INDEX IF NOT EXISTS idx_problems_created_by
    ON problems(created_by);

DO $$
    BEGIN
        IF NOT EXISTS (
            SELECT 1
            FROM pg_constraint
            WHERE conname = 'chk_problems_problem_type'
        ) THEN
            ALTER TABLE problems
                ADD CONSTRAINT chk_problems_problem_type
                    CHECK (
                        problem_type IN (
                                         'traditional',
                                         'interactive',
                                         'communication',
                                         'output_only',
                                         'heuristic'
                            )
                        );
        END IF;
    END $$;

DO $$
    BEGIN
        IF NOT EXISTS (
            SELECT 1
            FROM pg_constraint
            WHERE conname = 'chk_problems_visibility'
        ) THEN
            ALTER TABLE problems
                ADD CONSTRAINT chk_problems_visibility
                    CHECK (
                        visibility IN (
                                       'private',
                                       'public'
                            )
                        );
        END IF;
    END $$;

DO $$
    BEGIN
        IF NOT EXISTS (
            SELECT 1
            FROM pg_constraint
            WHERE conname = 'chk_problems_status'
        ) THEN
            ALTER TABLE problems
                ADD CONSTRAINT chk_problems_status
                    CHECK (
                        status IN (
                                   'draft',
                                   'ready',
                                   'published',
                                   'archived'
                            )
                        );
        END IF;
    END $$;

CREATE TABLE IF NOT EXISTS problem_files (
                                             id BIGSERIAL PRIMARY KEY,

                                             problem_id BIGINT NOT NULL REFERENCES problems(id) ON DELETE CASCADE,

                                             logical_path TEXT NOT NULL,
                                             file_kind TEXT NOT NULL,
                                             storage_path TEXT NOT NULL,

                                             sha256 TEXT NOT NULL DEFAULT '',
                                             size_bytes BIGINT NOT NULL DEFAULT 0,
                                             mime_type TEXT NOT NULL DEFAULT '',

                                             created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),

                                             UNIQUE(problem_id, logical_path)
);

CREATE INDEX IF NOT EXISTS idx_problem_files_problem_id
    ON problem_files(problem_id);

CREATE INDEX IF NOT EXISTS idx_problem_files_kind
    ON problem_files(problem_id, file_kind);

CREATE INDEX IF NOT EXISTS idx_problem_files_sha256
    ON problem_files(sha256);

INSERT INTO resource_types(code, service_code, name, description)
VALUES
    ('problem_package', 'problem-core', 'Problem Package', 'Canonical file-based problem package')
ON CONFLICT(code) DO UPDATE SET
                                service_code = EXCLUDED.service_code,
                                name = EXCLUDED.name,
                                description = EXCLUDED.description;
