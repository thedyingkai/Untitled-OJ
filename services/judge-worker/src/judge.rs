use anyhow::{Context, Result};
use std::path::Path;
use std::sync::Arc;
use tokio::fs;

use crate::checker::{default_trim_equal, truncate_message};
use crate::config::LanguagesConfig;
use crate::problem_package::{CaseRecord, load_problem_package};
use crate::result::{ResultCase, ResultFile};
use crate::sandbox::{SandboxStatus, compile_in_sandbox, run_case_in_sandbox, write_text};

pub async fn judge_artifacts(
    languages: Arc<LanguagesConfig>,
    submission_id: i64,
    language: &str,
    source_path: &Path,
    package_dir: &Path,
    result_dir: &Path,
) -> Result<ResultFile> {
    fs::create_dir_all(result_dir).await?;

    let package = load_problem_package(&package_dir.to_string_lossy()).await?;

    let Some(lang) = languages.get(language) else {
        return Ok(ResultFile {
            submission_id,
            status: "UNSUPPORTED_LANGUAGE".to_string(),
            score: 0,
            time_ms: 0,
            memory_kb: 0,
            message: format!("unsupported language: {}", language),
            cases: vec![],
        });
    };

    if let Some(message) = compile_in_sandbox(lang, source_path, result_dir)
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
        write_local_result(result_dir, &result).await?;
        return Ok(result);
    }

    let mut final_status = "ACCEPTED".to_string();
    let mut final_message = String::new();
    let mut total_score = 0;
    let mut max_time_ms = 0;
    let mut max_memory_kb = 0;
    let mut case_results = Vec::new();

    for case in &package.cases {
        let case_result =
            run_one_artifact_case(lang, source_path, language, result_dir, &package, case).await?;

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

    write_local_result(result_dir, &result).await?;
    Ok(result)
}

async fn run_one_artifact_case(
    lang: &crate::config::LanguageConfig,
    source_path: &Path,
    language: &str,
    result_dir: &Path,
    package: &crate::problem_package::LoadedProblemPackage,
    case: &CaseRecord,
) -> Result<ResultCase> {
    let case_name = format!("{:03}", case.case_no);
    let case_dir = result_dir.join("cases").join(&case_name);

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

    let limit = package.limit_for(language, case);

    let sandbox_output = run_case_in_sandbox(
        lang,
        source_path,
        result_dir,
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
        SandboxStatus::OutputLimitExceeded => {
            status = "OUTPUT_LIMIT_EXCEEDED".to_string();
            score = 0;
            message = if sandbox_output.message.trim().is_empty() {
                "output limit exceeded".to_string()
            } else {
                truncate_message(&sandbox_output.message)
            };
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

async fn write_local_result(result_dir: &Path, result: &ResultFile) -> Result<()> {
    let text = serde_json::to_string_pretty(result)?;
    fs::write(result_dir.join("result.json"), text).await?;
    Ok(())
}

fn path_string(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}
