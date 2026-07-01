CREATE TABLE IF NOT EXISTS problems (
    id BIGSERIAL PRIMARY KEY,
    title TEXT NOT NULL,
    time_limit_ms INT NOT NULL DEFAULT 1000,
    memory_limit_mb INT NOT NULL DEFAULT 256,
    slug TEXT,
    statement TEXT NOT NULL DEFAULT '',
    problem_type TEXT NOT NULL DEFAULT 'traditional',
    visibility TEXT NOT NULL DEFAULT 'private',
    package_dir TEXT NOT NULL DEFAULT '',
    manifest_path TEXT NOT NULL DEFAULT 'problem.yaml',
    manifest_sha256 TEXT NOT NULL DEFAULT '',
    source_format TEXT NOT NULL DEFAULT 'ojos',
    source_fingerprint TEXT NOT NULL DEFAULT '',
    status TEXT NOT NULL DEFAULT 'draft',
    created_by BIGINT,
    difficulty TEXT NOT NULL DEFAULT 'medium',
    tags TEXT[] NOT NULL DEFAULT '{}'::text[],
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    CONSTRAINT chk_problems_problem_type CHECK (
        problem_type IN (
            'traditional',
            'interactive',
            'communication',
            'output_only',
            'heuristic'
        )
    ),
    CONSTRAINT chk_problems_visibility CHECK (visibility IN ('private', 'public')),
    CONSTRAINT chk_problems_status CHECK (status IN ('draft', 'ready', 'published', 'archived')),
    CONSTRAINT chk_problems_difficulty CHECK (difficulty IN ('easy', 'medium', 'hard'))
);

CREATE UNIQUE INDEX IF NOT EXISTS uq_problems_slug
    ON problems(slug)
    WHERE slug IS NOT NULL AND slug <> '';

CREATE INDEX IF NOT EXISTS idx_problems_problem_type ON problems(problem_type);
CREATE INDEX IF NOT EXISTS idx_problems_visibility ON problems(visibility);
CREATE INDEX IF NOT EXISTS idx_problems_status ON problems(status);
CREATE INDEX IF NOT EXISTS idx_problems_created_by ON problems(created_by);
CREATE INDEX IF NOT EXISTS idx_problems_difficulty ON problems(difficulty);
CREATE INDEX IF NOT EXISTS idx_problems_tags_gin ON problems USING GIN(tags);
CREATE INDEX IF NOT EXISTS idx_problems_title ON problems(title);

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

CREATE INDEX IF NOT EXISTS idx_problem_files_problem_id ON problem_files(problem_id);
CREATE INDEX IF NOT EXISTS idx_problem_files_kind ON problem_files(problem_id, file_kind);
CREATE INDEX IF NOT EXISTS idx_problem_files_sha256 ON problem_files(sha256);
