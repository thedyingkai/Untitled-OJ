use anyhow::{Context, Result, anyhow};
use sqlx::PgPool;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::fs;
use tracing::info;

use crate::checker::{default_trim_equal, truncate_message};
use crate::config::LanguagesConfig;
use crate::db::{
    is_submission_cancelled, load_problem, load_submission, mark_submission_failed,
    save_judge_result, try_claim_submission,
};
use crate::problem_package::{CaseRecord, load_problem_package};
use crate::result::{ResultCase, ResultFile};
use crate::sandbox::{SandboxStatus, compile_in_sandbox, run_case_in_sandbox, write_text};

pub async fn handle_submission(
    db: &PgPool,
    languages: Arc<LanguagesConfig>,
    submission_id: i64,
) -> Result<()> {
    let claimed = try_claim_submission(db, submission_id).await?;
    if !claimed {
        info!(
            submission_id,
            "submission skipped because it is not pending"
        );
        return Ok(());
    }

    info!(submission_id, "submission claimed");

    let result = judge_submission(db, languages, submission_id).await;

    match result {
        Ok(_) => {
            info!(submission_id, "judge finished");
            Ok(())
        }
        Err(err) => {
            mark_submission_failed(db, submission_id, "SYSTEM_ERROR", &err.to_string()).await?;
            Err(err)
        }
    }
}

async fn judge_submission(
    db: &PgPool,
    languages: Arc<LanguagesConfig>,
    submission_id: i64,
) -> Result<()> {
    let submission = load_submission(db, submission_id).await?;

    if submission.status == "CANCELLED" {
        return Ok(());
    }

    let problem = load_problem(db, submission.problem_id).await?;
    if problem.package_dir.trim().is_empty() {
        return Err(anyhow!("problem package_dir is empty"));
    }

    let package = load_problem_package(&problem.package_dir).await?;

    let Some(lang) = languages.get(&submission.language) else {
        let result = ResultFile {
            submission_id,
            status: "UNSUPPORTED_LANGUAGE".to_string(),
            score: 0,
            time_ms: 0,
            memory_kb: 0,
            message: format!("unsupported language: {}", submission.language),
            cases: vec![],
        };

        write_result_and_update(db, &submission.result_path, &result).await?;
        return Ok(());
    };

    let submission_dir = submission_root_from_result_path(&submission.result_path)?;

    if let Some(message) =
        compile_in_sandbox(lang, Path::new(&submission.code_path), &submission_dir)
            .await
            .context("compile in sandbox failed")?
    {
        let result = ResultFile {
            submission_id,
            status: "COMPILE_ERROR".to_string(),
            score: 0,
            time_ms: 0,
            memory_kb: 0,
            message,
            cases: vec![],
        };

        write_result_and_update(db, &submission.result_path, &result).await?;
        return Ok(());
    }

    let mut final_status = "ACCEPTED".to_string();
    let mut final_message = String::new();
    let mut total_score = 0;
    let mut max_time_ms = 0;
    let mut max_memory_kb = 0;
    let mut case_results = Vec::new();

    for case in &package.cases {
        if is_submission_cancelled(db, submission_id).await? {
            return Ok(());
        }

        let case_result = run_one_case(lang, &submission, &submission_dir, &package, case).await?;

        if case_result.status == "ACCEPTED" {
            total_score += case_result.score;
        } else if final_status == "ACCEPTED" {
            final_status = case_result.status.clone();
            final_message = case_result.message.clone();
        }

        max_time_ms = max_time_ms.max(case_result.time_ms);
        max_memory_kb = max_memory_kb.max(case_result.memory_kb);

        case_results.push(case_result);
    }

    let result = ResultFile {
        submission_id,
        status: final_status,
        score: total_score,
        time_ms: max_time_ms,
        memory_kb: max_memory_kb,
        message: final_message,
        cases: case_results,
    };

    write_result_and_update(db, &submission.result_path, &result).await?;

    Ok(())
}

async fn run_one_case(
    lang: &crate::config::LanguageConfig,
    submission: &crate::db::Submission,
    submission_dir: &Path,
    package: &crate::problem_package::LoadedProblemPackage,
    case: &CaseRecord,
) -> Result<ResultCase> {
    let case_name = format!("{:03}", case.case_no);
    let case_dir = submission_dir.join("cases").join(&case_name);

    fs::create_dir_all(&case_dir).await?;

    let stdin_path = case_dir.join("stdin.txt");
    let stdout_path = case_dir.join("stdout.txt");
    let stderr_path = case_dir.join("stderr.txt");
    let checker_log_path = case_dir.join("checker.log");

    let _ = fs::remove_file(&stdout_path).await;
    let _ = fs::remove_file(&stderr_path).await;
    let _ = fs::remove_file(&checker_log_path).await;

    let input_path = package.input_path(case)?;
    let answer_path = package.answer_path(case)?;

    let input = fs::read(&input_path)
        .await
        .with_context(|| format!("read input failed: {}", input_path.display()))?;

    fs::write(&stdin_path, input).await?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        let mut perms = fs::metadata(&stdin_path).await?.permissions();
        perms.set_mode(0o644);
        fs::set_permissions(&stdin_path, perms).await?;
    }

    let limit = package.limit_for(&submission.language, case);

    let sandbox_output = run_case_in_sandbox(
        lang,
        Path::new(&submission.code_path),
        submission_dir,
        &case_dir,
        &stdin_path,
        &stdout_path,
        &stderr_path,
        &limit,
    )
    .await
    .context("run case in sandbox failed")?;

    let status;
    let score;
    let message;

    match sandbox_output.status {
        SandboxStatus::Ok => {
            let actual = String::from_utf8_lossy(&sandbox_output.stdout).to_string();
            let expected = fs::read_to_string(&answer_path)
                .await
                .with_context(|| format!("read answer failed: {}", answer_path.display()))?;

            if default_trim_equal(&actual, &expected) {
                status = "ACCEPTED".to_string();
                score = case.score;
                message = String::new();
                write_text(&checker_log_path, "accepted\n").await?;
            } else {
                status = "WRONG_ANSWER".to_string();
                score = 0;
                message = "wrong answer".to_string();

                let log = format!(
                    "expected: {}\nactual: {}\n",
                    truncate_message(&expected),
                    truncate_message(&actual)
                );
                write_text(&checker_log_path, &log).await?;
            }
        }
        SandboxStatus::TimeLimitExceeded => {
            status = "TIME_LIMIT_EXCEEDED".to_string();
            score = 0;
            message = "time limit exceeded".to_string();
            write_text(&checker_log_path, &message).await?;
        }
        SandboxStatus::MemoryLimitExceeded => {
            status = "MEMORY_LIMIT_EXCEEDED".to_string();
            score = 0;
            message = "memory limit exceeded".to_string();
            write_text(&checker_log_path, &message).await?;
        }
        SandboxStatus::RuntimeError => {
            status = "RUNTIME_ERROR".to_string();
            score = 0;

            let user_stderr = String::from_utf8_lossy(&sandbox_output.stderr).to_string();

            if sandbox_output.message.trim().is_empty() {
                if user_stderr.trim().is_empty() {
                    message = "runtime error".to_string();
                } else {
                    message = truncate_message(&user_stderr);
                }
            } else {
                message = truncate_message(&sandbox_output.message);
            }

            write_text(&checker_log_path, &message).await?;
        }
        SandboxStatus::SystemError => {
            status = "SYSTEM_ERROR".to_string();
            score = 0;
            message = sandbox_output.message;
            write_text(&checker_log_path, &message).await?;
        }
    }

    Ok(ResultCase {
        case_no: case.case_no,
        status,
        score,
        time_ms: sandbox_output.time_ms,
        memory_kb: sandbox_output.memory_kb,
        stdout_path: path_string(&stdout_path),
        stderr_path: path_string(&stderr_path),
        checker_log_path: path_string(&checker_log_path),
        message,
    })
}

async fn write_result_and_update(
    db: &PgPool,
    result_path: &str,
    result: &ResultFile,
) -> Result<()> {
    let text = serde_json::to_string_pretty(result)?;
    fs::write(result_path, text).await?;

    save_judge_result(db, result.submission_id, result_path, result).await?;

    Ok(())
}

fn submission_root_from_result_path(result_path: &str) -> Result<PathBuf> {
    let path = PathBuf::from(result_path);

    path.parent()
        .map(|p| p.to_path_buf())
        .ok_or_else(|| anyhow!("invalid result_path: {}", result_path))
}

fn path_string(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}
