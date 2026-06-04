use anyhow::{Context, Result, anyhow};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};
use tokio::fs;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;
use tokio::time::timeout;

use crate::checker::truncate_message;
use crate::config::{LanguageConfig, render_arg};
use crate::problem_package::LimitConfig;

#[derive(Debug, Clone)]
pub struct SandboxOutput {
    pub status: SandboxStatus,
    pub time_ms: i32,
    pub memory_kb: i32,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SandboxStatus {
    Ok,
    TimeLimitExceeded,
    MemoryLimitExceeded,
    RuntimeError,
    SystemError,
}

pub async fn compile_in_sandbox(
    lang: &LanguageConfig,
    source_path: &Path,
    submission_dir: &Path,
) -> Result<Option<String>> {
    if !lang.compile.enabled {
        return Ok(None);
    }

    let build_dir = submission_dir.join("build");
    fs::create_dir_all(&build_dir).await?;

    let source_name = source_path
        .file_name()
        .ok_or_else(|| anyhow!("source path has no file name"))?;

    let jail_source = build_dir.join(source_name);
    fs::copy(source_path, &jail_source)
        .await
        .context("copy source to build dir failed")?;

    let exe_path = if lang.exe_file.is_empty() {
        build_dir.join("unused-exe")
    } else {
        build_dir.join(&lang.exe_file)
    };

    let inside_source = PathBuf::from("/work").join(source_name);
    let inside_exe = if lang.exe_file.is_empty() {
        PathBuf::from("/work/unused-exe")
    } else {
        PathBuf::from("/work").join(&lang.exe_file)
    };

    let args: Vec<String> = lang
        .compile
        .args
        .iter()
        .map(|arg| render_arg(arg, &inside_source, &inside_exe, Path::new("/work")))
        .collect();

    let compile_command = render_arg(
        &lang.compile.command,
        &inside_source,
        &inside_exe,
        Path::new("/work"),
    );

    let shell = shell_command(&compile_command, &args);

    let compile_stdout = build_dir.join("compile.stdout.log");
    let compile_stderr = build_dir.join("compile.stderr.log");
    let compile_log = build_dir.join("compile.log");

    let _ = fs::remove_file(&compile_stdout).await;
    let _ = fs::remove_file(&compile_stderr).await;
    let _ = fs::remove_file(&compile_log).await;

    let shell = format!(
        "{} > /work/compile.stdout.log 2> /work/compile.stderr.log",
        shell
    );

    let output = run_nsjail_shell(
        &build_dir,
        &shell,
        None,
        None,
        None,
        lang.compile.timeout_ms.max(1000),
        lang.compile.memory_mb.unwrap_or(1024),
    )
    .await?;

    let stdout_text = fs::read_to_string(&compile_stdout)
        .await
        .unwrap_or_default();
    let stderr_text = fs::read_to_string(&compile_stderr)
        .await
        .unwrap_or_default();

    let merged_log = format!(
        "sandbox_status: {:?}\nsandbox_message: {}\n\n[stdout]\n{}\n\n[stderr]\n{}\n",
        output.status, output.message, stdout_text, stderr_text
    );

    fs::write(&compile_log, &merged_log).await?;

    match output.status {
        SandboxStatus::Ok => {
            if !lang.exe_file.is_empty() && fs::metadata(&exe_path).await.is_err() {
                Ok(Some(
                    "compile succeeded but executable not found".to_string(),
                ))
            } else {
                Ok(None)
            }
        }
        SandboxStatus::TimeLimitExceeded => Ok(Some("compile timeout".to_string())),
        _ => {
            if merged_log.trim().is_empty() {
                Ok(Some(truncate_message(&output.message)))
            } else {
                Ok(Some(truncate_message(&merged_log)))
            }
        }
    }
}

pub async fn run_case_in_sandbox(
    lang: &LanguageConfig,
    source_path: &Path,
    submission_dir: &Path,
    case_dir: &Path,
    stdin_path: &Path,
    stdout_path: &Path,
    stderr_path: &Path,
    limit: &LimitConfig,
) -> Result<SandboxOutput> {
    fs::create_dir_all(case_dir).await?;

    if lang.exe_file.is_empty() {
        let source_name = source_path
            .file_name()
            .ok_or_else(|| anyhow!("source path has no file name"))?;

        fs::copy(source_path, case_dir.join(source_name))
            .await
            .context("copy script source to case dir failed")?;

        let inside_source = PathBuf::from("/work").join(source_name);
        let run_args: Vec<String> = lang
            .run
            .args
            .iter()
            .map(|arg| render_arg(arg, &inside_source, Path::new(""), Path::new("/work")))
            .collect();

        let run_command = render_arg(
            &lang.run.command,
            &inside_source,
            Path::new(""),
            Path::new("/work"),
        );

        let shell = shell_command(&run_command, &run_args);
        let shell = format!(
            "{} < /work/stdin.txt > /work/stdout.txt 2> /work/stderr.txt",
            shell
        );

        return run_nsjail_shell(
            case_dir,
            &shell,
            None,
            None,
            None,
            limit.time_ms,
            limit.memory_mb,
        )
        .await;
    }

    let build_exe = submission_dir.join("build").join(&lang.exe_file);
    let case_exe = case_dir.join(&lang.exe_file);

    fs::copy(&build_exe, &case_exe)
        .await
        .with_context(|| format!("copy executable failed: {}", build_exe.display()))?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        let mut perms = fs::metadata(&case_exe).await?.permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&case_exe, perms).await?;
    }

    let inside_exe = PathBuf::from("/work").join(&lang.exe_file);
    let run_args: Vec<String> = lang
        .run
        .args
        .iter()
        .map(|arg| render_arg(arg, Path::new(""), &inside_exe, Path::new("/work")))
        .collect();

    let run_command = render_arg(
        &lang.run.command,
        Path::new(""),
        &inside_exe,
        Path::new("/work"),
    );

    let shell = shell_command(&run_command, &run_args);
    let shell = format!(
        "{} < /work/stdin.txt > /work/stdout.txt 2> /work/stderr.txt",
        shell
    );

    run_nsjail_shell(
        case_dir,
        &shell,
        None,
        None,
        None,
        limit.time_ms,
        limit.memory_mb,
    )
    .await
}

async fn run_nsjail_shell(
    work_dir: &Path,
    shell_command: &str,
    stdin_file: Option<&Path>,
    stdout_file: Option<&Path>,
    stderr_file: Option<&Path>,
    time_limit_ms: u64,
    memory_mb: u64,
) -> Result<SandboxOutput> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        if let Ok(metadata) = std::fs::metadata(work_dir) {
            let mut perms = metadata.permissions();
            perms.set_mode(0o777);
            let _ = std::fs::set_permissions(work_dir, perms);
        }
    }
    let time_limit_sec = ((time_limit_ms + 999) / 1000).max(1);
    let wall_timeout = Duration::from_millis(time_limit_ms + 1000);

    let mut cmd = Command::new("nsjail");

    cmd.arg("--mode")
        .arg("o")
        .arg("--user")
        .arg("10001")
        .arg("--group")
        .arg("10001")
        .arg("--disable_clone_newuser")
        .arg("--time_limit")
        .arg(time_limit_sec.to_string())
        .arg("--rlimit_as")
        .arg(memory_mb.to_string())
        .arg("--rlimit_nofile")
        .arg("64")
        .arg("--rlimit_nproc")
        .arg("64")
        .arg("--cwd")
        .arg("/work")
        .arg("--chroot")
        .arg("/jail/root")
        .arg("--bindmount_ro")
        .arg("/bin:/bin")
        .arg("--bindmount_ro")
        .arg("/lib:/lib")
        .arg("--bindmount_ro")
        .arg("/lib64:/lib64")
        .arg("--bindmount_ro")
        .arg("/usr:/usr")
        .arg("--bindmount_ro")
        .arg("/etc/alternatives:/etc/alternatives")
        .arg("--bindmount_ro")
        .arg("/dev/null:/dev/null")
        .arg("--bindmount_ro")
        .arg("/dev/zero:/dev/zero")
        .arg("--bindmount_ro")
        .arg("/dev/urandom:/dev/urandom")
        .arg("--bindmount")
        .arg(format!("{}:/work", work_dir.display()))
        .arg("--tmpfsmount")
        .arg("/tmp")
        .arg("--")
        .arg("/bin/bash")
        .arg("-lc")
        .arg(format!(
            "export PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin; {}",
            shell_command
        ));

    if let Some(stdin_file) = stdin_file {
        let stdin = std::fs::File::open(stdin_file)
            .with_context(|| format!("open stdin failed: {}", stdin_file.display()))?;
        cmd.stdin(Stdio::from(stdin));
    } else {
        cmd.stdin(Stdio::null());
    }

    if let Some(stdout_file) = stdout_file {
        let stdout = std::fs::File::create(stdout_file)
            .with_context(|| format!("create stdout failed: {}", stdout_file.display()))?;
        cmd.stdout(Stdio::from(stdout));
    } else {
        cmd.stdout(Stdio::piped());
    }

    if let Some(stderr_file) = stderr_file {
        let stderr = std::fs::File::create(stderr_file)
            .with_context(|| format!("create stderr failed: {}", stderr_file.display()))?;
        cmd.stderr(Stdio::from(stderr));
    } else {
        cmd.stderr(Stdio::piped());
    }

    let start = Instant::now();

    let output = match timeout(wall_timeout, cmd.output()).await {
        Ok(result) => result.context("run nsjail failed")?,
        Err(_) => {
            return Ok(SandboxOutput {
                status: SandboxStatus::TimeLimitExceeded,
                time_ms: time_limit_ms as i32,
                memory_kb: 0,
                stdout: vec![],
                stderr: vec![],
                message: "time limit exceeded".to_string(),
            });
        }
    };

    let elapsed_ms = start.elapsed().as_millis() as i32;
    let mut stderr = output.stderr;

    if let Some(stderr_file) = stderr_file {
        stderr = fs::read(stderr_file).await.unwrap_or_default();
    }

    let mut stdout = output.stdout;
    if let Some(stdout_file) = stdout_file {
        stdout = fs::read(stdout_file).await.unwrap_or_default();
    }

    let stderr_text = String::from_utf8_lossy(&stderr).to_string();

    if output.status.success() {
        return Ok(SandboxOutput {
            status: SandboxStatus::Ok,
            time_ms: elapsed_ms,
            memory_kb: 0,
            stdout,
            stderr,
            message: String::new(),
        });
    }

    let exit_message = match output.status.code() {
        Some(code) => format!("process exited with code {}", code),
        None => format!("process terminated by signal"),
    };

    let raw_message = if stderr_text.trim().is_empty() {
        exit_message
    } else {
        format!("{}\n{}", exit_message, stderr_text)
    };

    let message = truncate_message(&raw_message);

    let lower = raw_message.to_lowercase();

    let status = if lower.contains("time limit") || lower.contains("timed out") {
        SandboxStatus::TimeLimitExceeded
    } else if lower.contains("memory") || lower.contains("oom") {
        SandboxStatus::MemoryLimitExceeded
    } else {
        SandboxStatus::RuntimeError
    };

    Ok(SandboxOutput {
        status,
        time_ms: elapsed_ms,
        memory_kb: 0,
        stdout,
        stderr,
        message,
    })
}

fn shell_command(command: &str, args: &[String]) -> String {
    let mut parts = Vec::with_capacity(args.len() + 1);
    parts.push(shell_escape(command));

    for arg in args {
        parts.push(shell_escape(arg));
    }

    parts.join(" ")
}

fn shell_escape(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

pub async fn write_text(path: &Path, content: &str) -> Result<()> {
    let mut file = fs::File::create(path).await?;
    file.write_all(content.as_bytes()).await?;
    Ok(())
}
