DROP INDEX IF EXISTS idx_problem_files_sha256;
DROP INDEX IF EXISTS idx_problem_files_kind;
DROP INDEX IF EXISTS idx_problem_files_problem_id;
DROP TABLE IF EXISTS problem_files;

DROP INDEX IF EXISTS idx_problems_title;
DROP INDEX IF EXISTS idx_problems_tags_gin;
DROP INDEX IF EXISTS idx_problems_difficulty;
DROP INDEX IF EXISTS idx_problems_created_by;
DROP INDEX IF EXISTS idx_problems_status;
DROP INDEX IF EXISTS idx_problems_visibility;
DROP INDEX IF EXISTS idx_problems_problem_type;
DROP INDEX IF EXISTS uq_problems_slug;
DROP TABLE IF EXISTS problems;
