use crate::{OrchestratorError, Result, validate_endpoint_id};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashSet;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Endpoint {
    pub endpoint: String,
    pub service_id: String,
    pub protocol: String,
    #[serde(default)]
    pub health_path: String,
    #[serde(default)]
    pub health: String,
    #[serde(default)]
    pub reachable: bool,
    #[serde(default)]
    pub display_name: String,
    #[serde(default)]
    pub note: String,
    #[serde(default)]
    pub config: Value,
    #[serde(default)]
    pub created_at: String,
    #[serde(default)]
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Link {
    pub source_endpoint: String,
    pub target_endpoint: String,
    pub protocol: String,
    #[serde(default)]
    pub auth_mode: String,
    #[serde(default)]
    pub scope: String,
    #[serde(default)]
    pub health: String,
    #[serde(default)]
    pub latency_ms: Option<u32>,
    #[serde(default)]
    pub config_ref: String,
    #[serde(default)]
    pub secret_ref: String,
    #[serde(default)]
    pub policy: Value,
    #[serde(default)]
    pub created_at: String,
    #[serde(default)]
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum OperationStatus {
    Planned,
    AwaitingConfirmation,
    Running,
    Succeeded,
    Failed,
    RolledBack,
    Cancelled,
    Expired,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Operation {
    pub operation_id: String,
    pub action: String,
    pub target_type: String,
    pub target_id: String,
    pub status: OperationStatus,
    #[serde(default)]
    pub request: Value,
    #[serde(default)]
    pub plan: Value,
    #[serde(default)]
    pub result: Value,
    #[serde(default)]
    pub error_message: String,
    #[serde(default)]
    pub rollback_plan: Value,
    #[serde(default)]
    pub created_at: String,
    #[serde(default)]
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct OperationLogRecord {
    pub operation_id: String,
    pub level: String,
    pub message: String,
    #[serde(default)]
    pub redacted: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LogView {
    pub source_id: String,
    pub service_id: String,
    pub endpoint: String,
    pub location: String,
    #[serde(default)]
    pub display_name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DiagnosticReport {
    pub report_id: String,
    pub target_type: String,
    pub target_id: String,
    pub status: String,
    pub summary: String,
    #[serde(default)]
    pub findings: Vec<DiagnosticFinding>,
    #[serde(default)]
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DiagnosticFinding {
    pub code: String,
    pub severity: String,
    pub message: String,
    #[serde(default)]
    pub redacted: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TopologyAuthority {
    pub root_host: String,
    pub root_endpoint: String,
    pub exposure_policy: String,
    #[serde(default)]
    pub notes: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Topology {
    pub root_host: String,
    pub root_endpoint: String,
    pub authority: TopologyAuthority,
    #[serde(default)]
    pub services: Vec<String>,
    #[serde(default)]
    pub sets: Vec<String>,
    #[serde(default)]
    pub endpoints: Vec<Endpoint>,
    #[serde(default)]
    pub links: Vec<Link>,
    #[serde(default)]
    pub operations: Vec<Operation>,
    #[serde(default)]
    pub log_views: Vec<LogView>,
    #[serde(default)]
    pub diagnostic_reports: Vec<DiagnosticReport>,
}

pub fn validate_endpoint(endpoint: &Endpoint) -> Result<()> {
    validate_endpoint_id(&endpoint.endpoint)?;
    if endpoint.service_id.trim().is_empty() {
        return Err(OrchestratorError::InvalidManifest(
            "endpoint service_id is required".to_string(),
        ));
    }
    if endpoint.protocol.trim().is_empty() {
        return Err(OrchestratorError::InvalidManifest(
            "endpoint protocol is required".to_string(),
        ));
    }
    Ok(())
}

pub fn validate_link(link: &Link, endpoints: &[Endpoint]) -> Result<()> {
    validate_endpoint_id(&link.source_endpoint)?;
    validate_endpoint_id(&link.target_endpoint)?;
    if link.source_endpoint == link.target_endpoint {
        return Err(OrchestratorError::InvalidManifest(
            "link source and target must be different endpoints".to_string(),
        ));
    }
    let known = endpoints
        .iter()
        .map(|item| item.endpoint.as_str())
        .collect::<HashSet<_>>();
    if !known.contains(link.source_endpoint.as_str()) {
        return Err(OrchestratorError::InvalidManifest(
            "link source endpoint is not registered".to_string(),
        ));
    }
    if !known.contains(link.target_endpoint.as_str()) {
        return Err(OrchestratorError::InvalidManifest(
            "link target endpoint is not registered".to_string(),
        ));
    }
    Ok(())
}

pub fn validate_topology(topology: &Topology) -> Result<()> {
    validate_endpoint_id(&topology.root_endpoint)?;
    if topology.root_host.trim().is_empty() {
        return Err(OrchestratorError::InvalidManifest(
            "topology root_host is required".to_string(),
        ));
    }
    let root_host = endpoint_host(&topology.root_endpoint)?;
    if topology.root_host != root_host {
        return Err(OrchestratorError::InvalidManifest(
            "topology root_host must match root_endpoint host".to_string(),
        ));
    }
    if topology.authority.root_host != topology.root_host {
        return Err(OrchestratorError::InvalidManifest(
            "authority root_host must match topology root_host".to_string(),
        ));
    }
    if topology.authority.root_endpoint != topology.root_endpoint {
        return Err(OrchestratorError::InvalidManifest(
            "authority root_endpoint must match topology root_endpoint".to_string(),
        ));
    }
    if topology.authority.exposure_policy.trim().is_empty() {
        return Err(OrchestratorError::InvalidManifest(
            "authority exposure_policy is required".to_string(),
        ));
    }
    let mut seen = HashSet::new();
    for endpoint in &topology.endpoints {
        validate_endpoint(endpoint)?;
        if !seen.insert(endpoint.endpoint.as_str()) {
            return Err(OrchestratorError::InvalidManifest(
                "duplicate endpoint".to_string(),
            ));
        }
    }
    if !seen.contains(topology.root_endpoint.as_str()) {
        return Err(OrchestratorError::InvalidManifest(
            "topology root_endpoint must be a registered endpoint".to_string(),
        ));
    }
    for link in &topology.links {
        validate_link(link, &topology.endpoints)?;
    }
    Ok(())
}

pub fn topology_authority(root_endpoint: &str) -> Result<TopologyAuthority> {
    let root_host = endpoint_host(root_endpoint)?;
    Ok(TopologyAuthority {
        root_host,
        root_endpoint: root_endpoint.to_string(),
        exposure_policy: "root-host-gui-tui-only".to_string(),
        notes: vec![
            "root host exposes full GUI/TUI".to_string(),
            "non-root hosts cannot change global topology or create global links".to_string(),
        ],
    })
}

pub fn build_topology(
    root_endpoint: String,
    services: Vec<String>,
    sets: Vec<String>,
    endpoints: Vec<Endpoint>,
    links: Vec<Link>,
    operations: Vec<Operation>,
    log_views: Vec<LogView>,
    diagnostic_reports: Vec<DiagnosticReport>,
) -> Result<Topology> {
    let authority = topology_authority(&root_endpoint)?;
    let topology = Topology {
        root_host: authority.root_host.clone(),
        root_endpoint,
        authority,
        services,
        sets,
        endpoints,
        links,
        operations,
        log_views,
        diagnostic_reports,
    };
    validate_topology(&topology)?;
    Ok(topology)
}

fn endpoint_host(endpoint: &str) -> Result<String> {
    validate_endpoint_id(endpoint)?;
    endpoint
        .rsplit_once(':')
        .map(|(host, _port)| host.to_string())
        .ok_or_else(|| OrchestratorError::InvalidManifest("endpoint must be IP:Port".to_string()))
}

pub fn plan_operation(
    operation_id: impl Into<String>,
    action: impl Into<String>,
    target_type: impl Into<String>,
    target_id: impl Into<String>,
    request: Value,
    plan: Value,
    rollback_plan: Value,
) -> Result<Operation> {
    let operation = Operation {
        operation_id: operation_id.into(),
        action: action.into(),
        target_type: target_type.into(),
        target_id: target_id.into(),
        status: OperationStatus::Planned,
        request,
        plan,
        result: Value::Null,
        error_message: String::new(),
        rollback_plan,
        created_at: String::new(),
        updated_at: String::new(),
    };
    validate_operation(&operation)?;
    Ok(operation)
}

pub fn confirm_operation(operation: &Operation) -> Result<Operation> {
    ensure_operation_status(operation, &[OperationStatus::Planned])?;
    let mut next = operation.clone();
    next.status = OperationStatus::AwaitingConfirmation;
    Ok(next)
}

pub fn start_operation(operation: &Operation) -> Result<Operation> {
    ensure_operation_status(
        operation,
        &[
            OperationStatus::Planned,
            OperationStatus::AwaitingConfirmation,
        ],
    )?;
    let mut next = operation.clone();
    next.status = OperationStatus::Running;
    Ok(next)
}

pub fn succeed_operation(operation: &Operation, result: Value) -> Result<Operation> {
    ensure_operation_status(operation, &[OperationStatus::Running])?;
    let mut next = operation.clone();
    next.status = OperationStatus::Succeeded;
    next.result = result;
    next.error_message.clear();
    Ok(next)
}

pub fn fail_operation(operation: &Operation, error_message: impl AsRef<str>) -> Result<Operation> {
    ensure_operation_status(operation, &[OperationStatus::Running])?;
    let mut next = operation.clone();
    next.status = OperationStatus::Failed;
    next.error_message = redact_secret_text(error_message.as_ref());
    Ok(next)
}

pub fn rollback_operation(operation: &Operation, result: Value) -> Result<Operation> {
    ensure_operation_status(
        operation,
        &[OperationStatus::Failed, OperationStatus::Succeeded],
    )?;
    if operation.rollback_plan.is_null() {
        return Err(OrchestratorError::Blocked(
            "operation rollback plan is not available".to_string(),
        ));
    }
    let mut next = operation.clone();
    next.status = OperationStatus::RolledBack;
    next.result = result;
    Ok(next)
}

pub fn cancel_operation(operation: &Operation) -> Result<Operation> {
    ensure_operation_status(
        operation,
        &[
            OperationStatus::Planned,
            OperationStatus::AwaitingConfirmation,
        ],
    )?;
    let mut next = operation.clone();
    next.status = OperationStatus::Cancelled;
    Ok(next)
}

pub fn expire_operation(operation: &Operation) -> Result<Operation> {
    ensure_operation_status(
        operation,
        &[
            OperationStatus::Planned,
            OperationStatus::AwaitingConfirmation,
        ],
    )?;
    let mut next = operation.clone();
    next.status = OperationStatus::Expired;
    Ok(next)
}

pub fn operation_log_record(
    operation_id: impl Into<String>,
    level: impl Into<String>,
    message: impl AsRef<str>,
) -> OperationLogRecord {
    let original = message.as_ref();
    let redacted = redact_secret_text(original);
    OperationLogRecord {
        operation_id: operation_id.into(),
        level: level.into(),
        redacted: redacted != original,
        message: redacted,
    }
}

pub fn redact_secret_text(value: &str) -> String {
    let lower = value.to_ascii_lowercase();
    if lower.contains("token")
        || lower.contains("secret")
        || lower.contains("password")
        || lower.contains("private_key")
    {
        "<redacted>".to_string()
    } else {
        value.to_string()
    }
}

fn validate_operation(operation: &Operation) -> Result<()> {
    if operation.operation_id.trim().is_empty() {
        return Err(OrchestratorError::InvalidManifest(
            "operation_id is required".to_string(),
        ));
    }
    if operation.action.trim().is_empty() {
        return Err(OrchestratorError::InvalidManifest(
            "operation action is required".to_string(),
        ));
    }
    if operation.target_type.trim().is_empty() {
        return Err(OrchestratorError::InvalidManifest(
            "operation target_type is required".to_string(),
        ));
    }
    if operation.target_id.trim().is_empty() {
        return Err(OrchestratorError::InvalidManifest(
            "operation target_id is required".to_string(),
        ));
    }
    Ok(())
}

fn ensure_operation_status(operation: &Operation, allowed: &[OperationStatus]) -> Result<()> {
    if allowed.iter().any(|status| status == &operation.status) {
        Ok(())
    } else {
        Err(OrchestratorError::Blocked(format!(
            "operation status {:?} cannot transition",
            operation.status
        )))
    }
}
