use anyhow::{Context, Result, anyhow};
use serde::Deserialize;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::fs;

use crate::checker::{default_trim_equal, truncate_message};
use crate::config::{LanguageConfig, LanguagesConfig};
use crate::problem_package::{
    CaseRecord, ComponentConfig, LimitConfig, LoadedProblemPackage, load_problem_package,
};
use crate::result::{ResultCase, ResultFile};
use crate::sandbox::{
    SandboxOutput, SandboxStatus, compile_in_sandbox, run_case_in_sandbox,
    run_language_program_in_sandbox, write_text,
};

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

    let components = prepare_components(languages.clone(), &package, result_dir).await?;

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
    let mut max_time_ms = 0;
    let mut max_memory_kb = 0;
    let mut case_results = Vec::new();

    for case in &package.cases {
        let case_result = run_one_artifact_case(
            &languages,
            lang,
            source_path,
            language,
            result_dir,
            &package,
            &components,
            case,
        )
        .await?;

        if case_result.status != "ACCEPTED" && final_status == "ACCEPTED" {
            final_status = case_result.status.clone();
            final_message = case_result.message.clone();
        }

        max_time_ms = max_time_ms.max(case_result.time_ms);
        max_memory_kb = max_memory_kb.max(case_result.memory_kb);

        case_results.push(case_result);
    }

    let total_score =
        score_submission(&languages, result_dir, &package, &components, &case_results).await?;

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

#[derive(Debug, Clone)]
struct PreparedComponents {
    runner: Option<CustomComponentRuntime>,
    checker: Option<CustomComponentRuntime>,
    validator: Option<CustomComponentRuntime>,
    scorer: Option<CustomComponentRuntime>,
}

#[derive(Debug, Clone)]
struct CustomComponentRuntime {
    kind: String,
    config: ComponentConfig,
    language: String,
    source_path: PathBuf,
    program_dir: PathBuf,
}

async fn prepare_components(
    languages: Arc<LanguagesConfig>,
    package: &LoadedProblemPackage,
    result_dir: &Path,
) -> Result<PreparedComponents> {
    Ok(PreparedComponents {
        runner: prepare_component(
            languages.clone(),
            package,
            "runner",
            &package.runner,
            result_dir,
        )
        .await?,
        checker: prepare_component(
            languages.clone(),
            package,
            "checker",
            &package.checker,
            result_dir,
        )
        .await?,
        validator: prepare_component(
            languages.clone(),
            package,
            "validator",
            &package.validator,
            result_dir,
        )
        .await?,
        scorer: prepare_component(languages, package, "scorer", &package.scorer, result_dir)
            .await?,
    })
}

async fn prepare_component(
    languages: Arc<LanguagesConfig>,
    package: &LoadedProblemPackage,
    kind: &str,
    config: &ComponentConfig,
    result_dir: &Path,
) -> Result<Option<CustomComponentRuntime>> {
    if !config.is_custom() {
        return Ok(None);
    }

    let language = config
        .language()
        .ok_or_else(|| anyhow!("custom {} component has no language", kind))?;
    let Some(lang) = languages.get(&language) else {
        return Err(anyhow!(
            "custom {} component language is not configured: {}",
            kind,
            language
        ));
    };

    let source_path = package.component_source_path(config)?;
    let program_dir = result_dir.join("components").join(kind);
    fs::create_dir_all(&program_dir).await?;

    if let Some(message) = compile_in_sandbox(lang, &source_path, &program_dir)
        .await
        .with_context(|| format!("compile {} component failed", kind))?
    {
        return Err(anyhow!("compile {} component failed: {}", kind, message));
    }

    Ok(Some(CustomComponentRuntime {
        kind: kind.to_string(),
        config: config.clone(),
        language,
        source_path,
        program_dir,
    }))
}

#[allow(clippy::too_many_arguments)]
async fn run_one_artifact_case(
    languages: &LanguagesConfig,
    lang: &LanguageConfig,
    source_path: &Path,
    language: &str,
    result_dir: &Path,
    package: &LoadedProblemPackage,
    components: &PreparedComponents,
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
    make_world_readable(&stdin_path).await?;

    let limit = package.limit_for(language, case);

    if let Some(validator) = &components.validator
        && let Some(result) =
            run_custom_validator(languages, validator, &case_dir, &input_path, &limit, case).await?
    {
        write_text(&checker_log_path, &result.message).await?;
        return Ok(result.with_paths(stdout_path, stderr_path, checker_log_path));
    }

    let run_output = if let Some(runner) = &components.runner {
        run_custom_runner(
            languages,
            runner,
            source_path,
            language,
            lang,
            result_dir,
            &case_dir,
            &input_path,
            &answer_path,
            &stdout_path,
            &stderr_path,
            &limit,
        )
        .await?
    } else {
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
        RunnerOutcome {
            sandbox_output,
            report: ComponentReport::default(),
        }
    };

    let case_verdict = verdict_after_run(
        &run_output.sandbox_output,
        &run_output.report,
        &stdout_path,
        &answer_path,
        &checker_log_path,
        package,
        components,
        languages,
        &case_dir,
        &input_path,
        &limit,
        case,
    )
    .await?;

    Ok(case_verdict.with_paths(stdout_path, stderr_path, checker_log_path))
}

#[derive(Debug, Clone, Default)]
struct ComponentReport {
    status: Option<String>,
    score: Option<i32>,
    message: Option<String>,
    time_ms: Option<i32>,
    memory_kb: Option<i32>,
    accepted: Option<bool>,
}

#[derive(Debug, Deserialize)]
struct RawComponentReport {
    status: Option<String>,
    verdict: Option<String>,
    score: Option<i32>,
    message: Option<String>,
    time_ms: Option<i32>,
    memory_kb: Option<i32>,
    accepted: Option<bool>,
}

impl ComponentReport {
    fn status_upper(&self) -> Option<String> {
        self.status
            .as_ref()
            .map(|status| normalize_status(status))
            .filter(|status| !status.is_empty())
    }

    fn accepted(&self) -> Option<bool> {
        if let Some(accepted) = self.accepted {
            return Some(accepted);
        }
        self.status_upper()
            .map(|status| status == "ACCEPTED" || status == "OK")
    }

    fn message(&self) -> String {
        self.message.clone().unwrap_or_default()
    }
}

impl From<RawComponentReport> for ComponentReport {
    fn from(raw: RawComponentReport) -> Self {
        Self {
            status: raw.status.or(raw.verdict),
            score: raw.score,
            message: raw.message,
            time_ms: raw.time_ms,
            memory_kb: raw.memory_kb,
            accepted: raw.accepted,
        }
    }
}

#[derive(Debug)]
struct RunnerOutcome {
    sandbox_output: SandboxOutput,
    report: ComponentReport,
}

#[derive(Debug)]
struct CaseVerdict {
    case_no: i32,
    status: String,
    score: i32,
    time_ms: i32,
    memory_kb: i32,
    message: String,
}

impl CaseVerdict {
    fn with_paths(
        self,
        stdout_path: PathBuf,
        stderr_path: PathBuf,
        checker_log_path: PathBuf,
    ) -> ResultCase {
        ResultCase {
            case_no: self.case_no,
            status: self.status,
            score: self.score.clamp(0, 100),
            time_ms: self.time_ms,
            memory_kb: self.memory_kb,
            stdout_path: path_string(&stdout_path),
            stderr_path: path_string(&stderr_path),
            checker_log_path: path_string(&checker_log_path),
            message: self.message,
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn verdict_after_run(
    sandbox_output: &SandboxOutput,
    runner_report: &ComponentReport,
    stdout_path: &Path,
    answer_path: &Path,
    checker_log_path: &Path,
    package: &LoadedProblemPackage,
    components: &PreparedComponents,
    languages: &LanguagesConfig,
    case_dir: &Path,
    input_path: &Path,
    limit: &LimitConfig,
    case: &CaseRecord,
) -> Result<CaseVerdict> {
    let mut verdict = verdict_from_sandbox(case, sandbox_output, runner_report);
    if verdict.status != "ACCEPTED" {
        write_text(checker_log_path, &verdict.message).await?;
        return Ok(verdict);
    }

    if let Some(checker) = &components.checker {
        verdict = run_custom_checker(
            languages,
            checker,
            case_dir,
            input_path,
            answer_path,
            stdout_path,
            checker_log_path,
            limit,
            case,
            sandbox_output,
        )
        .await?;
        return Ok(verdict);
    }

    if builtin_checker_is_runner_authoritative(&package.checker)
        && let Some(accepted) = runner_report.accepted()
    {
        if accepted {
            let score = runner_report.score.unwrap_or(case.score);
            write_text(checker_log_path, "accepted by runner\n").await?;
            verdict.score = score;
            verdict.message = runner_report.message();
            verdict.status = "ACCEPTED".to_string();
        } else {
            let message = first_non_empty(&[runner_report.message(), "wrong answer".to_string()]);
            write_text(checker_log_path, &message).await?;
            verdict.status = runner_report
                .status_upper()
                .filter(|status| status != "OK" && status != "ACCEPTED")
                .unwrap_or_else(|| "WRONG_ANSWER".to_string());
            verdict.score = runner_report.score.unwrap_or(0);
            verdict.message = message;
        }
        return Ok(verdict);
    }

    if package.checker.is_builtin("default-trim-checker")
        || package.checker.is_builtin("output-only-checker")
    {
        return run_default_trim_checker(
            stdout_path,
            answer_path,
            checker_log_path,
            case,
            sandbox_output,
            runner_report,
        )
        .await;
    }

    if package.checker.is_builtin("heuristic-checker") {
        write_text(checker_log_path, "accepted; score delegated to scorer\n").await?;
        verdict.score = runner_report.score.unwrap_or(case.score);
        return Ok(verdict);
    }

    run_default_trim_checker(
        stdout_path,
        answer_path,
        checker_log_path,
        case,
        sandbox_output,
        runner_report,
    )
    .await
}

fn verdict_from_sandbox(
    case: &CaseRecord,
    sandbox_output: &SandboxOutput,
    report: &ComponentReport,
) -> CaseVerdict {
    let status = report.status_upper();
    let message = report.message();
    let score = report.score.unwrap_or(case.score);
    let time_ms = report.time_ms.unwrap_or(sandbox_output.time_ms);
    let memory_kb = report.memory_kb.unwrap_or(sandbox_output.memory_kb);

    if let Some(false) = report.accepted() {
        return CaseVerdict {
            case_no: case.case_no,
            status: status
                .filter(|status| status != "OK" && status != "ACCEPTED")
                .unwrap_or_else(|| "WRONG_ANSWER".to_string()),
            score: report.score.unwrap_or(0),
            time_ms,
            memory_kb,
            message: first_non_empty(&[message, "wrong answer".to_string()]),
        };
    }

    match sandbox_output.status {
        SandboxStatus::Ok => CaseVerdict {
            case_no: case.case_no,
            status: status
                .filter(|status| status != "OK")
                .unwrap_or_else(|| "ACCEPTED".to_string()),
            score,
            time_ms,
            memory_kb,
            message,
        },
        SandboxStatus::TimeLimitExceeded => CaseVerdict {
            case_no: case.case_no,
            status: "TIME_LIMIT_EXCEEDED".to_string(),
            score: 0,
            time_ms: sandbox_output.time_ms,
            memory_kb: sandbox_output.memory_kb,
            message: "time limit exceeded".to_string(),
        },
        SandboxStatus::MemoryLimitExceeded => CaseVerdict {
            case_no: case.case_no,
            status: "MEMORY_LIMIT_EXCEEDED".to_string(),
            score: 0,
            time_ms: sandbox_output.time_ms,
            memory_kb: sandbox_output.memory_kb,
            message: "memory limit exceeded".to_string(),
        },
        SandboxStatus::OutputLimitExceeded => CaseVerdict {
            case_no: case.case_no,
            status: "OUTPUT_LIMIT_EXCEEDED".to_string(),
            score: 0,
            time_ms: sandbox_output.time_ms,
            memory_kb: sandbox_output.memory_kb,
            message: if sandbox_output.message.trim().is_empty() {
                "output limit exceeded".to_string()
            } else {
                truncate_message(&sandbox_output.message)
            },
        },
        SandboxStatus::RuntimeError => {
            let user_stderr = String::from_utf8_lossy(&sandbox_output.stderr).to_string();
            let message = if sandbox_output.message.trim().is_empty() {
                if user_stderr.trim().is_empty() {
                    "runtime error".to_string()
                } else {
                    truncate_message(&user_stderr)
                }
            } else {
                truncate_message(&sandbox_output.message)
            };
            CaseVerdict {
                case_no: case.case_no,
                status: "RUNTIME_ERROR".to_string(),
                score: 0,
                time_ms: sandbox_output.time_ms,
                memory_kb: sandbox_output.memory_kb,
                message,
            }
        }
    }
}

async fn run_default_trim_checker(
    stdout_path: &Path,
    answer_path: &Path,
    checker_log_path: &Path,
    case: &CaseRecord,
    sandbox_output: &SandboxOutput,
    runner_report: &ComponentReport,
) -> Result<CaseVerdict> {
    let actual = fs::read_to_string(stdout_path).await.unwrap_or_default();
    let expected = fs::read_to_string(answer_path)
        .await
        .with_context(|| format!("read answer failed: {}", answer_path.display()))?;

    if default_trim_equal(&actual, &expected) {
        write_text(checker_log_path, "accepted\n").await?;
        Ok(CaseVerdict {
            case_no: case.case_no,
            status: "ACCEPTED".to_string(),
            score: runner_report.score.unwrap_or(case.score),
            time_ms: runner_report.time_ms.unwrap_or(sandbox_output.time_ms),
            memory_kb: runner_report.memory_kb.unwrap_or(sandbox_output.memory_kb),
            message: runner_report.message(),
        })
    } else {
        let log = format!(
            "expected: {}\nactual: {}\n",
            truncate_message(&expected),
            truncate_message(&actual)
        );
        write_text(checker_log_path, &log).await?;
        Ok(CaseVerdict {
            case_no: case.case_no,
            status: "WRONG_ANSWER".to_string(),
            score: 0,
            time_ms: runner_report.time_ms.unwrap_or(sandbox_output.time_ms),
            memory_kb: runner_report.memory_kb.unwrap_or(sandbox_output.memory_kb),
            message: "wrong answer".to_string(),
        })
    }
}

async fn run_custom_validator(
    languages: &LanguagesConfig,
    validator: &CustomComponentRuntime,
    case_dir: &Path,
    input_path: &Path,
    limit: &LimitConfig,
    case: &CaseRecord,
) -> Result<Option<CaseVerdict>> {
    let work_dir = case_dir.join("validator");
    fs::create_dir_all(&work_dir).await?;
    fs::copy(input_path, work_dir.join("input.txt")).await?;

    let stdout = work_dir.join("validator.stdout.log");
    let stderr = work_dir.join("validator.stderr.log");
    let args = component_args(validator, &["input.txt"]);
    let output = run_custom_component(
        languages, validator, &work_dir, &stdout, &stderr, limit, args,
    )
    .await?;

    if output.status == SandboxStatus::Ok {
        return Ok(None);
    }

    let log = merged_component_log("validator", &output, &stdout, &stderr).await;
    Ok(Some(CaseVerdict {
        case_no: case.case_no,
        status: "SYSTEM_ERROR".to_string(),
        score: 0,
        time_ms: output.time_ms,
        memory_kb: output.memory_kb,
        message: truncate_message(&log),
    }))
}

#[allow(clippy::too_many_arguments)]
async fn run_custom_runner(
    languages: &LanguagesConfig,
    runner: &CustomComponentRuntime,
    source_path: &Path,
    language: &str,
    submission_lang: &LanguageConfig,
    submission_dir: &Path,
    case_dir: &Path,
    input_path: &Path,
    answer_path: &Path,
    stdout_path: &Path,
    stderr_path: &Path,
    limit: &LimitConfig,
) -> Result<RunnerOutcome> {
    let work_dir = case_dir.join("runner");
    fs::create_dir_all(&work_dir).await?;
    fs::copy(input_path, work_dir.join("input.txt")).await?;
    fs::copy(answer_path, work_dir.join("answer.txt")).await?;

    let submission_path =
        stage_submission_for_component(submission_lang, source_path, submission_dir, &work_dir)
            .await?;
    let component_stdout = work_dir.join("runner.stdout.log");
    let component_stderr = work_dir.join("runner.stderr.log");
    let contestant_stdout = work_dir.join("stdout.txt");
    let contestant_stderr = work_dir.join("stderr.txt");
    let report_path = work_dir.join("runner_result.json");

    let protocol_args: Vec<&str> = vec![
        "input.txt",
        "answer.txt",
        submission_path.as_str(),
        "stdout.txt",
        "stderr.txt",
        "runner_result.json",
        language,
    ];
    let args = component_args(runner, &protocol_args);
    let output = run_custom_component(
        languages,
        runner,
        &work_dir,
        &component_stdout,
        &component_stderr,
        limit,
        args,
    )
    .await?;

    ensure_file(&contestant_stdout).await?;
    ensure_file(&contestant_stderr).await?;
    fs::copy(&contestant_stdout, stdout_path).await?;
    fs::copy(&contestant_stderr, stderr_path).await?;

    let report = parse_component_report_from_paths(&report_path, &component_stdout).await?;

    Ok(RunnerOutcome {
        sandbox_output: output,
        report,
    })
}

#[allow(clippy::too_many_arguments)]
async fn run_custom_checker(
    languages: &LanguagesConfig,
    checker: &CustomComponentRuntime,
    case_dir: &Path,
    input_path: &Path,
    answer_path: &Path,
    stdout_path: &Path,
    checker_log_path: &Path,
    limit: &LimitConfig,
    case: &CaseRecord,
    sandbox_output: &SandboxOutput,
) -> Result<CaseVerdict> {
    let work_dir = case_dir.join("checker");
    fs::create_dir_all(&work_dir).await?;
    fs::copy(input_path, work_dir.join("input.txt")).await?;
    fs::copy(answer_path, work_dir.join("answer.txt")).await?;
    fs::copy(stdout_path, work_dir.join("output.txt")).await?;

    let component_stdout = work_dir.join("checker.stdout.log");
    let component_stderr = work_dir.join("checker.stderr.log");
    let report_path = work_dir.join("checker_result.json");
    let args = component_args(
        checker,
        &[
            "input.txt",
            "answer.txt",
            "output.txt",
            "checker_result.json",
        ],
    );
    let output = run_custom_component(
        languages,
        checker,
        &work_dir,
        &component_stdout,
        &component_stderr,
        limit,
        args,
    )
    .await?;

    let report = parse_component_report_from_paths(&report_path, &component_stdout).await?;
    let log = merged_component_log("checker", &output, &component_stdout, &component_stderr).await;
    write_text(checker_log_path, &log).await?;

    let accepted = match output.status {
        SandboxStatus::Ok => report.accepted().unwrap_or(true),
        _ => report.accepted().unwrap_or(false),
    };

    if accepted {
        Ok(CaseVerdict {
            case_no: case.case_no,
            status: report
                .status_upper()
                .filter(|status| status != "OK")
                .unwrap_or_else(|| "ACCEPTED".to_string()),
            score: report.score.unwrap_or(case.score),
            time_ms: report.time_ms.unwrap_or(sandbox_output.time_ms),
            memory_kb: report.memory_kb.unwrap_or(sandbox_output.memory_kb),
            message: report.message(),
        })
    } else {
        Ok(CaseVerdict {
            case_no: case.case_no,
            status: report
                .status_upper()
                .filter(|status| status != "OK" && status != "ACCEPTED")
                .unwrap_or_else(|| "WRONG_ANSWER".to_string()),
            score: report.score.unwrap_or(0),
            time_ms: report.time_ms.unwrap_or(sandbox_output.time_ms),
            memory_kb: report.memory_kb.unwrap_or(sandbox_output.memory_kb),
            message: first_non_empty(&[report.message(), "wrong answer".to_string()]),
        })
    }
}

async fn score_submission(
    languages: &LanguagesConfig,
    result_dir: &Path,
    package: &LoadedProblemPackage,
    components: &PreparedComponents,
    cases: &[ResultCase],
) -> Result<i32> {
    if let Some(scorer) = &components.scorer {
        let score = run_custom_scorer(languages, scorer, result_dir, package, cases).await?;
        return Ok(score.clamp(0, 100));
    }

    Ok(cases
        .iter()
        .map(|case| case.score.max(0))
        .sum::<i32>()
        .min(100))
}

async fn run_custom_scorer(
    languages: &LanguagesConfig,
    scorer: &CustomComponentRuntime,
    result_dir: &Path,
    package: &LoadedProblemPackage,
    cases: &[ResultCase],
) -> Result<i32> {
    let work_dir = result_dir.join("scorer");
    fs::create_dir_all(&work_dir).await?;
    let cases_path = work_dir.join("case_results.json");
    let score_path = work_dir.join("score.json");
    let payload = serde_json::json!({
        "problem_type": &package.manifest.problem_type,
        "max_score": 100,
        "cases": cases,
    });
    fs::write(&cases_path, serde_json::to_vec_pretty(&payload)?).await?;

    let stdout = work_dir.join("scorer.stdout.log");
    let stderr = work_dir.join("scorer.stderr.log");
    let limit = LimitConfig {
        time_ms: package.manifest.limits.default.time_ms.max(1000),
        memory_mb: package.manifest.limits.default.memory_mb.max(64),
    };
    let args = component_args(scorer, &["case_results.json", "score.json"]);
    let output =
        run_custom_component(languages, scorer, &work_dir, &stdout, &stderr, &limit, args).await?;

    if output.status != SandboxStatus::Ok {
        let log = merged_component_log("scorer", &output, &stdout, &stderr).await;
        return Err(anyhow!("custom scorer failed: {}", truncate_message(&log)));
    }

    let report = parse_component_report_from_paths(&score_path, &stdout).await?;
    if let Some(score) = report.score {
        return Ok(score);
    }

    let stdout_text = fs::read_to_string(&stdout).await.unwrap_or_default();
    parse_score_text(&stdout_text).ok_or_else(|| anyhow!("custom scorer did not produce a score"))
}

async fn run_custom_component(
    languages: &LanguagesConfig,
    component: &CustomComponentRuntime,
    work_dir: &Path,
    stdout: &Path,
    stderr: &Path,
    limit: &LimitConfig,
    args: Vec<String>,
) -> Result<SandboxOutput> {
    let lang = languages
        .get(&component.language)
        .ok_or_else(|| anyhow!("component language missing: {}", component.language))?;
    run_language_program_in_sandbox(
        lang,
        &component.source_path,
        &component.program_dir,
        work_dir,
        None,
        stdout,
        stderr,
        limit,
        &args,
    )
    .await
    .with_context(|| format!("run {} component failed", component.kind))
}

fn component_args(component: &CustomComponentRuntime, protocol_args: &[&str]) -> Vec<String> {
    let mut args = component.config.args();
    args.extend(protocol_args.iter().map(|arg| (*arg).to_string()));
    args
}

async fn stage_submission_for_component(
    lang: &LanguageConfig,
    source_path: &Path,
    submission_dir: &Path,
    work_dir: &Path,
) -> Result<String> {
    if lang.exe_file.is_empty() {
        let source_name = source_path
            .file_name()
            .ok_or_else(|| anyhow!("source path has no file name"))?;
        fs::copy(source_path, work_dir.join(source_name)).await?;
        return Ok(source_name.to_string_lossy().to_string());
    }

    let build_exe = submission_dir.join("build").join(&lang.exe_file);
    let staged = work_dir.join(&lang.exe_file);
    fs::copy(&build_exe, &staged)
        .await
        .with_context(|| format!("copy submission executable failed: {}", build_exe.display()))?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        let mut perms = fs::metadata(&staged).await?.permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&staged, perms).await?;
    }

    Ok(lang.exe_file.clone())
}

async fn parse_component_report_from_paths(
    report_path: &Path,
    stdout_path: &Path,
) -> Result<ComponentReport> {
    if let Ok(text) = fs::read_to_string(report_path).await
        && let Some(report) = parse_component_report(&text)
    {
        return Ok(report);
    }
    if let Ok(text) = fs::read_to_string(stdout_path).await
        && let Some(report) = parse_component_report(&text)
    {
        return Ok(report);
    }
    Ok(ComponentReport::default())
}

fn parse_component_report(text: &str) -> Option<ComponentReport> {
    let text = text.trim();
    if text.is_empty() {
        return None;
    }
    if let Ok(raw) = serde_json::from_str::<RawComponentReport>(text) {
        return Some(raw.into());
    }
    if let Some(score) = parse_score_text(text) {
        return Some(ComponentReport {
            score: Some(score),
            ..ComponentReport::default()
        });
    }
    None
}

fn parse_score_text(text: &str) -> Option<i32> {
    text.trim().lines().next()?.trim().parse::<i32>().ok()
}

async fn merged_component_log(
    name: &str,
    output: &SandboxOutput,
    stdout_path: &Path,
    stderr_path: &Path,
) -> String {
    let stdout = fs::read_to_string(stdout_path).await.unwrap_or_default();
    let stderr = fs::read_to_string(stderr_path).await.unwrap_or_default();
    format!(
        "{}_status: {:?}\n{}_message: {}\n\n[stdout]\n{}\n\n[stderr]\n{}\n",
        name, output.status, name, output.message, stdout, stderr
    )
}

fn builtin_checker_is_runner_authoritative(checker: &ComponentConfig) -> bool {
    checker.is_builtin("interactive-checker")
        || checker.is_builtin("communication-checker")
        || checker.is_builtin("heuristic-checker")
}

fn normalize_status(status: &str) -> String {
    status.trim().replace(['-', ' '], "_").to_ascii_uppercase()
}

fn first_non_empty(values: &[String]) -> String {
    values
        .iter()
        .find(|value| !value.trim().is_empty())
        .cloned()
        .unwrap_or_default()
}

async fn ensure_file(path: &Path) -> Result<()> {
    if fs::metadata(path).await.is_err() {
        fs::write(path, "").await?;
    }
    Ok(())
}

async fn make_world_readable(_path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        let mut perms = fs::metadata(_path).await?.permissions();
        perms.set_mode(0o644);
        fs::set_permissions(_path, perms).await?;
    }
    Ok(())
}

async fn write_local_result(result_dir: &Path, result: &ResultFile) -> Result<()> {
    let text = serde_json::to_string_pretty(result)?;
    fs::write(result_dir.join("result.json"), text).await?;
    Ok(())
}

fn path_string(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::LanguagesConfig;
    use crate::sandbox::nsjail_available;
    use std::process::{Command as StdCommand, Stdio};
    use std::sync::Arc;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[derive(Debug, Clone, Copy)]
    enum MatrixVerdict {
        Accepted,
        WrongAnswer,
        CompileError,
        RuntimeError,
        TimeLimitExceeded,
    }

    #[derive(Debug, Clone, Copy)]
    struct LanguageCase {
        id: &'static str,
        source_file: &'static str,
        compile_tools: &'static [&'static str],
        run_tools: &'static [&'static str],
        accepted: &'static str,
        wrong_answer: &'static str,
        compile_error: &'static str,
        runtime_error: &'static str,
        time_limit: &'static str,
    }

    const LANGUAGE_CASES: &[LanguageCase] = &[
        LanguageCase {
            id: "cpp17",
            source_file: "main.cpp",
            compile_tools: &["g++"],
            run_tools: &[],
            accepted: r#"
#include <iostream>
int main() {
    long long a, b;
    if (std::cin >> a >> b) {
        std::cout << (a + b) << '\n';
    }
    return 0;
}
"#,
            wrong_answer: r#"
#include <iostream>
int main() {
    std::cout << 0 << '\n';
    return 0;
}
"#,
            compile_error: "int main( { return 0; }\n",
            runtime_error: r#"
#include <cstdlib>
int main() {
    std::abort();
}
"#,
            time_limit: r#"
int main() {
    while (true) {}
}
"#,
        },
        LanguageCase {
            id: "c11",
            source_file: "main.c",
            compile_tools: &["gcc"],
            run_tools: &[],
            accepted: r#"
#include <stdio.h>
int main(void) {
    long long a, b;
    if (scanf("%lld %lld", &a, &b) == 2) {
        printf("%lld\n", a + b);
    }
    return 0;
}
"#,
            wrong_answer: r#"
#include <stdio.h>
int main(void) {
    printf("0\n");
    return 0;
}
"#,
            compile_error: "int main( { return 0; }\n",
            runtime_error: r#"
#include <stdlib.h>
int main(void) {
    abort();
}
"#,
            time_limit: r#"
int main(void) {
    for (;;) {}
}
"#,
        },
        LanguageCase {
            id: "java17",
            source_file: "Main.java",
            compile_tools: &["javac"],
            run_tools: &["java"],
            accepted: r#"
import java.util.*;
public class Main {
    public static void main(String[] args) {
        Scanner sc = new Scanner(System.in);
        long a = sc.nextLong();
        long b = sc.nextLong();
        System.out.println(a + b);
    }
}
"#,
            wrong_answer: r#"
public class Main {
    public static void main(String[] args) {
        System.out.println(0);
    }
}
"#,
            compile_error: "public class Main { public static void main(String[] args) { }\n",
            runtime_error: r#"
public class Main {
    public static void main(String[] args) {
        throw new RuntimeException("boom");
    }
}
"#,
            time_limit: r#"
public class Main {
    public static void main(String[] args) {
        while (true) {}
    }
}
"#,
        },
        LanguageCase {
            id: "python3",
            source_file: "main.py",
            compile_tools: &["python3"],
            run_tools: &["python3"],
            accepted: r#"
import sys
a, b = map(int, sys.stdin.read().split())
print(a + b)
"#,
            wrong_answer: "print(0)\n",
            compile_error: "def main(:\n    pass\n",
            runtime_error: "raise RuntimeError('boom')\n",
            time_limit: "while True:\n    pass\n",
        },
    ];

    #[tokio::test]
    async fn nsjail_c_cpp_java_python_verdict_matrix_when_available() {
        if !live_verdict_matrix_available() {
            return;
        }

        let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
        let languages = Arc::new(
            LanguagesConfig::load(&manifest_dir.join("config/languages.yaml").to_string_lossy())
                .await
                .expect("load languages config"),
        );

        for language in LANGUAGE_CASES {
            for verdict in [
                MatrixVerdict::Accepted,
                MatrixVerdict::WrongAnswer,
                MatrixVerdict::CompileError,
                MatrixVerdict::RuntimeError,
                MatrixVerdict::TimeLimitExceeded,
            ] {
                let root = create_verdict_matrix_workspace(language, verdict);
                let source = root.join("source").join(language.source_file);
                let package = root.join("problem");
                let result = root.join("result");

                let judged = judge_artifacts(
                    languages.clone(),
                    1000 + matrix_verdict_index(verdict),
                    language.id,
                    &source,
                    &package,
                    &result,
                )
                .await
                .unwrap_or_else(|err| {
                    panic!("{} {:?} matrix run failed: {err:#}", language.id, verdict)
                });

                assert_eq!(
                    judged.status,
                    expected_status(verdict),
                    "{} {:?} should produce expected final status: {}",
                    language.id,
                    verdict,
                    judged.message
                );
                assert_eq!(
                    judged.score,
                    expected_score(verdict),
                    "{} {:?} should produce expected score",
                    language.id,
                    verdict
                );
                assert!(
                    result.join("result.json").exists(),
                    "{} {:?} should write result.json",
                    language.id,
                    verdict
                );

                let _ = std::fs::remove_dir_all(&root);
            }
        }
    }

    fn live_verdict_matrix_available() -> bool {
        if !cfg!(target_os = "linux") {
            return skip_or_fail("judge verdict matrix requires Linux");
        }
        if !nsjail_available() {
            return skip_or_fail("judge verdict matrix requires nsjail on PATH");
        }
        for language in LANGUAGE_CASES {
            for tool in language
                .compile_tools
                .iter()
                .chain(language.run_tools.iter())
            {
                if !command_available(tool) {
                    return skip_or_fail(&format!(
                        "judge verdict matrix requires tool {tool} for {}",
                        language.id
                    ));
                }
            }
        }
        true
    }

    fn skip_or_fail(message: &str) -> bool {
        if require_nsjail_live() {
            panic!("{message}");
        }
        eprintln!("skipping {message}");
        false
    }

    fn require_nsjail_live() -> bool {
        std::env::var("OJOS_REQUIRE_NSJAIL_LIVE")
            .map(|value| {
                matches!(
                    value.trim().to_ascii_lowercase().as_str(),
                    "1" | "true" | "yes" | "on"
                )
            })
            .unwrap_or(false)
    }

    fn command_available(name: &str) -> bool {
        StdCommand::new(name)
            .arg("--version")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok()
    }

    fn create_verdict_matrix_workspace(language: &LanguageCase, verdict: MatrixVerdict) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "ojos-judge-verdict-matrix-{}-{}-{}",
            language.id,
            matrix_verdict_index(verdict),
            unique_nanos()
        ));
        let package = root.join("problem");
        let source_dir = root.join("source");
        std::fs::create_dir_all(package.join("tests")).expect("create problem tests");
        std::fs::create_dir_all(&source_dir).expect("create source dir");
        write_file(
            &source_dir.join(language.source_file),
            source_for_verdict(language, verdict),
        );
        write_problem_package(&package, verdict);
        root
    }

    fn write_problem_package(package: &Path, verdict: MatrixVerdict) {
        let case_time_limit = match verdict {
            MatrixVerdict::TimeLimitExceeded => 400,
            _ => 3000,
        };
        write_file(
            &package.join("problem.yaml"),
            r#"
schema: ojos.problem.v1
id: 1
slug: verdict-matrix
title: Verdict Matrix
type: traditional
visibility: public
status: ready
limits:
  default:
    time_ms: 3000
    memory_mb: 256
  languages:
    java17:
      time_ms: 5000
      memory_mb: 512
runner:
  config: runner.yaml
checker:
  config: checker.yaml
validator:
  config: validator.yaml
scorer:
  config: scorer.yaml
tests:
  root: tests
  groups: tests/groups.yaml
  cases: tests/cases.yaml
"#,
        );
        write_file(
            &package.join("runner.yaml"),
            "type: builtin\nname: traditional-runner\nconfig: {}\n",
        );
        write_file(
            &package.join("checker.yaml"),
            "type: builtin\nname: default-trim-checker\nconfig: {}\n",
        );
        write_file(
            &package.join("validator.yaml"),
            "type: builtin\nname: default-input-validator\nconfig: {}\n",
        );
        write_file(
            &package.join("scorer.yaml"),
            "type: builtin\nname: default-sum-scorer\nconfig: {}\n",
        );
        write_file(
            &package.join("tests").join("groups.yaml"),
            "groups:\n  - group_no: 0\n    score: 100\n    rule: sum\n",
        );
        write_file(
            &package.join("tests").join("cases.yaml"),
            &format!(
                r#"
cases:
  - case_no: 1
    input: 001.in
    answer: 001.ans
    score: 40
    group: 0
    sample: true
    hidden: false
    time_limit_ms: {case_time_limit}
    memory_limit_mb: 256
  - case_no: 2
    input: 002.in
    answer: 002.ans
    score: 60
    group: 0
    sample: false
    hidden: true
    time_limit_ms: {case_time_limit}
    memory_limit_mb: 256
"#,
            ),
        );
        write_file(&package.join("tests").join("001.in"), "1 2\n");
        write_file(&package.join("tests").join("001.ans"), "3\n");
        write_file(&package.join("tests").join("002.in"), "10 20\n");
        write_file(&package.join("tests").join("002.ans"), "30\n");
    }

    fn source_for_verdict(language: &LanguageCase, verdict: MatrixVerdict) -> &'static str {
        match verdict {
            MatrixVerdict::Accepted => language.accepted,
            MatrixVerdict::WrongAnswer => language.wrong_answer,
            MatrixVerdict::CompileError => language.compile_error,
            MatrixVerdict::RuntimeError => language.runtime_error,
            MatrixVerdict::TimeLimitExceeded => language.time_limit,
        }
    }

    fn expected_status(verdict: MatrixVerdict) -> &'static str {
        match verdict {
            MatrixVerdict::Accepted => "ACCEPTED",
            MatrixVerdict::WrongAnswer => "WRONG_ANSWER",
            MatrixVerdict::CompileError => "COMPILE_ERROR",
            MatrixVerdict::RuntimeError => "RUNTIME_ERROR",
            MatrixVerdict::TimeLimitExceeded => "TIME_LIMIT_EXCEEDED",
        }
    }

    fn expected_score(verdict: MatrixVerdict) -> i32 {
        match verdict {
            MatrixVerdict::Accepted => 100,
            _ => 0,
        }
    }

    fn matrix_verdict_index(verdict: MatrixVerdict) -> i64 {
        match verdict {
            MatrixVerdict::Accepted => 1,
            MatrixVerdict::WrongAnswer => 2,
            MatrixVerdict::CompileError => 3,
            MatrixVerdict::RuntimeError => 4,
            MatrixVerdict::TimeLimitExceeded => 5,
        }
    }

    fn unique_nanos() -> u128 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time")
            .as_nanos()
    }

    fn write_file(path: &Path, content: &str) {
        std::fs::write(path, content.trim_start()).expect("write matrix file");
    }
}
