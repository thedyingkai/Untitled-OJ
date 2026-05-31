use anyhow::{Context, Result};
use sqlx::{PgPool, Row};

#[derive(Debug)]
pub struct Submission {
    pub id: i64,
    pub problem_id: i64,
    pub user_id: i64,
    pub language: String,
    pub code: String,
}

#[derive(Debug)]
pub struct Problem {
    pub id: i64,
    pub time_limit_ms: i32,
    pub memory_limit_mb: i32,
}

#[derive(Debug)]
pub struct TestCase {
    pub id: i64,
    pub input: String,
    pub output: String,
    pub score: i32,
}

#[derive(Debug)]
pub struct CaseResult {
    pub test_case_id: i64,
    pub status: String,
    pub time_ms: i32,
    pub memory_kb: i32,
    pub message: String,
    pub passed_score: i32,
}

#[derive(Debug)]
pub struct JudgeResult {
    pub status: String,
    pub score: i32,
    pub time_ms: i32,
    pub memory_kb: i32,
    pub message: String,
    pub cases: Vec<CaseResult>,
}

pub async fn load_submission(db: &PgPool, submission_id: i64) -> Result<Submission> {
    let row = sqlx::query(
        r#"
        SELECT id, problem_id, user_id, language, code
        FROM submissions
        WHERE id = $1
        "#,
    )
    .bind(submission_id)
    .fetch_one(db)
    .await
    .context("submission not found")?;

    Ok(Submission {
        id: row.try_get("id")?,
        problem_id: row.try_get("problem_id")?,
        user_id: row.try_get("user_id")?,
        language: row.try_get("language")?,
        code: row.try_get("code")?,
    })
}

pub async fn load_problem(db: &PgPool, problem_id: i64) -> Result<Problem> {
    let row = sqlx::query(
        r#"
        SELECT id, time_limit_ms, memory_limit_mb
        FROM problems
        WHERE id = $1
        "#,
    )
    .bind(problem_id)
    .fetch_one(db)
    .await
    .context("problem not found")?;

    Ok(Problem {
        id: row.try_get("id")?,
        time_limit_ms: row.try_get("time_limit_ms")?,
        memory_limit_mb: row.try_get("memory_limit_mb")?,
    })
}

pub async fn load_test_cases(db: &PgPool, problem_id: i64) -> Result<Vec<TestCase>> {
    let rows = sqlx::query(
        r#"
        SELECT id, input, output, score
        FROM test_cases
        WHERE problem_id = $1
        ORDER BY id
        "#,
    )
    .bind(problem_id)
    .fetch_all(db)
    .await?;

    let mut cases = Vec::with_capacity(rows.len());

    for row in rows {
        cases.push(TestCase {
            id: row.try_get("id")?,
            input: row.try_get("input")?,
            output: row.try_get("output")?,
            score: row.try_get("score")?,
        });
    }

    Ok(cases)
}

pub async fn update_submission_status(
    db: &PgPool,
    submission_id: i64,
    status: &str,
    score: i32,
    time_ms: i32,
    memory_kb: i32,
    message: &str,
) -> Result<()> {
    sqlx::query(
        r#"
        UPDATE submissions
        SET status = $2,
            score = $3,
            time_ms = $4,
            memory_kb = $5,
            message = $6,
            updated_at = NOW()
        WHERE id = $1
        "#,
    )
    .bind(submission_id)
    .bind(status)
    .bind(score)
    .bind(time_ms)
    .bind(memory_kb)
    .bind(message)
    .execute(db)
    .await?;

    Ok(())
}

pub async fn mark_submission_failed(
    db: &PgPool,
    submission_id: i64,
    status: &str,
    message: &str,
) -> Result<()> {
    update_submission_status(db, submission_id, status, 0, 0, 0, message).await
}

pub async fn save_judge_result(db: &PgPool, submission_id: i64, result: JudgeResult) -> Result<()> {
    let mut tx = db.begin().await?;

    sqlx::query(
        r#"
        DELETE FROM submission_cases
        WHERE submission_id = $1
        "#,
    )
    .bind(submission_id)
    .execute(&mut *tx)
    .await?;

    for case in &result.cases {
        sqlx::query(
            r#"
            INSERT INTO submission_cases(
                submission_id,
                test_case_id,
                status,
                time_ms,
                memory_kb,
                message
            )
            VALUES($1, $2, $3, $4, $5, $6)
            "#,
        )
        .bind(submission_id)
        .bind(case.test_case_id)
        .bind(&case.status)
        .bind(case.time_ms)
        .bind(case.memory_kb)
        .bind(&case.message)
        .execute(&mut *tx)
        .await?;
    }

    sqlx::query(
        r#"
        UPDATE submissions
        SET status = $2,
            score = $3,
            time_ms = $4,
            memory_kb = $5,
            message = $6,
            updated_at = NOW()
        WHERE id = $1
        "#,
    )
    .bind(submission_id)
    .bind(&result.status)
    .bind(result.score)
    .bind(result.time_ms)
    .bind(result.memory_kb)
    .bind(&result.message)
    .execute(&mut *tx)
    .await?;

    tx.commit().await?;

    Ok(())
}

pub async fn list_pending_submission_ids(db: &PgPool, limit: i64) -> Result<Vec<i64>> {
    let ids = sqlx::query_scalar::<_, i64>(
        r#"
        SELECT id
        FROM submissions
        WHERE status = 'PENDING'
        ORDER BY id ASC
        LIMIT $1
        "#,
    )
    .bind(limit)
    .fetch_all(db)
    .await?;

    Ok(ids)
}

pub async fn try_claim_submission(db: &PgPool, submission_id: i64) -> Result<bool> {
    let claimed: Option<i64> = sqlx::query_scalar::<_, i64>(
        r#"
        UPDATE submissions
        SET status = 'RUNNING', updated_at = NOW()
        WHERE id = $1 AND status = 'PENDING'
        RETURNING id
        "#,
    )
    .bind(submission_id)
    .fetch_optional(db)
    .await?;

    Ok(claimed.is_some())
}
