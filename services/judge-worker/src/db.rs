use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use sqlx::{PgPool, Row};

use crate::result::ResultFile;

#[derive(Debug, Clone)]
pub struct Submission {
    pub id: i64,
    pub problem_id: i64,
    pub user_id: i64,
    pub language: String,
    pub status: String,
    pub code_path: String,
    pub result_path: String,
}

#[derive(Debug, Clone)]
pub struct Problem {
    pub id: i64,
    pub package_dir: String,
}

pub async fn load_submission(db: &PgPool, submission_id: i64) -> Result<Submission> {
    let row = sqlx::query(
        r#"
SELECT
    id,
    problem_id,
    user_id,
    language,
    status,
    code_path,
    result_path
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
        status: row.try_get("status")?,
        code_path: row.try_get("code_path")?,
        result_path: row.try_get("result_path")?,
    })
}

pub async fn load_problem(db: &PgPool, problem_id: i64) -> Result<Problem> {
    let row = sqlx::query(
        r#"
SELECT
    id,
    package_dir
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
        package_dir: row.try_get("package_dir")?,
    })
}

pub async fn try_claim_submission(db: &PgPool, submission_id: i64) -> Result<bool> {
    let claimed: Option<i64> = sqlx::query_scalar(
        r#"
UPDATE submissions
SET status = 'JUDGING',
    updated_at = NOW()
WHERE id = $1
  AND status = 'PENDING'
RETURNING id
"#,
    )
    .bind(submission_id)
    .fetch_optional(db)
    .await?;

    Ok(claimed.is_some())
}

pub async fn is_submission_cancelled(db: &PgPool, submission_id: i64) -> Result<bool> {
    let status: String = sqlx::query_scalar(
        r#"
SELECT status
FROM submissions
WHERE id = $1
"#,
    )
    .bind(submission_id)
    .fetch_one(db)
    .await?;

    Ok(status == "CANCELLED")
}

pub async fn mark_submission_failed(
    db: &PgPool,
    submission_id: i64,
    status: &str,
    message: &str,
) -> Result<()> {
    sqlx::query(
        r#"
UPDATE submissions
SET status = $2,
    score = 0,
    time_ms = 0,
    memory_kb = 0,
    message = $3,
    judged_at = NOW(),
    updated_at = NOW()
WHERE id = $1
  AND status <> 'CANCELLED'
"#,
    )
    .bind(submission_id)
    .bind(status)
    .bind(message)
    .execute(db)
    .await?;

    Ok(())
}

pub async fn save_judge_result(
    db: &PgPool,
    submission_id: i64,
    result_path: &str,
    result: &ResultFile,
) -> Result<()> {
    sqlx::query(
        r#"
UPDATE submissions
SET status = $2,
    score = $3,
    time_ms = $4,
    memory_kb = $5,
    message = $6,
    result_path = $7,
    judged_at = NOW(),
    updated_at = NOW()
WHERE id = $1
  AND status <> 'CANCELLED'
"#,
    )
    .bind(submission_id)
    .bind(&result.status)
    .bind(result.score)
    .bind(result.time_ms)
    .bind(result.memory_kb)
    .bind(&result.message)
    .bind(result_path)
    .execute(db)
    .await?;

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

#[allow(dead_code)]
pub fn now_utc() -> DateTime<Utc> {
    Utc::now()
}
