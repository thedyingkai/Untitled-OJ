ALTER TABLE problems
    ADD COLUMN IF NOT EXISTS difficulty TEXT NOT NULL DEFAULT 'medium',
    ADD COLUMN IF NOT EXISTS tags TEXT[] NOT NULL DEFAULT '{}'::text[];

DO $$
    BEGIN
        IF NOT EXISTS (
            SELECT 1
            FROM pg_constraint
            WHERE conname = 'chk_problems_difficulty'
        ) THEN
            ALTER TABLE problems
                ADD CONSTRAINT chk_problems_difficulty
                    CHECK (difficulty IN ('easy', 'medium', 'hard'));
        END IF;
    END $$;

CREATE INDEX IF NOT EXISTS idx_problems_difficulty
    ON problems(difficulty);

CREATE INDEX IF NOT EXISTS idx_problems_tags_gin
    ON problems USING GIN(tags);

CREATE INDEX IF NOT EXISTS idx_problems_title
    ON problems(title);
