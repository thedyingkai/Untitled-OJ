CREATE TABLE IF NOT EXISTS problems (
                                        id BIGSERIAL PRIMARY KEY,

                                        title TEXT NOT NULL,
                                        time_limit_ms INT NOT NULL DEFAULT 1000,
                                        memory_limit_mb INT NOT NULL DEFAULT 256,

                                        created_at TIMESTAMP NOT NULL DEFAULT NOW(),
                                        updated_at TIMESTAMP NOT NULL DEFAULT NOW()
);

CREATE TABLE IF NOT EXISTS test_cases (
                                          id BIGSERIAL PRIMARY KEY,

                                          problem_id BIGINT NOT NULL REFERENCES problems(id) ON DELETE CASCADE,

                                          input TEXT NOT NULL,
                                          output TEXT NOT NULL,
                                          score INT NOT NULL DEFAULT 100,

                                          created_at TIMESTAMP NOT NULL DEFAULT NOW()
);

CREATE TABLE IF NOT EXISTS submissions (
                                           id BIGSERIAL PRIMARY KEY,

                                           problem_id BIGINT NOT NULL REFERENCES problems(id),
                                           user_id BIGINT NOT NULL DEFAULT 1,

                                           language TEXT NOT NULL,
                                           code TEXT NOT NULL,

                                           status TEXT NOT NULL DEFAULT 'PENDING',

                                           score INT NOT NULL DEFAULT 0,
                                           time_ms INT NOT NULL DEFAULT 0,
                                           memory_kb INT NOT NULL DEFAULT 0,
                                           message TEXT,

                                           created_at TIMESTAMP NOT NULL DEFAULT NOW(),
                                           updated_at TIMESTAMP NOT NULL DEFAULT NOW()
);

CREATE TABLE IF NOT EXISTS submission_cases (
                                                id BIGSERIAL PRIMARY KEY,

                                                submission_id BIGINT NOT NULL REFERENCES submissions(id) ON DELETE CASCADE,
                                                test_case_id BIGINT NOT NULL REFERENCES test_cases(id),

                                                status TEXT NOT NULL,
                                                time_ms INT NOT NULL DEFAULT 0,
                                                memory_kb INT NOT NULL DEFAULT 0,
                                                message TEXT,

                                                created_at TIMESTAMP NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_test_cases_problem_id
    ON test_cases(problem_id);

CREATE INDEX IF NOT EXISTS idx_submissions_problem_id
    ON submissions(problem_id);

CREATE INDEX IF NOT EXISTS idx_submissions_user_id
    ON submissions(user_id);

CREATE INDEX IF NOT EXISTS idx_submissions_status
    ON submissions(status);

CREATE INDEX IF NOT EXISTS idx_submission_cases_submission_id
    ON submission_cases(submission_id);