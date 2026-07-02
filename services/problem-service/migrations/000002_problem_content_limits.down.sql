DROP TABLE IF EXISTS problem_language_limits;
DROP INDEX IF EXISTS uq_problems_problem_no;

ALTER TABLE problems
    DROP COLUMN IF EXISTS solution_format,
    DROP COLUMN IF EXISTS solution,
    DROP COLUMN IF EXISTS statement_format,
    DROP COLUMN IF EXISTS problem_no;
