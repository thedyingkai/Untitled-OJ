DROP INDEX IF EXISTS idx_problems_title;
DROP INDEX IF EXISTS idx_problems_tags_gin;
DROP INDEX IF EXISTS idx_problems_difficulty;

ALTER TABLE problems
    DROP CONSTRAINT IF EXISTS chk_problems_difficulty,
    DROP COLUMN IF EXISTS tags,
    DROP COLUMN IF EXISTS difficulty;
