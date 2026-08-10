use crate::{OrchestratorError, Result, parse_endpoint_id, validate_endpoint_id};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, HashSet};

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
    /// Link 启停开关，对应 Pg service_links.enabled（NOT NULL DEFAULT TRUE）。
    /// 注意必须使用 default_true 而不是 #[serde(default)]：bool 的 Default 是 false，
    /// 历史快照/请求里没有该字段时会被误判成“全部禁用”。
    #[serde(default = "default_true")]
    pub enabled: bool,
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
pub struct ServiceRelease {
    pub service_name: String,
    pub version: String,
    #[serde(default)]
    pub release_url: String,
    #[serde(default)]
    pub manifest: Value,
    #[serde(default)]
    pub checksum: String,
    #[serde(default)]
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HostService {
    pub host_ip: String,
    pub service_name: String,
    pub version: String,
    pub status: String,
    #[serde(default)]
    pub config: Value,
    #[serde(default)]
    pub labels: Value,
    #[serde(default)]
    pub created_at: String,
    #[serde(default)]
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct NodeRecord {
    pub node_id: String,
    pub host_ip: String,
    #[serde(default)]
    pub parent_node_id: String,
    pub role: String,
    #[serde(default)]
    pub labels: Value,
    pub status: String,
    #[serde(default)]
    pub created_at: String,
    #[serde(default)]
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ServiceApiSurface {
    pub service_name: String,
    pub version: String,
    pub api_id: String,
    pub protocol: String,
    pub port_name: String,
    #[serde(default)]
    pub path_prefix: String,
    #[serde(default)]
    pub methods: Vec<String>,
    pub visibility: String,
    pub auth_mode: String,
    pub permission: String,
    pub stability: String,
    pub api_version: String,
    #[serde(default)]
    pub rate_limit: String,
    #[serde(default)]
    pub timeout: String,
    #[serde(default)]
    pub config: Value,
    #[serde(default)]
    pub created_at: String,
    #[serde(default)]
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DeployedServiceApi {
    pub host_ip: String,
    pub service_name: String,
    pub version: String,
    pub endpoint: String,
    pub api_id: String,
    pub status: String,
    #[serde(default)]
    pub created_at: String,
    #[serde(default)]
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EffectiveApiRoute {
    pub node_id: String,
    pub api_id: String,
    pub provider_node_id: String,
    pub provider_host_ip: String,
    pub provider_service_name: String,
    pub provider_endpoint: String,
    pub protocol: String,
    pub path_prefix: String,
    pub methods: Vec<String>,
    pub permission: String,
    pub auth_mode: String,
    pub visibility_source: String,
    pub distance: u32,
    pub status: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ServiceRoute {
    pub path: String,
    pub method: String,
    pub target_type: String,
    pub target_service_name: String,
    #[serde(default)]
    pub target_selector: Value,
    #[serde(default)]
    pub permission: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub created_at: String,
    #[serde(default)]
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ServiceMigrationRecord {
    pub service_name: String,
    pub migration_version: String,
    #[serde(default)]
    pub checksum: String,
    pub status: String,
    #[serde(default)]
    pub applied_at: String,
    #[serde(default)]
    pub created_at: String,
    #[serde(default)]
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ServicePermissionRecord {
    pub service_name: String,
    pub permission_key: String,
    #[serde(default)]
    pub source: String,
    #[serde(default)]
    pub created_at: String,
    #[serde(default)]
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ServiceFrontendEntry {
    pub service_name: String,
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub route_prefix: String,
    #[serde(default)]
    pub remote_entry: String,
    #[serde(default)]
    pub menu_items: Vec<String>,
    #[serde(default)]
    pub created_at: String,
    #[serde(default)]
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ServiceRedisResource {
    pub service_name: String,
    pub name: String,
    pub kind: String,
    pub usage: String,
    #[serde(default)]
    pub created_at: String,
    #[serde(default)]
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ServiceStorageResource {
    pub service_name: String,
    pub object_type: String,
    pub bucket: String,
    #[serde(default)]
    pub path_prefix: String,
    #[serde(default)]
    pub created_at: String,
    #[serde(default)]
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RenderedServiceConfig {
    pub service_name: String,
    pub version: String,
    #[serde(default)]
    pub config: Value,
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
    #[serde(default)]
    pub confirmed_at: String,
    #[serde(default)]
    pub started_at: String,
    #[serde(default)]
    pub finished_at: String,
    #[serde(default)]
    pub rolled_back_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct OperationLogRecord {
    pub operation_id: String,
    #[serde(default)]
    pub step_id: String,
    pub level: String,
    pub message: String,
    #[serde(default)]
    pub data: Value,
    #[serde(default)]
    pub redacted: bool,
    #[serde(default)]
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct OperationLock {
    pub lock_key: String,
    pub operation_id: String,
    pub owner: String,
    pub expires_at: String,
    #[serde(default)]
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LogView {
    pub source_id: String,
    pub service_id: String,
    pub endpoint: String,
    #[serde(default)]
    pub operation_id: String,
    pub path: String,
    pub driver: String,
    pub read_policy: String,
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
    pub operation_id: String,
    #[serde(default)]
    pub data: Value,
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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TopologySnapshot {
    pub snapshot_id: String,
    pub topology: Topology,
    #[serde(default)]
    pub created_at: String,
}

pub fn validate_endpoint(endpoint: &Endpoint) -> Result<()> {
    validate_endpoint_id(&endpoint.endpoint)?;
    let identity = parse_endpoint_id(&endpoint.endpoint)?;
    if endpoint.service_id.trim().is_empty() {
        return Err(OrchestratorError::InvalidManifest(
            "endpoint service_id is required".to_string(),
        ));
    }
    if identity.service_name != endpoint.service_id {
        return Err(OrchestratorError::InvalidManifest(
            "endpoint service name must match service_id".to_string(),
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
    if link.protocol.trim().is_empty() {
        return Err(OrchestratorError::InvalidManifest(
            "link protocol is required".to_string(),
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

pub fn validate_service_release_record(release: &ServiceRelease) -> Result<()> {
    if release.service_name.trim().is_empty() {
        return Err(OrchestratorError::InvalidManifest(
            "release service_name is required".to_string(),
        ));
    }
    if release.version.trim().is_empty() {
        return Err(OrchestratorError::InvalidManifest(
            "release version is required".to_string(),
        ));
    }
    Ok(())
}

pub fn validate_host_service(host_service: &HostService) -> Result<()> {
    if host_service.host_ip.trim().is_empty() {
        return Err(OrchestratorError::InvalidManifest(
            "host service host_ip is required".to_string(),
        ));
    }
    if host_service.host_ip.parse::<std::net::IpAddr>().is_err() {
        return Err(OrchestratorError::InvalidManifest(
            "host service host_ip must be an IP address".to_string(),
        ));
    }
    if host_service.service_name.trim().is_empty() {
        return Err(OrchestratorError::InvalidManifest(
            "host service service_name is required".to_string(),
        ));
    }
    if host_service.version.trim().is_empty() {
        return Err(OrchestratorError::InvalidManifest(
            "host service version is required".to_string(),
        ));
    }
    if host_service.status.trim().is_empty() {
        return Err(OrchestratorError::InvalidManifest(
            "host service status is required".to_string(),
        ));
    }
    Ok(())
}

pub fn validate_node_record(node: &NodeRecord) -> Result<()> {
    if node.node_id.trim().is_empty() {
        return Err(OrchestratorError::InvalidManifest(
            "node node_id is required".to_string(),
        ));
    }
    if node.host_ip.trim().is_empty() {
        return Err(OrchestratorError::InvalidManifest(
            "node host_ip is required".to_string(),
        ));
    }
    if node.host_ip.parse::<std::net::IpAddr>().is_err() {
        return Err(OrchestratorError::InvalidManifest(
            "node host_ip must be an IP address".to_string(),
        ));
    }
    if !matches!(node.role.as_str(), "root" | "node" | "standalone") {
        return Err(OrchestratorError::InvalidManifest(
            "node role is invalid".to_string(),
        ));
    }
    if node.role == "root" && !node.parent_node_id.trim().is_empty() {
        return Err(OrchestratorError::InvalidManifest(
            "root node must not have parent_node_id".to_string(),
        ));
    }
    if node.role != "root" && node.node_id == node.parent_node_id {
        return Err(OrchestratorError::InvalidManifest(
            "node parent_node_id must not point to itself".to_string(),
        ));
    }
    if node.status.trim().is_empty() {
        return Err(OrchestratorError::InvalidManifest(
            "node status is required".to_string(),
        ));
    }
    Ok(())
}

pub fn validate_service_api_surface(api: &ServiceApiSurface) -> Result<()> {
    if api.service_name.trim().is_empty() || api.version.trim().is_empty() {
        return Err(OrchestratorError::InvalidManifest(
            "api surface service_name and version are required".to_string(),
        ));
    }
    if api.api_id.trim().is_empty() {
        return Err(OrchestratorError::InvalidManifest(
            "api surface api_id is required".to_string(),
        ));
    }
    if api.protocol.trim().is_empty() {
        return Err(OrchestratorError::InvalidManifest(
            "api surface protocol is required".to_string(),
        ));
    }
    if api.port_name.trim().is_empty() {
        return Err(OrchestratorError::InvalidManifest(
            "api surface port_name is required".to_string(),
        ));
    }
    if api.protocol == "http" || api.protocol == "https" {
        if api.path_prefix.trim().is_empty() || !api.path_prefix.starts_with('/') {
            return Err(OrchestratorError::InvalidManifest(
                "api surface path_prefix must start with /".to_string(),
            ));
        }
        if api.methods.is_empty() {
            return Err(OrchestratorError::InvalidManifest(
                "http api surface methods are required".to_string(),
            ));
        }
    }
    if !matches!(
        api.visibility.as_str(),
        "private" | "same-node" | "descendants" | "children" | "ancestors" | "global" | "explicit"
    ) {
        return Err(OrchestratorError::InvalidManifest(
            "api surface visibility is invalid".to_string(),
        ));
    }
    if !matches!(
        api.auth_mode.as_str(),
        "public" | "user" | "service" | "internal" | "workload"
    ) {
        return Err(OrchestratorError::InvalidManifest(
            "api surface auth_mode is invalid".to_string(),
        ));
    }
    if !matches!(
        api.stability.as_str(),
        "stable" | "experimental" | "deprecated"
    ) {
        return Err(OrchestratorError::InvalidManifest(
            "api surface stability is invalid".to_string(),
        ));
    }
    if api.api_version.trim().is_empty() {
        return Err(OrchestratorError::InvalidManifest(
            "api surface version is required".to_string(),
        ));
    }
    Ok(())
}

pub fn validate_deployed_service_api(api: &DeployedServiceApi) -> Result<()> {
    validate_endpoint_id(&api.endpoint)?;
    let identity = parse_endpoint_id(&api.endpoint)?;
    if identity.service_name != api.service_name {
        return Err(OrchestratorError::InvalidManifest(
            "deployed api endpoint service-name must match service_name".to_string(),
        ));
    }
    if api.host_ip.trim().is_empty()
        || api.service_name.trim().is_empty()
        || api.version.trim().is_empty()
        || api.api_id.trim().is_empty()
        || api.status.trim().is_empty()
    {
        return Err(OrchestratorError::InvalidManifest(
            "deployed api fields are required".to_string(),
        ));
    }
    Ok(())
}

pub fn validate_service_route(route: &ServiceRoute) -> Result<()> {
    if route.path.trim().is_empty() || !route.path.starts_with('/') {
        return Err(OrchestratorError::InvalidManifest(
            "route path must start with /".to_string(),
        ));
    }
    if route.method.trim().is_empty() {
        return Err(OrchestratorError::InvalidManifest(
            "route method is required".to_string(),
        ));
    }
    if !matches!(
        route.target_type.as_str(),
        "endpoint" | "endpoint-group" | "frontend"
    ) {
        return Err(OrchestratorError::InvalidManifest(
            "route target_type is invalid".to_string(),
        ));
    }
    if route.target_service_name.trim().is_empty() {
        return Err(OrchestratorError::InvalidManifest(
            "route target_service_name is required".to_string(),
        ));
    }
    Ok(())
}

pub fn validate_service_migration_record(record: &ServiceMigrationRecord) -> Result<()> {
    if record.service_name.trim().is_empty() {
        return Err(OrchestratorError::InvalidManifest(
            "migration service_name is required".to_string(),
        ));
    }
    if record.migration_version.trim().is_empty() {
        return Err(OrchestratorError::InvalidManifest(
            "migration version is required".to_string(),
        ));
    }
    if record.status.trim().is_empty() {
        return Err(OrchestratorError::InvalidManifest(
            "migration status is required".to_string(),
        ));
    }
    Ok(())
}

pub fn validate_service_permission_record(record: &ServicePermissionRecord) -> Result<()> {
    if record.service_name.trim().is_empty() {
        return Err(OrchestratorError::InvalidManifest(
            "permission service_name is required".to_string(),
        ));
    }
    if record.permission_key.trim().is_empty() {
        return Err(OrchestratorError::InvalidManifest(
            "permission key is required".to_string(),
        ));
    }
    Ok(())
}

pub fn validate_service_frontend_entry(entry: &ServiceFrontendEntry) -> Result<()> {
    if entry.service_name.trim().is_empty() {
        return Err(OrchestratorError::InvalidManifest(
            "frontend service_name is required".to_string(),
        ));
    }
    if entry.enabled && entry.route_prefix.trim().is_empty() {
        return Err(OrchestratorError::InvalidManifest(
            "frontend route_prefix is required".to_string(),
        ));
    }
    Ok(())
}

pub fn validate_service_redis_resource(resource: &ServiceRedisResource) -> Result<()> {
    if resource.service_name.trim().is_empty() {
        return Err(OrchestratorError::InvalidManifest(
            "redis service_name is required".to_string(),
        ));
    }
    if resource.name.trim().is_empty() {
        return Err(OrchestratorError::InvalidManifest(
            "redis resource name is required".to_string(),
        ));
    }
    if resource.kind.trim().is_empty() {
        return Err(OrchestratorError::InvalidManifest(
            "redis resource kind is required".to_string(),
        ));
    }
    Ok(())
}

pub fn validate_service_storage_resource(resource: &ServiceStorageResource) -> Result<()> {
    if resource.service_name.trim().is_empty() {
        return Err(OrchestratorError::InvalidManifest(
            "storage service_name is required".to_string(),
        ));
    }
    if resource.object_type.trim().is_empty() {
        return Err(OrchestratorError::InvalidManifest(
            "storage object_type is required".to_string(),
        ));
    }
    if resource.bucket.trim().is_empty() {
        return Err(OrchestratorError::InvalidManifest(
            "storage bucket is required".to_string(),
        ));
    }
    Ok(())
}

pub fn validate_rendered_service_config(config: &RenderedServiceConfig) -> Result<()> {
    if config.service_name.trim().is_empty() {
        return Err(OrchestratorError::InvalidManifest(
            "rendered config service_name is required".to_string(),
        ));
    }
    if config.version.trim().is_empty() {
        return Err(OrchestratorError::InvalidManifest(
            "rendered config version is required".to_string(),
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
        exposure_policy: "root-host-web-tui-only".to_string(),
        notes: vec![
            "root host exposes the full Web/TUI control plane".to_string(),
            "non-root hosts cannot change global topology or create global links".to_string(),
        ],
    })
}

pub fn build_topology(
    root_endpoint: String,
    services: Vec<String>,
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
        endpoints,
        links,
        operations,
        log_views,
        diagnostic_reports,
    };
    validate_topology(&topology)?;
    Ok(topology)
}

pub fn diagnostic_report_json(topology: &Topology) -> Result<String> {
    let failed_operations = topology
        .operations
        .iter()
        .filter(|operation| matches!(operation.status, OperationStatus::Failed))
        .map(|operation| operation.operation_id.clone())
        .collect::<Vec<_>>();
    let unhealthy_endpoints = topology
        .endpoints
        .iter()
        .filter(|endpoint| {
            matches!(
                endpoint.health.as_str(),
                "degraded" | "blocked" | "unreachable"
            ) || !endpoint.reachable
        })
        .map(|endpoint| endpoint.endpoint.clone())
        .collect::<Vec<_>>();
    // 已禁用的 Link 是运维显式关停的连接，不参与健康统计，否则诊断会一直报假告警。
    let unhealthy_links = topology
        .links
        .iter()
        .filter(|link| {
            link.enabled && matches!(link.health.as_str(), "degraded" | "blocked" | "unreachable")
        })
        .map(|link| format!("{} -> {}", link.source_endpoint, link.target_endpoint))
        .collect::<Vec<_>>();
    let service_endpoint_groups = service_endpoint_groups(&topology.endpoints);

    serde_json::to_string_pretty(&serde_json::json!({
        "services_summary": {
            "count": topology.services.len(),
            "services": topology.services,
        },
        "service_name_endpoint_groups_summary": {
            "count": service_endpoint_groups.len(),
            "groups": service_endpoint_groups,
        },
        "endpoints_summary": {
            "count": topology.endpoints.len(),
            "unhealthy": unhealthy_endpoints,
        },
        "links_summary": {
            "count": topology.links.len(),
            "unhealthy": unhealthy_links,
        },
        "operations_summary": {
            "count": topology.operations.len(),
            "failed": failed_operations,
        },
        "recent_operation_logs": topology.log_views,
        "diagnostic_reports": topology.diagnostic_reports,
        "database_schema_check": {
            "formal_tables": crate::ORCHESTRATOR_TABLES,
        },
        "forbidden_concept_scan_summary": {
            "formal_core_objects": [
                "Service",
                "Endpoint",
                "Link",
                "Operation",
                "Topology",
                "LogView",
                "DiagnosticReport"
            ]
        }
    }))
    .map_err(OrchestratorError::Json)
}

fn service_endpoint_groups(endpoints: &[Endpoint]) -> BTreeMap<String, Vec<String>> {
    let mut groups = BTreeMap::<String, Vec<String>>::new();
    for endpoint in endpoints {
        groups
            .entry(format!("{}[*]", endpoint.service_id))
            .or_default()
            .push(endpoint.endpoint.clone());
    }
    for endpoints in groups.values_mut() {
        endpoints.sort();
    }
    groups
}

fn endpoint_host(endpoint: &str) -> Result<String> {
    Ok(parse_endpoint_id(endpoint)?.host.to_string())
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
        created_at: timestamp_marker("planned"),
        updated_at: timestamp_marker("planned"),
        confirmed_at: String::new(),
        started_at: String::new(),
        finished_at: String::new(),
        rolled_back_at: String::new(),
    };
    validate_operation(&operation)?;
    Ok(operation)
}

pub fn confirm_operation(operation: &Operation) -> Result<Operation> {
    ensure_operation_status(operation, &[OperationStatus::Planned])?;
    let mut next = operation.clone();
    next.status = OperationStatus::AwaitingConfirmation;
    next.confirmed_at = timestamp_marker("confirmed");
    next.updated_at = timestamp_marker("confirmed");
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
    next.started_at = timestamp_marker("started");
    next.updated_at = timestamp_marker("started");
    Ok(next)
}

pub fn succeed_operation(operation: &Operation, result: Value) -> Result<Operation> {
    ensure_operation_status(operation, &[OperationStatus::Running])?;
    let mut next = operation.clone();
    next.status = OperationStatus::Succeeded;
    next.result = result;
    next.error_message.clear();
    next.finished_at = timestamp_marker("finished");
    next.updated_at = timestamp_marker("finished");
    Ok(next)
}

pub fn fail_operation(operation: &Operation, error_message: impl AsRef<str>) -> Result<Operation> {
    ensure_operation_status(operation, &[OperationStatus::Running])?;
    let mut next = operation.clone();
    next.status = OperationStatus::Failed;
    next.error_message = redact_secret_text(error_message.as_ref());
    next.finished_at = timestamp_marker("failed");
    next.updated_at = timestamp_marker("failed");
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
    next.rolled_back_at = timestamp_marker("rolled_back");
    next.updated_at = timestamp_marker("rolled_back");
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
    next.updated_at = timestamp_marker("cancelled");
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
    next.updated_at = timestamp_marker("expired");
    Ok(next)
}

pub fn operation_log_record(
    operation_id: impl Into<String>,
    level: impl Into<String>,
    message: impl AsRef<str>,
) -> OperationLogRecord {
    operation_step_log_record(operation_id, "", level, message, Value::Null)
}

pub fn operation_step_log_record(
    operation_id: impl Into<String>,
    step_id: impl Into<String>,
    level: impl Into<String>,
    message: impl AsRef<str>,
    data: Value,
) -> OperationLogRecord {
    let original = message.as_ref();
    let redacted = redact_secret_text(original);
    OperationLogRecord {
        operation_id: operation_id.into(),
        step_id: step_id.into(),
        level: level.into(),
        redacted: redacted != original,
        message: redacted,
        data,
        created_at: String::new(),
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

fn timestamp_marker(label: &str) -> String {
    label.to_string()
}

fn default_true() -> bool {
    true
}

#[cfg(test)]
mod tests {
    use super::{ServiceApiSurface, validate_service_api_surface};
    use serde_json::Value;

    #[test]
    fn service_api_surface_accepts_service_contract_v2_workload_auth() {
        let surface = ServiceApiSurface {
            service_name: "storage-service".to_string(),
            version: "0.1.0".to_string(),
            api_id: "storage.object.get".to_string(),
            protocol: "http".to_string(),
            port_name: "default".to_string(),
            path_prefix: "/api/storage/objects".to_string(),
            methods: vec!["GET".to_string()],
            visibility: "explicit".to_string(),
            auth_mode: "workload".to_string(),
            permission: "storage.object.read".to_string(),
            stability: "stable".to_string(),
            api_version: "1.0.0".to_string(),
            rate_limit: String::new(),
            timeout: "300000ms".to_string(),
            config: Value::Null,
            created_at: String::new(),
            updated_at: String::new(),
        };

        validate_service_api_surface(&surface)
            .expect("Service Contract v2 workload surfaces must persist in the API registry");
    }
}
