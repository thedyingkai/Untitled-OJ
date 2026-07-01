use crate::{
    Endpoint, Link, LogView, OrchestratorError, ReleaseRuntimeDecl, Result, validate_endpoint_id,
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DriverRequest {
    pub action: String,
    pub service_id: String,
    pub endpoint: String,
    pub link: Option<Link>,
    pub log_source: Option<LogView>,
    #[serde(default)]
    pub release_runtime: Option<ReleaseRuntimeDecl>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DriverResult {
    pub action: String,
    pub status: String,
    pub message: String,
    pub command: Vec<String>,
    #[serde(default)]
    pub pid: Option<u32>,
    #[serde(default)]
    pub pid_file: String,
}

pub trait ExecutionDriver {
    fn name(&self) -> &'static str;
    fn execute(&self, request: &DriverRequest) -> Result<DriverResult>;
}

#[derive(Debug, Clone)]
pub struct LocalProcessDriver {
    project_dir: PathBuf,
    state_dir: PathBuf,
}

#[derive(Debug, Clone)]
pub struct DockerComposeDriver {
    docker_binary: String,
    compose_file: PathBuf,
    project_dir: PathBuf,
    run_commands: bool,
}

#[derive(Debug, Default, Clone)]
pub struct ExternalEndpointDriver;

impl LocalProcessDriver {
    pub fn new() -> Self {
        let project_dir = std::env::var("ORCHESTRATOR_RELEASE_PACKAGE_ROOT")
            .or_else(|_| std::env::var("OJOS_REPO_ROOT"))
            .ok()
            .map(PathBuf::from)
            .or_else(|| std::env::current_dir().ok())
            .unwrap_or_else(|| PathBuf::from("."));
        Self::with_project_dir(project_dir)
    }

    pub fn with_project_dir(project_dir: impl Into<PathBuf>) -> Self {
        let project_dir = project_dir.into();
        let state_dir = std::env::var("OJOS_LOCAL_PROCESS_STATE_DIR")
            .ok()
            .filter(|value| !value.trim().is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                project_dir
                    .join(".ojos")
                    .join("runtime")
                    .join("local-process")
            });
        Self {
            project_dir,
            state_dir,
        }
    }

    pub fn with_state_dir(mut self, state_dir: impl Into<PathBuf>) -> Self {
        self.state_dir = state_dir.into();
        self
    }
}

impl DockerComposeDriver {
    pub fn new(project_dir: impl Into<PathBuf>, compose_file: impl Into<PathBuf>) -> Self {
        Self {
            docker_binary: Self::docker_binary_from_env(),
            project_dir: project_dir.into(),
            compose_file: compose_file.into(),
            run_commands: false,
        }
    }

    pub fn docker_binary_from_env() -> String {
        std::env::var("OJOS_ORCHESTRATOR_DOCKER_BINARY")
            .ok()
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| "docker".to_string())
    }

    pub fn with_execution_enabled(mut self) -> Self {
        self.run_commands = true;
        self
    }

    #[cfg(test)]
    pub(crate) fn with_docker_binary_for_test(mut self, binary: impl Into<String>) -> Self {
        self.docker_binary = binary.into();
        self
    }

    pub fn command_for(&self, action: &str, service_id: &str) -> Result<Vec<String>> {
        let subcommand = match action {
            "release.install" | "service.enable" => "up",
            "service.start" => "start",
            "service.stop" | "service.disable" => "stop",
            "service.restart" => "restart",
            "service.delete" => "rm",
            "log.create" | "log.query" => "logs",
            "service.health.check" => "ps",
            _ => return unsupported(action, "docker compose driver action is not fixed"),
        };
        let mut command = vec![
            safe_executable(&self.docker_binary)?,
            "compose".to_string(),
            "--project-directory".to_string(),
            safe_path(&self.project_dir)?,
            "-f".to_string(),
            safe_path(&self.compose_file)?,
            subcommand.to_string(),
        ];
        match subcommand {
            "up" => {
                command.push("-d".to_string());
                command.push(service_id.to_string());
            }
            "ps" => {}
            _ => command.push(service_id.to_string()),
        }
        Ok(command)
    }
}

impl ExecutionDriver for LocalProcessDriver {
    fn name(&self) -> &'static str {
        "local-process"
    }

    fn execute(&self, request: &DriverRequest) -> Result<DriverResult> {
        match request.action.as_str() {
            "service.health.check" | "log.create" | "log.query" => Ok(DriverResult {
                action: request.action.clone(),
                status: "SUPPORTED".to_string(),
                message: "local process read-only action is allowed".to_string(),
                command: Vec::new(),
                pid: None,
                pid_file: String::new(),
            }),
            "release.install" | "service.start" | "service.enable" => {
                self.start_local_process(request)
            }
            "service.stop" | "service.disable" | "service.delete" => {
                self.stop_local_process(request)
            }
            "service.restart" => {
                let _ = self.stop_local_process(request)?;
                self.start_local_process(request)
            }
            _ => unsupported(&request.action, "unsupported local process action"),
        }
    }
}

impl LocalProcessDriver {
    fn start_local_process(&self, request: &DriverRequest) -> Result<DriverResult> {
        let runtime = local_process_runtime(request)?;
        let command = expand_runtime_value(&runtime.command, &runtime.env)?;
        let executable = safe_command(&command)?;
        let args = runtime
            .args
            .iter()
            .map(|arg| {
                expand_runtime_value(arg, &runtime.env).and_then(|value| safe_argument(&value))
            })
            .collect::<Result<Vec<_>>>()?;
        let working_dir = self.runtime_working_dir(runtime)?;
        fs::create_dir_all(&self.state_dir)?;
        let pid_file = self.pid_file(&request.service_id)?;
        let stdout = OpenOptions::new().create(true).append(true).open(
            self.state_dir
                .join(format!("{}.stdout.log", request.service_id)),
        )?;
        let stderr = OpenOptions::new().create(true).append(true).open(
            self.state_dir
                .join(format!("{}.stderr.log", request.service_id)),
        )?;
        let mut command_builder = Command::new(&executable);
        command_builder
            .args(&args)
            .current_dir(&working_dir)
            .stdin(Stdio::null())
            .stdout(Stdio::from(stdout))
            .stderr(Stdio::from(stderr))
            .env("OJOS_SERVICE_ID", &request.service_id)
            .env("OJOS_SERVICE_ENDPOINT", &request.endpoint);
        for (key, value) in &runtime.env {
            let expanded = expand_runtime_value(value, &runtime.env)?;
            command_builder.env(key, expanded);
        }
        let child = command_builder.spawn().map_err(|err| {
            OrchestratorError::Dependency(format!(
                "local process service_start failed to spawn {}: {err}",
                request.service_id
            ))
        })?;
        let pid = child.id();
        fs::write(&pid_file, pid.to_string())?;
        Ok(DriverResult {
            action: request.action.clone(),
            status: "SUCCEEDED".to_string(),
            message: format!(
                "local process service_start spawned {} with pid {}",
                request.service_id, pid
            ),
            command: std::iter::once(executable).chain(args).collect(),
            pid: Some(pid),
            pid_file: safe_path(&pid_file)?,
        })
    }

    fn stop_local_process(&self, request: &DriverRequest) -> Result<DriverResult> {
        fs::create_dir_all(&self.state_dir)?;
        let pid_file = self.pid_file(&request.service_id)?;
        let pid_text = match fs::read_to_string(&pid_file) {
            Ok(value) => value,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                return Ok(DriverResult {
                    action: request.action.clone(),
                    status: "SUCCEEDED".to_string(),
                    message: format!(
                        "local process service_stop found no pid file for {}",
                        request.service_id
                    ),
                    command: Vec::new(),
                    pid: None,
                    pid_file: safe_path(&pid_file)?,
                });
            }
            Err(err) => return Err(err.into()),
        };
        let pid: u32 = pid_text.trim().parse().map_err(|_| {
            OrchestratorError::Dependency(format!(
                "local process pid file for {} is invalid",
                request.service_id
            ))
        })?;
        let command = stop_process_command(pid)?;
        let output = Command::new(&command[0]).args(&command[1..]).output();
        match output {
            Ok(output) if output.status.success() || process_already_stopped(&output) => {
                let _ = fs::remove_file(&pid_file);
                Ok(DriverResult {
                    action: request.action.clone(),
                    status: "SUCCEEDED".to_string(),
                    message: format!(
                        "local process service_stop stopped {} pid {}",
                        request.service_id, pid
                    ),
                    command,
                    pid: Some(pid),
                    pid_file: safe_path(&pid_file)?,
                })
            }
            Ok(output) => Ok(DriverResult {
                action: request.action.clone(),
                status: "FAILED".to_string(),
                message: driver_output_message(&output)?,
                command,
                pid: Some(pid),
                pid_file: safe_path(&pid_file)?,
            }),
            Err(err) => Ok(DriverResult {
                action: request.action.clone(),
                status: "FAILED".to_string(),
                message: format!("local process service_stop failed to run stop command: {err}"),
                command,
                pid: Some(pid),
                pid_file: safe_path(&pid_file)?,
            }),
        }
    }

    fn runtime_working_dir(&self, runtime: &ReleaseRuntimeDecl) -> Result<PathBuf> {
        let relative = runtime.working_dir.trim();
        let path = if relative.is_empty() {
            self.project_dir.clone()
        } else {
            safe_relative_path(relative).map(|path| self.project_dir.join(path))?
        };
        Ok(path)
    }

    fn pid_file(&self, service_id: &str) -> Result<PathBuf> {
        Ok(self
            .state_dir
            .join(format!("{}.pid", safe_file_stem(service_id)?)))
    }
}

impl ExecutionDriver for DockerComposeDriver {
    fn name(&self) -> &'static str {
        "docker-compose"
    }

    fn execute(&self, request: &DriverRequest) -> Result<DriverResult> {
        let command = self.command_for(&request.action, &request.service_id)?;
        if self.run_commands {
            let output = match Command::new(&command[0]).args(&command[1..]).output() {
                Ok(output) => output,
                Err(err) => {
                    return Ok(DriverResult {
                        action: request.action.clone(),
                        status: "FAILED".to_string(),
                        message: format!("docker compose fixed command failed to start: {err}"),
                        command,
                        pid: None,
                        pid_file: String::new(),
                    });
                }
            };
            let status = if output.status.success() {
                "SUCCEEDED"
            } else {
                "FAILED"
            };
            return Ok(DriverResult {
                action: request.action.clone(),
                status: status.to_string(),
                message: driver_output_message(&output)?,
                command,
                pid: None,
                pid_file: String::new(),
            });
        }
        Ok(DriverResult {
            action: request.action.clone(),
            status: "PLANNED".to_string(),
            message: "fixed docker compose command built".to_string(),
            command,
            pid: None,
            pid_file: String::new(),
        })
    }
}

impl ExecutionDriver for ExternalEndpointDriver {
    fn name(&self) -> &'static str {
        "external-endpoint"
    }

    fn execute(&self, request: &DriverRequest) -> Result<DriverResult> {
        if !request.endpoint.is_empty() {
            validate_endpoint_id(&request.endpoint)?;
        }
        if matches!(
            request.action.as_str(),
            "link.create" | "link.update" | "link.delete" | "link.health.check"
        ) {
            let link = request.link.as_ref().ok_or_else(|| {
                OrchestratorError::InvalidManifest(
                    "external endpoint link action requires source_endpoint and target_endpoint"
                        .to_string(),
                )
            })?;
            validate_endpoint_id(&link.source_endpoint)?;
            validate_endpoint_id(&link.target_endpoint)?;
        }
        match request.action.as_str() {
            "endpoint.create"
            | "endpoint.update"
            | "endpoint.delete"
            | "endpoint.health.check"
            | "link.create"
            | "link.update"
            | "link.delete"
            | "link.health.check"
            | "log.create"
            | "log.query"
            | "diagnostic.create"
            | "diagnostic.export" => Ok(DriverResult {
                action: request.action.clone(),
                status: "SUPPORTED".to_string(),
                message: "external endpoint metadata action is allowed".to_string(),
                command: Vec::new(),
                pid: None,
                pid_file: String::new(),
            }),
            "service.start" | "service.stop" | "service.restart" => unsupported(
                &request.action,
                "external endpoint driver cannot control service lifecycle",
            ),
            _ => unsupported(&request.action, "unsupported external endpoint action"),
        }
    }
}

pub fn driver_request_for_endpoint(action: &str, endpoint: &Endpoint) -> DriverRequest {
    DriverRequest {
        action: action.to_string(),
        service_id: endpoint.service_id.clone(),
        endpoint: endpoint.endpoint.clone(),
        link: None,
        log_source: None,
        release_runtime: None,
    }
}

fn local_process_runtime(request: &DriverRequest) -> Result<&ReleaseRuntimeDecl> {
    let runtime = request.release_runtime.as_ref().ok_or_else(|| {
        OrchestratorError::Blocked(
            "local-process lifecycle requires release runtime configuration".to_string(),
        )
    })?;
    if runtime.kind.trim().eq_ignore_ascii_case("local-process") {
        Ok(runtime)
    } else {
        Err(OrchestratorError::Blocked(format!(
            "[DEFERRED] runtime kind {} not supported in local smoke",
            runtime.kind
        )))
    }
}

fn unsupported<T>(action: &str, reason: &str) -> Result<T> {
    Err(OrchestratorError::Blocked(format!(
        "{action} unsupported: {reason}"
    )))
}

fn safe_path(path: &Path) -> Result<String> {
    let text = path
        .to_str()
        .ok_or_else(|| OrchestratorError::UnsafePath("driver path must be UTF-8".to_string()))?
        .replace('\\', "/");
    if text.contains('\n') || text.contains('\r') || text.trim().is_empty() {
        return Err(OrchestratorError::UnsafePath(
            "driver path is not safe".to_string(),
        ));
    }
    Ok(text)
}

fn safe_relative_path(path: &str) -> Result<PathBuf> {
    let path = Path::new(path.trim());
    if path.is_absolute()
        || path
            .components()
            .any(|component| matches!(component, Component::ParentDir | Component::Prefix(_)))
    {
        return Err(OrchestratorError::UnsafePath(
            "local process working_dir must stay inside project directory".to_string(),
        ));
    }
    Ok(path.to_path_buf())
}

fn safe_command(value: &str) -> Result<String> {
    let trimmed = value.trim();
    if trimmed.is_empty()
        || trimmed.contains('\n')
        || trimmed.contains('\r')
        || trimmed.contains('\0')
    {
        return Err(OrchestratorError::UnsafePath(
            "local process command is not safe".to_string(),
        ));
    }
    Ok(trimmed.to_string())
}

fn safe_argument(value: &str) -> Result<String> {
    if value.contains('\n') || value.contains('\r') || value.contains('\0') {
        return Err(OrchestratorError::UnsafePath(
            "local process argument is not safe".to_string(),
        ));
    }
    Ok(value.to_string())
}

fn safe_file_stem(value: &str) -> Result<String> {
    let trimmed = value.trim();
    if trimmed.is_empty()
        || trimmed.contains('/')
        || trimmed.contains('\\')
        || trimmed.contains('\n')
        || trimmed.contains('\r')
        || trimmed.contains('\0')
    {
        return Err(OrchestratorError::UnsafePath(
            "local process service id is not safe".to_string(),
        ));
    }
    Ok(trimmed.to_string())
}

fn safe_executable(value: &str) -> Result<String> {
    let trimmed = value.trim();
    if trimmed.is_empty()
        || trimmed.contains('\n')
        || trimmed.contains('\r')
        || trimmed.contains('/')
        || trimmed.contains('\\')
    {
        return Err(OrchestratorError::UnsafePath(
            "driver executable is not safe".to_string(),
        ));
    }
    Ok(trimmed.to_string())
}

fn expand_runtime_value(value: &str, local_env: &BTreeMap<String, String>) -> Result<String> {
    let mut out = String::new();
    let mut rest = value;
    while let Some(start) = rest.find("${") {
        out.push_str(&rest[..start]);
        let after_start = &rest[start + 2..];
        let Some(end) = after_start.find('}') else {
            return Err(OrchestratorError::InvalidManifest(
                "local process runtime variable is not closed".to_string(),
            ));
        };
        let key = &after_start[..end];
        if key.is_empty()
            || !key
                .chars()
                .all(|ch| ch == '_' || ch.is_ascii_uppercase() || ch.is_ascii_digit())
        {
            return Err(OrchestratorError::InvalidManifest(
                "local process runtime variable is invalid".to_string(),
            ));
        }
        let placeholder = format!("${{{key}}}");
        let value = local_env
            .get(key)
            .filter(|value| value.trim() != placeholder)
            .cloned()
            .or_else(|| std::env::var(key).ok())
            .ok_or_else(|| {
                OrchestratorError::Dependency(format!(
                    "local process runtime variable {key} is not configured"
                ))
            })?;
        out.push_str(&value);
        rest = &after_start[end + 1..];
    }
    out.push_str(rest);
    Ok(out)
}

#[cfg(windows)]
fn stop_process_command(pid: u32) -> Result<Vec<String>> {
    Ok(vec![
        "taskkill".to_string(),
        "/PID".to_string(),
        pid.to_string(),
        "/T".to_string(),
        "/F".to_string(),
    ])
}

#[cfg(not(windows))]
fn stop_process_command(pid: u32) -> Result<Vec<String>> {
    Ok(vec!["kill".to_string(), pid.to_string()])
}

#[cfg(windows)]
fn process_already_stopped(output: &std::process::Output) -> bool {
    decode_driver_output_bytes(&output.stderr)
        .map(|text| text.contains("not found") || text.contains("not running"))
        .unwrap_or(false)
}

#[cfg(not(windows))]
fn process_already_stopped(output: &std::process::Output) -> bool {
    decode_driver_output_bytes(&output.stderr)
        .map(|text| text.contains("No such process"))
        .unwrap_or(false)
}

fn driver_output_message(output: &std::process::Output) -> Result<String> {
    let stdout = decode_driver_output_bytes(&output.stdout)?
        .trim()
        .to_string();
    let stderr = decode_driver_output_bytes(&output.stderr)?
        .trim()
        .to_string();
    if output.status.success() {
        if stdout.is_empty() {
            Ok("fixed docker compose command succeeded".to_string())
        } else {
            Ok(stdout)
        }
    } else if stderr.is_empty() {
        Ok(format!(
            "fixed docker compose command exited with {}",
            output.status
        ))
    } else {
        Ok(stderr)
    }
}

pub(crate) fn decode_driver_output_bytes(bytes: &[u8]) -> Result<String> {
    std::str::from_utf8(bytes)
        .map(str::to_string)
        .map_err(|err| OrchestratorError::Dependency(format!("driver output is not UTF-8: {err}")))
}
