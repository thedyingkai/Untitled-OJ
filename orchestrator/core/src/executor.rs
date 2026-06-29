use crate::{Endpoint, Link, LogView, OrchestratorError, Result, validate_endpoint_id};
#[cfg(target_os = "windows")]
use encoding_rs::GBK;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DriverRequest {
    pub action: String,
    pub service_id: String,
    pub endpoint: String,
    pub link: Option<Link>,
    pub log_source: Option<LogView>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DriverResult {
    pub action: String,
    pub status: String,
    pub message: String,
    pub command: Vec<String>,
}

pub trait ExecutionDriver {
    fn name(&self) -> &'static str;
    fn execute(&self, request: &DriverRequest) -> Result<DriverResult>;
}

#[derive(Debug, Default, Clone)]
pub struct LocalProcessDriver;

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
        Self
    }
}

impl DockerComposeDriver {
    pub fn new(project_dir: impl Into<PathBuf>, compose_file: impl Into<PathBuf>) -> Self {
        Self {
            docker_binary: "docker".to_string(),
            project_dir: project_dir.into(),
            compose_file: compose_file.into(),
            run_commands: false,
        }
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
            "service.start" | "service.enable" | "service.install" => "up",
            "service.stop" | "service.disable" => "stop",
            "service.restart" => "restart",
            "service.delete" => "rm",
            "service.logs.view" => "logs",
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
            "service.health.check" | "service.logs.view" => Ok(DriverResult {
                action: request.action.clone(),
                status: "SUPPORTED".to_string(),
                message: "local process read-only action is allowed".to_string(),
                command: Vec::new(),
            }),
            "service.start" | "service.stop" | "service.restart" | "service.enable"
            | "service.disable" | "service.install" | "service.delete" => unsupported(
                &request.action,
                "local process lifecycle needs a supervisor binding before it can run safely",
            ),
            _ => unsupported(&request.action, "unsupported local process action"),
        }
    }
}

impl ExecutionDriver for DockerComposeDriver {
    fn name(&self) -> &'static str {
        "docker-compose"
    }

    fn execute(&self, request: &DriverRequest) -> Result<DriverResult> {
        let command = self.command_for(&request.action, &request.service_id)?;
        if self.run_commands {
            let output = Command::new(&command[0])
                .args(&command[1..])
                .output()
                .map_err(|err| {
                    OrchestratorError::Dependency(format!(
                        "docker compose fixed command failed to start: {err}"
                    ))
                })?;
            let status = if output.status.success() {
                "SUCCEEDED"
            } else {
                "FAILED"
            };
            return Ok(DriverResult {
                action: request.action.clone(),
                status: status.to_string(),
                message: driver_output_message(&output),
                command,
            });
        }
        Ok(DriverResult {
            action: request.action.clone(),
            status: "PLANNED".to_string(),
            message: "fixed docker compose command built".to_string(),
            command,
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
            "endpoint.register"
            | "endpoint.update"
            | "endpoint.delete"
            | "endpoint.health.check"
            | "link.create"
            | "link.update"
            | "link.delete"
            | "link.health.check"
            | "service.logs.view"
            | "operation.logs.view"
            | "diagnostics.run"
            | "diagnostics.export" => Ok(DriverResult {
                action: request.action.clone(),
                status: "SUPPORTED".to_string(),
                message: "external endpoint metadata action is allowed".to_string(),
                command: Vec::new(),
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
    }
}

fn unsupported<T>(action: &str, reason: &str) -> Result<T> {
    Err(OrchestratorError::Blocked(format!(
        "{action} unsupported: {reason}"
    )))
}

fn safe_path(path: &Path) -> Result<String> {
    let text = path.to_string_lossy().replace('\\', "/");
    if text.contains('\n') || text.contains('\r') || text.trim().is_empty() {
        return Err(OrchestratorError::UnsafePath(
            "driver path is not safe".to_string(),
        ));
    }
    Ok(text)
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

fn driver_output_message(output: &std::process::Output) -> String {
    let stdout = decode_driver_output_bytes(&output.stdout)
        .trim()
        .to_string();
    let stderr = decode_driver_output_bytes(&output.stderr)
        .trim()
        .to_string();
    if output.status.success() {
        if stdout.is_empty() {
            "fixed docker compose command succeeded".to_string()
        } else {
            stdout
        }
    } else if stderr.is_empty() {
        format!("fixed docker compose command exited with {}", output.status)
    } else {
        stderr
    }
}

pub(crate) fn decode_driver_output_bytes(bytes: &[u8]) -> String {
    #[cfg(target_os = "windows")]
    {
        if let Ok(text) = std::str::from_utf8(bytes) {
            return text.to_string();
        }
        let (decoded, _, _) = GBK.decode(bytes);
        decoded.into_owned()
    }

    #[cfg(not(target_os = "windows"))]
    {
        String::from_utf8_lossy(bytes).to_string()
    }
}
