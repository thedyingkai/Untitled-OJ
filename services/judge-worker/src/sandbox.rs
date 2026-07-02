use anyhow::{Context, Result, anyhow};
use std::path::{Path, PathBuf};
use std::process::{Command as StdCommand, Stdio};
use std::time::{Duration, Instant};
use tokio::fs;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;
use tokio::time::timeout;

use crate::cgroup::CgroupRun;
use crate::checker::truncate_message;
use crate::config::{LanguageConfig, render_arg, render_run_arg};
use crate::problem_package::LimitConfig;

const COMPILE_OUTPUT_LIMIT_BYTES: u64 = 16 * 1024 * 1024;
const RUN_OUTPUT_LIMIT_BYTES: u64 = 64 * 1024 * 1024;

const COMPILE_FILE_SIZE_LIMIT_MB: u64 = 256;
const RUN_FILE_SIZE_LIMIT_MB: u64 = 64;

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
    OutputLimitExceeded,
    RuntimeError,
}

#[cfg_attr(not(test), allow(dead_code))]
pub fn nsjail_available() -> bool {
    StdCommand::new("nsjail")
        .arg("--help")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
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
        COMPILE_FILE_SIZE_LIMIT_MB,
        COMPILE_OUTPUT_LIMIT_BYTES,
    )
    .await?;

    let stdout_bytes = match read_file_limited(
        &compile_stdout,
        COMPILE_OUTPUT_LIMIT_BYTES,
        "compile stdout",
    )
    .await
    {
        Ok(data) => data,
        Err(err) => {
            let message = truncate_message(&err.to_string());
            let merged_log = format!(
                "sandbox_status: RuntimeError\nsandbox_message: {}\n\n[stdout]\n\n\n[stderr]\n\n",
                message
            );
            fs::write(&compile_log, &merged_log).await?;
            return Ok(Some(message));
        }
    };

    let stderr_bytes = match read_file_limited(
        &compile_stderr,
        COMPILE_OUTPUT_LIMIT_BYTES,
        "compile stderr",
    )
    .await
    {
        Ok(data) => data,
        Err(err) => {
            let message = truncate_message(&err.to_string());
            let merged_log = format!(
                "sandbox_status: RuntimeError\nsandbox_message: {}\n\n[stdout]\n\n\n[stderr]\n\n",
                message
            );
            fs::write(&compile_log, &merged_log).await?;
            return Ok(Some(message));
        }
    };

    let stdout_text = String::from_utf8_lossy(&stdout_bytes).to_string();
    let stderr_text = String::from_utf8_lossy(&stderr_bytes).to_string();

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
    _stdin_path: &Path,
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
            .map(|arg| {
                render_run_arg(
                    arg,
                    &inside_source,
                    Path::new(""),
                    Path::new("/work"),
                    limit.memory_mb,
                )
            })
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

        let output = run_nsjail_shell(
            case_dir,
            &shell,
            None,
            None,
            None,
            limit.time_ms,
            limit.memory_mb,
            RUN_FILE_SIZE_LIMIT_MB,
            RUN_OUTPUT_LIMIT_BYTES,
        )
        .await?;

        return attach_case_output(output, stdout_path, stderr_path).await;
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
        .map(|arg| {
            render_run_arg(
                arg,
                Path::new(""),
                &inside_exe,
                Path::new("/work"),
                limit.memory_mb,
            )
        })
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

    let output = run_nsjail_shell(
        case_dir,
        &shell,
        None,
        None,
        None,
        limit.time_ms,
        limit.memory_mb,
        RUN_FILE_SIZE_LIMIT_MB,
        RUN_OUTPUT_LIMIT_BYTES,
    )
    .await?;

    attach_case_output(output, stdout_path, stderr_path).await
}

async fn run_nsjail_shell(
    work_dir: &Path,
    shell_command: &str,
    stdin_file: Option<&Path>,
    stdout_file: Option<&Path>,
    stderr_file: Option<&Path>,
    time_limit_ms: u64,
    memory_mb: u64,
    file_size_limit_mb: u64,
    output_limit_bytes: u64,
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

    cmd.arg("--mode").arg("o");

    if env_bool("OJOS_NSJAIL_NO_PIVOTROOT") {
        cmd.arg("--no_pivotroot");
    }

    cmd.arg("--user")
        .arg("10001")
        .arg("--group")
        .arg("10001")
        .arg("--disable_clone_newuser")
        .arg("--time_limit")
        .arg(time_limit_sec.to_string())
        .arg("--rlimit_as")
        .arg("inf")
        .arg("--rlimit_fsize")
        .arg(file_size_limit_mb.to_string())
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
        .arg("--bindmount_ro")
        .arg("/dev/random:/dev/random");

    if Path::new("/etc/java-17-openjdk").exists() {
        cmd.arg("--bindmount_ro")
            .arg("/etc/java-17-openjdk:/etc/java-17-openjdk");
    }

    cmd.arg("--bindmount")
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

    let stdout_capture_path = stdout_file
        .map(|path| path.to_path_buf())
        .unwrap_or_else(|| work_dir.join(".nsjail.stdout.log"));
    let stderr_capture_path = stderr_file
        .map(|path| path.to_path_buf())
        .unwrap_or_else(|| work_dir.join(".nsjail.stderr.log"));

    let _ = std::fs::remove_file(&stdout_capture_path);
    let _ = std::fs::remove_file(&stderr_capture_path);

    let stdout = std::fs::File::create(&stdout_capture_path)
        .with_context(|| format!("create stdout failed: {}", stdout_capture_path.display()))?;
    cmd.stdout(Stdio::from(stdout));

    let stderr = std::fs::File::create(&stderr_capture_path)
        .with_context(|| format!("create stderr failed: {}", stderr_capture_path.display()))?;
    cmd.stderr(Stdio::from(stderr));

    let start = Instant::now();
    let cgroup =
        CgroupRun::create(memory_mb, 64).context("create cgroup v2 sandbox context failed")?;

    #[cfg(target_os = "linux")]
    {
        if let Some(cgroup_path) = cgroup.path() {
            let cgroup_procs = cgroup_path.join("cgroup.procs");
            unsafe {
                cmd.pre_exec(move || {
                    let pid = libc::getpid();
                    std::fs::write(&cgroup_procs, pid.to_string())?;
                    Ok(())
                });
            }
        }
    }

    let mut child = cmd.spawn().context("spawn nsjail failed")?;

    let exit_status = match timeout(wall_timeout, child.wait()).await {
        Ok(result) => result.context("wait nsjail failed")?,
        Err(_) => {
            let _ = child.start_kill();
            let _ = child.wait().await;
            let memory_kb = cgroup.memory_peak_kb().unwrap_or(0);
            if cgroup.oom_killed().unwrap_or(false) {
                return Ok(SandboxOutput {
                    status: SandboxStatus::MemoryLimitExceeded,
                    time_ms: time_limit_ms as i32,
                    memory_kb,
                    stdout: vec![],
                    stderr: vec![],
                    message: "memory limit exceeded".to_string(),
                });
            }
            return Ok(SandboxOutput {
                status: SandboxStatus::TimeLimitExceeded,
                time_ms: time_limit_ms as i32,
                memory_kb,
                stdout: vec![],
                stderr: vec![],
                message: "time limit exceeded".to_string(),
            });
        }
    };

    let elapsed_ms = start.elapsed().as_millis() as i32;
    let memory_kb = cgroup.memory_peak_kb().unwrap_or(0);
    let oom_killed = cgroup.oom_killed().unwrap_or(false);

    let stdout = match read_file_limited(&stdout_capture_path, output_limit_bytes, "stdout").await {
        Ok(data) => data,
        Err(err) => {
            return Ok(SandboxOutput {
                status: SandboxStatus::OutputLimitExceeded,
                time_ms: elapsed_ms,
                memory_kb,
                stdout: vec![],
                stderr: vec![],
                message: truncate_message(&err.to_string()),
            });
        }
    };

    let stderr = match read_file_limited(&stderr_capture_path, output_limit_bytes, "stderr").await {
        Ok(data) => data,
        Err(err) => {
            return Ok(SandboxOutput {
                status: SandboxStatus::OutputLimitExceeded,
                time_ms: elapsed_ms,
                memory_kb,
                stdout: vec![],
                stderr: vec![],
                message: truncate_message(&err.to_string()),
            });
        }
    };

    let stderr_text = String::from_utf8_lossy(&stderr).to_string();

    if oom_killed {
        return Ok(SandboxOutput {
            status: SandboxStatus::MemoryLimitExceeded,
            time_ms: elapsed_ms,
            memory_kb,
            stdout,
            stderr,
            message: "memory limit exceeded".to_string(),
        });
    }

    if exit_status.success() {
        return Ok(SandboxOutput {
            status: SandboxStatus::Ok,
            time_ms: elapsed_ms,
            memory_kb,
            stdout,
            stderr,
            message: String::new(),
        });
    }

    let exit_message = match exit_status.code() {
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
    } else if lower.contains("memory")
        || lower.contains("oom")
        || lower.contains("outofmemoryerror")
    {
        SandboxStatus::MemoryLimitExceeded
    } else {
        SandboxStatus::RuntimeError
    };

    Ok(SandboxOutput {
        status,
        time_ms: elapsed_ms,
        memory_kb,
        stdout,
        stderr,
        message,
    })
}

fn env_bool(key: &str) -> bool {
    std::env::var(key)
        .map(|value| {
            let value = value.trim();
            value == "1" || value.eq_ignore_ascii_case("true") || value.eq_ignore_ascii_case("yes")
        })
        .unwrap_or(false)
}

async fn attach_case_output(
    mut output: SandboxOutput,
    stdout_path: &Path,
    stderr_path: &Path,
) -> Result<SandboxOutput> {
    match read_file_limited(stdout_path, RUN_OUTPUT_LIMIT_BYTES, "stdout").await {
        Ok(data) => output.stdout = data,
        Err(err) => {
            return Ok(SandboxOutput {
                status: SandboxStatus::OutputLimitExceeded,
                time_ms: output.time_ms,
                memory_kb: output.memory_kb,
                stdout: vec![],
                stderr: vec![],
                message: truncate_message(&err.to_string()),
            });
        }
    }

    match read_file_limited(stderr_path, RUN_OUTPUT_LIMIT_BYTES, "stderr").await {
        Ok(data) => {
            if output.status == SandboxStatus::RuntimeError && looks_like_memory_error(&data) {
                output.status = SandboxStatus::MemoryLimitExceeded;
                output.message = "memory limit exceeded".to_string();
            }
            output.stderr = data;
        }
        Err(err) => {
            return Ok(SandboxOutput {
                status: SandboxStatus::OutputLimitExceeded,
                time_ms: output.time_ms,
                memory_kb: output.memory_kb,
                stdout: vec![],
                stderr: vec![],
                message: truncate_message(&err.to_string()),
            });
        }
    }

    Ok(output)
}

fn looks_like_memory_error(stderr: &[u8]) -> bool {
    let text = String::from_utf8_lossy(stderr).to_lowercase();
    text.contains("outofmemoryerror")
        || text.contains("memoryerror")
        || text.contains("java heap space")
        || text.contains("cannot allocate memory")
}

async fn read_file_limited(path: &Path, limit_bytes: u64, name: &str) -> Result<Vec<u8>> {
    match fs::metadata(path).await {
        Ok(meta) => {
            let size = meta.len();
            if size >= limit_bytes {
                return Err(anyhow!(
                    "{} output limit exceeded: {} bytes >= {} bytes",
                    name,
                    size,
                    limit_bytes
                ));
            }
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            return Ok(vec![]);
        }
        Err(err) => return Err(err.into()),
    }

    Ok(fs::read(path).await.unwrap_or_default())
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn nsjail_availability_probe_is_safe() {
        let _ = nsjail_available();
    }

    #[tokio::test]
    async fn nsjail_echo_smoke_when_available() {
        let Some(work_dir) = nsjail_live_work_dir("echo").await else {
            return;
        };
        if !Path::new("/bin/echo").exists() {
            eprintln!("skipping nsjail echo smoke: /bin/echo is unavailable");
            return;
        }

        let Some(output) =
            run_nsjail_live_or_skip(&work_dir, "/bin/echo OK", 1000, 1024 * 1024).await
        else {
            return;
        };

        assert_eq!(output.status, SandboxStatus::Ok);
        assert_eq!(String::from_utf8_lossy(&output.stdout), "OK\n");
        let _ = fs::remove_dir_all(&work_dir).await;
    }

    #[tokio::test]
    async fn nsjail_cpp_hello_world_when_available() {
        let Some(work_dir) = nsjail_live_work_dir("cpp-hello").await else {
            return;
        };
        if !command_available("g++") {
            eprintln!("skipping nsjail C++ hello smoke: g++ is unavailable");
            let _ = fs::remove_dir_all(&work_dir).await;
            return;
        }

        let command = "cat > main.cpp <<'EOF'\n#include <iostream>\nint main(){ std::cout << \"OK\\n\"; }\nEOF\ng++ main.cpp -O2 -pipe -o main\n./main";
        let Some(output) = run_nsjail_live_or_skip(&work_dir, command, 3000, 1024 * 1024).await
        else {
            return;
        };

        assert_eq!(output.status, SandboxStatus::Ok);
        assert_eq!(String::from_utf8_lossy(&output.stdout), "OK\n");
        let _ = fs::remove_dir_all(&work_dir).await;
    }

    #[tokio::test]
    async fn nsjail_runtime_error_when_available() {
        let Some(work_dir) = nsjail_live_work_dir("runtime-error").await else {
            return;
        };
        let Some(output) = run_nsjail_live_or_skip(&work_dir, "exit 7", 1000, 1024 * 1024).await
        else {
            return;
        };

        assert_eq!(output.status, SandboxStatus::RuntimeError);
        assert!(output.message.contains("code 7"));
        let _ = fs::remove_dir_all(&work_dir).await;
    }

    #[tokio::test]
    async fn nsjail_timeout_when_available() {
        let Some(work_dir) = nsjail_live_work_dir("timeout").await else {
            return;
        };
        let Some(output) =
            run_nsjail_live_or_skip(&work_dir, "while true; do :; done", 300, 1024 * 1024).await
        else {
            return;
        };

        assert_eq!(output.status, SandboxStatus::TimeLimitExceeded);
        let _ = fs::remove_dir_all(&work_dir).await;
    }

    #[tokio::test]
    async fn nsjail_output_limit_when_available() {
        let Some(work_dir) = nsjail_live_work_dir("output-limit").await else {
            return;
        };
        let Some(output) = run_nsjail_live_or_skip(&work_dir, "yes X", 1000, 1024).await else {
            return;
        };

        assert!(
            output.status == SandboxStatus::OutputLimitExceeded
                || output.status == SandboxStatus::TimeLimitExceeded,
            "unexpected status: {:?}",
            output.status
        );
        let _ = fs::remove_dir_all(&work_dir).await;
    }

    async fn nsjail_live_work_dir(name: &str) -> Option<PathBuf> {
        if !cfg!(target_os = "linux") {
            eprintln!("skipping nsjail {name} smoke: nsjail runner requires Linux/WSL");
            return None;
        }
        if !nsjail_available() {
            eprintln!(
                "skipping nsjail {name} smoke: nsjail binary is unavailable; install nsjail in Linux/WSL and rerun cargo test"
            );
            return None;
        }
        let work_dir = std::env::temp_dir().join(format!(
            "ojos-nsjail-{name}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("system time")
                .as_nanos()
        ));
        fs::create_dir_all(&work_dir)
            .await
            .expect("create nsjail smoke work dir");
        Some(work_dir)
    }

    async fn run_nsjail_live_or_skip(
        work_dir: &Path,
        command: &str,
        time_limit_ms: u64,
        output_limit_bytes: u64,
    ) -> Option<SandboxOutput> {
        match run_nsjail_shell(
            work_dir,
            command,
            None,
            None,
            None,
            time_limit_ms,
            64,
            16,
            output_limit_bytes,
        )
        .await
        {
            Ok(output) => Some(output),
            Err(err) if nsjail_environment_error(&err.to_string()) => {
                eprintln!("skipping nsjail live smoke: {err}");
                let _ = fs::remove_dir_all(work_dir).await;
                None
            }
            Err(err) => panic!("nsjail live smoke failed: {err}"),
        }
    }

    fn nsjail_environment_error(message: &str) -> bool {
        let lower = message.to_ascii_lowercase();
        lower.contains("cgroup")
            || lower.contains("operation not permitted")
            || lower.contains("permission denied")
            || lower.contains("no such file or directory")
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
}
