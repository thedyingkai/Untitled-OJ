use anyhow::{Context, Result};
use sqlx::PgPool;
use std::path::Path;
use std::process::Stdio;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tempfile::TempDir;
use tokio::fs;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;
use tokio::time::timeout;
use tracing::info;

use crate::config::{LanguageConfig, LanguagesConfig, render_arg};
use crate::db::{
    CaseResult, JudgeResult, Problem, Submission, TestCase, load_problem, load_submission,
    load_test_cases, mark_submission_failed, save_judge_result, try_claim_submission,
};

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
    info!(submission_id, "start judging");

    let submission = load_submission(db, submission_id).await?;
    let problem = load_problem(db, submission.problem_id).await?;
    let test_cases = load_test_cases(db, submission.problem_id).await?;

    if test_cases.is_empty() {
        mark_submission_failed(db, submission_id, "SYSTEM_ERROR", "no test cases").await?;
        return Ok(());
    }

    let result = judge_submission(&submission, &problem, &test_cases, &languages).await?;

    save_judge_result(db, submission_id, result).await?;

    info!(submission_id, "judge finished");

    Ok(())
}

async fn judge_submission(
    submission: &Submission,
    problem: &Problem,
    test_cases: &[TestCase],
    languages: &LanguagesConfig,
) -> Result<JudgeResult> {
    let Some(lang) = languages.get(&submission.language) else {
        return Ok(JudgeResult {
            status: "UNSUPPORTED_LANGUAGE".to_string(),
            score: 0,
            time_ms: 0,
            memory_kb: 0,
            message: format!("unsupported language: {}", submission.language),
            cases: vec![],
        });
    };

    let work_dir = TempDir::new().context("create temp dir failed")?;
    let work_path = work_dir.path().to_path_buf();

    let source_path = work_path.join(&lang.source_file);

    let exe_path = if lang.exe_file.is_empty() {
        work_path.join("unused-exe")
    } else {
        work_path.join(&lang.exe_file)
    };

    fs::write(&source_path, &submission.code)
        .await
        .context("write source failed")?;

    if lang.compile.enabled {
        let compile_error = compile(lang, &source_path, &exe_path, &work_path).await?;

        if let Some(message) = compile_error {
            return Ok(JudgeResult {
                status: "COMPILE_ERROR".to_string(),
                score: 0,
                time_ms: 0,
                memory_kb: 0,
                message,
                cases: vec![],
            });
        }
    }

    let mut case_results = Vec::new();
    let mut total_score = 0;
    let mut max_time_ms = 0;
    let mut final_status = "ACCEPTED".to_string();
    let mut final_message = String::new();

    for tc in test_cases {
        let case_result = run_case(lang, &source_path, &exe_path, &work_path, problem, tc).await?;

        if case_result.status == "ACCEPTED" {
            total_score += case_result.passed_score;
        } else if final_status == "ACCEPTED" {
            final_status = case_result.status.clone();
            final_message = case_result.message.clone();
        }

        max_time_ms = max_time_ms.max(case_result.time_ms);
        case_results.push(case_result);

        if final_status != "ACCEPTED" {
            break;
        }
    }

    Ok(JudgeResult {
        status: final_status,
        score: total_score,
        time_ms: max_time_ms,
        memory_kb: 0,
        message: final_message,
        cases: case_results,
    })
}

async fn compile(
    lang: &LanguageConfig,
    source_path: &Path,
    exe_path: &Path,
    work_path: &Path,
) -> Result<Option<String>> {
    let args: Vec<String> = lang
        .compile
        .args
        .iter()
        .map(|arg| render_arg(arg, source_path, exe_path, work_path))
        .collect();

    let mut cmd = Command::new(&lang.compile.command);
    cmd.args(args);
    cmd.current_dir(work_path);

    let compile_future = cmd.output();

    let output = if lang.compile.timeout_ms > 0 {
        match timeout(
            Duration::from_millis(lang.compile.timeout_ms),
            compile_future,
        )
        .await
        {
            Ok(result) => result.context("run compile command failed")?,
            Err(_) => {
                return Ok(Some("compile timeout".to_string()));
            }
        }
    } else {
        compile_future.await.context("run compile command failed")?
    };

    if output.status.success() {
        Ok(None)
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        Ok(Some(truncate_message(&stderr)))
    }
}

async fn run_case(
    lang: &LanguageConfig,
    source_path: &Path,
    exe_path: &Path,
    work_path: &Path,
    problem: &Problem,
    tc: &TestCase,
) -> Result<CaseResult> {
    let run_command = render_arg(&lang.run.command, source_path, exe_path, work_path);

    let run_args: Vec<String> = lang
        .run
        .args
        .iter()
        .map(|arg| render_arg(arg, source_path, exe_path, work_path))
        .collect();

    let mut child = Command::new(run_command)
        .args(run_args)
        .current_dir(work_path)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .context("spawn user program failed")?;

    if let Some(mut stdin) = child.stdin.take() {
        stdin
            .write_all(tc.input.as_bytes())
            .await
            .context("write stdin failed")?;
    }

    let start = Instant::now();
    let limit = Duration::from_millis(problem.time_limit_ms.max(1) as u64);

    let output = match timeout(limit, child.wait_with_output()).await {
        Ok(result) => result.context("wait user program failed")?,
        Err(_) => {
            return Ok(CaseResult {
                test_case_id: tc.id,
                status: "TIME_LIMIT_EXCEEDED".to_string(),
                time_ms: problem.time_limit_ms,
                memory_kb: 0,
                message: "time limit exceeded".to_string(),
                passed_score: 0,
            });
        }
    };

    let elapsed_ms = start.elapsed().as_millis() as i32;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();

        return Ok(CaseResult {
            test_case_id: tc.id,
            status: "RUNTIME_ERROR".to_string(),
            time_ms: elapsed_ms,
            memory_kb: 0,
            message: truncate_message(&stderr),
            passed_score: 0,
        });
    }

    let actual = String::from_utf8_lossy(&output.stdout).to_string();

    if normalize_output(&actual) == normalize_output(&tc.output) {
        Ok(CaseResult {
            test_case_id: tc.id,
            status: "ACCEPTED".to_string(),
            time_ms: elapsed_ms,
            memory_kb: 0,
            message: String::new(),
            passed_score: tc.score,
        })
    } else {
        Ok(CaseResult {
            test_case_id: tc.id,
            status: "WRONG_ANSWER".to_string(),
            time_ms: elapsed_ms,
            memory_kb: 0,
            message: format!(
                "expected `{}`, got `{}`",
                truncate_message(&tc.output),
                truncate_message(&actual)
            ),
            passed_score: 0,
        })
    }
}

fn normalize_output(s: &str) -> String {
    s.replace("\r\n", "\n").trim_end().to_string()
}

fn truncate_message(s: &str) -> String {
    const LIMIT: usize = 512;

    let s = s.trim();

    if s.len() <= LIMIT {
        s.to_string()
    } else {
        format!("{}...", &s[..LIMIT])
    }
}
