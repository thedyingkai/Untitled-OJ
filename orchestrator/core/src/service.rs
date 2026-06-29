use crate::{
    Endpoint, Link, Operation, OperationStatus, OrchestratorError, Result, Topology,
    plan_operation, sanitize_path_for_error, validate_endpoint, validate_link, validate_topology,
};
use regex::Regex;
use semver::Version;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashSet;
use std::fs;
use std::net::IpAddr;
use std::path::{Component, Path};

const SERVICE_SCHEMA_VERSION: u32 = 1;
const SET_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ServiceManifest {
    #[serde(default = "default_schema_version")]
    pub schema_version: u32,
    pub id: String,
    pub name: String,
    pub version: String,
    pub kind: String,
    #[serde(default)]
    pub description: String,
    pub endpoint: EndpointDecl,
    pub runtime: ServiceRuntimeDecl,
    #[serde(default)]
    pub config_schema: Value,
    pub requires: ServiceRequires,
    pub provides: ServiceProvides,
    #[serde(default)]
    pub ui: ServiceUiDecl,
    #[serde(default)]
    pub permissions: Vec<String>,
    #[serde(default)]
    pub security: ServiceSecurityDecl,
    pub source: SourceDecl,
    pub health: ServiceHealthDecl,
    #[serde(default)]
    pub resources: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct EndpointDecl {
    pub protocol: String,
    pub default_port: u16,
    #[serde(default)]
    pub health_path: String,
    #[serde(default)]
    pub expose: bool,
    #[serde(default)]
    pub routes: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ServiceRuntimeDecl {
    pub mode: RuntimeMode,
    #[serde(default)]
    pub driver: String,
    #[serde(default)]
    pub root_allowed: bool,
    #[serde(default)]
    pub non_root_allowed: bool,
    #[serde(default)]
    pub start_policy: String,
    #[serde(default)]
    pub restart_policy: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum RuntimeMode {
    LocalProcess,
    Container,
    External,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ServiceRequires {
    #[serde(default)]
    pub services: Vec<String>,
    #[serde(default)]
    pub links: Vec<RequiredLinkDecl>,
    #[serde(default)]
    pub optional_links: Vec<RequiredLinkDecl>,
    #[serde(default)]
    pub storage: Vec<String>,
    #[serde(default)]
    pub database: Vec<String>,
    #[serde(default)]
    pub queue: Vec<String>,
    #[serde(default)]
    pub secrets: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RequiredLinkDecl {
    pub id: String,
    pub protocol: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ServiceProvides {
    #[serde(default)]
    pub capabilities: Vec<String>,
    #[serde(default)]
    pub endpoints: Vec<String>,
    #[serde(default)]
    pub routes: Vec<String>,
    #[serde(default)]
    pub workers: Vec<String>,
    #[serde(default)]
    pub storage_buckets: Vec<String>,
    #[serde(default)]
    pub events: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ServiceUiDecl {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub menu_scope: String,
    #[serde(default)]
    pub routes: Vec<String>,
    #[serde(default)]
    pub menus: Vec<String>,
    #[serde(default)]
    pub permissions: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ServiceSecurityDecl {
    #[serde(default)]
    pub allow_privileged: bool,
    #[serde(default)]
    pub allow_host_mount: bool,
    #[serde(default)]
    pub allow_arbitrary_command: bool,
    #[serde(default)]
    pub required_secrets: Vec<String>,
    #[serde(default)]
    pub sandbox: Value,
    #[serde(default)]
    pub network_policy: Value,
}

impl Default for ServiceSecurityDecl {
    fn default() -> Self {
        Self {
            allow_privileged: false,
            allow_host_mount: false,
            allow_arbitrary_command: false,
            required_secrets: Vec::new(),
            sandbox: Value::Null,
            network_policy: Value::Null,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SourceDecl {
    #[serde(default)]
    pub r#type: String,
    #[serde(default, rename = "ref")]
    pub reference: String,
    #[serde(default)]
    pub build: Value,
    #[serde(default)]
    pub artifact: Value,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ServiceHealthDecl {
    #[serde(default)]
    pub checks: Vec<String>,
    #[serde(default)]
    pub timeout_seconds: u32,
    #[serde(default)]
    pub interval_seconds: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ServiceSet {
    #[serde(default = "default_schema_version")]
    pub schema_version: u32,
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub scenario: Value,
    #[serde(default)]
    pub services: Vec<ServiceSetService>,
    #[serde(default)]
    pub default_endpoints: Vec<ServiceSetEndpoint>,
    #[serde(default)]
    pub default_links: Vec<ServiceSetLink>,
    #[serde(default)]
    pub policies: Value,
    #[serde(default)]
    pub operations: ServiceSetOperations,
    #[serde(default)]
    pub notes: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(untagged)]
pub enum ServiceSetService {
    Id(String),
    Spec(ServiceSetServiceSpec),
}

impl ServiceSetService {
    pub fn id(&self) -> &str {
        match self {
            Self::Id(value) => value,
            Self::Spec(value) => &value.id,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ServiceSetServiceSpec {
    pub id: String,
    #[serde(default = "default_true")]
    pub required: bool,
    #[serde(default = "default_count")]
    pub count: u32,
    #[serde(default)]
    pub placement: Value,
    #[serde(default)]
    pub config: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ServiceSetEndpoint {
    pub service: String,
    pub port: u16,
    pub protocol: String,
    #[serde(default)]
    pub expose: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ServiceSetLink {
    pub from: String,
    pub to: String,
    #[serde(default)]
    pub protocol: String,
    #[serde(default)]
    pub auth_mode: String,
    #[serde(default)]
    pub scope: String,
    #[serde(default = "default_true")]
    pub required: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ServiceSetOperations {
    #[serde(default)]
    pub install_order: Vec<String>,
    #[serde(default)]
    pub start_order: Vec<String>,
    #[serde(default)]
    pub stop_order: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SetExpandResult {
    pub set_id: String,
    pub services: Vec<String>,
    pub default_links: Vec<ServiceSetLink>,
}

pub fn validate_service_manifest_file(
    repo_root: &Path,
    manifest_path: &Path,
) -> Result<ServiceManifest> {
    validate_service_manifest_path(repo_root, manifest_path)?;
    let text = fs::read_to_string(repo_root.join(manifest_path))?;
    let manifest: ServiceManifest = serde_yaml::from_str(&text)?;
    validate_service_manifest(&manifest)?;
    Ok(manifest)
}

pub fn validate_service_manifest(manifest: &ServiceManifest) -> Result<()> {
    let id_re = Regex::new(r"^[a-z0-9][a-z0-9.-]*$").expect("valid regex");
    let key_re = Regex::new(r"^[a-z0-9][a-z0-9_.:-]*$").expect("valid regex");
    let path_re = Regex::new(r"^/[A-Za-z0-9_./:{}-]*$").expect("valid regex");

    ensure(
        manifest.schema_version == SERVICE_SCHEMA_VERSION,
        "unsupported service schema_version",
    )?;
    ensure(
        id_re.is_match(manifest.id.trim()),
        "service id format is invalid",
    )?;
    ensure(!manifest.name.trim().is_empty(), "service name is required")?;
    Version::parse(manifest.version.trim()).map_err(|_| {
        OrchestratorError::InvalidManifest("service version must be semver".to_string())
    })?;
    ensure(
        is_supported_service_kind(&manifest.kind),
        "service kind is invalid",
    )?;
    ensure(
        is_supported_protocol(&manifest.endpoint.protocol),
        "endpoint protocol is invalid",
    )?;
    ensure(
        manifest.endpoint.default_port > 0,
        "endpoint default_port is required",
    )?;
    if !manifest.endpoint.health_path.trim().is_empty() {
        ensure(
            path_re.is_match(&manifest.endpoint.health_path),
            "endpoint health_path is invalid",
        )?;
    }
    ensure(
        manifest.runtime.root_allowed || manifest.runtime.non_root_allowed,
        "service must allow at least one runtime exposure mode",
    )?;
    ensure(
        is_supported_source_type(&manifest.source.r#type),
        "source.type is invalid",
    )?;
    ensure(
        !manifest.source.reference.trim().is_empty(),
        "source.ref is required",
    )?;
    ensure(
        !manifest.health.checks.is_empty(),
        "service health checks are required",
    )?;
    ensure(
        manifest.health.timeout_seconds > 0,
        "service health timeout_seconds is required",
    )?;
    ensure(
        manifest.health.interval_seconds > 0,
        "service health interval_seconds is required",
    )?;
    reject_dangerous_service_values(&serde_json::to_value(manifest)?)?;
    ensure(
        !manifest.security.allow_privileged,
        "service cannot allow privileged runtime",
    )?;
    ensure(
        !manifest.security.allow_host_mount,
        "service cannot allow host mount",
    )?;
    ensure(
        !manifest.security.allow_arbitrary_command,
        "service cannot allow arbitrary command",
    )?;

    unique_by(
        manifest.permissions.iter().map(String::as_str),
        "duplicate permission",
    )?;
    for permission in &manifest.permissions {
        ensure(key_re.is_match(permission), "permission key is invalid")?;
    }
    unique_by(
        manifest.provides.capabilities.iter().map(String::as_str),
        "duplicate capability",
    )?;
    for capability in &manifest.provides.capabilities {
        ensure(key_re.is_match(capability), "capability is invalid")?;
    }
    validate_link_requirements(&manifest.requires.links, &key_re)?;
    validate_link_requirements(&manifest.requires.optional_links, &key_re)?;
    Ok(())
}

pub fn validate_service_set_file(repo_root: &Path, set_path: &Path) -> Result<ServiceSet> {
    validate_set_path(repo_root, set_path)?;
    let text = fs::read_to_string(repo_root.join(set_path))?;
    let set: ServiceSet = serde_yaml::from_str(&text)?;
    validate_service_set(&set)?;
    validate_service_set_references(repo_root, &set)?;
    Ok(set)
}

pub fn validate_service_set_references(repo_root: &Path, set: &ServiceSet) -> Result<()> {
    let service_ids = discover_service_ids(repo_root)?;
    let set_service_ids = set
        .services
        .iter()
        .map(ServiceSetService::id)
        .collect::<HashSet<_>>();
    for service in &set.services {
        ensure(
            service_ids.contains(service.id()),
            "set references missing service",
        )?;
    }
    for endpoint in &set.default_endpoints {
        ensure(
            set_service_ids.contains(endpoint.service.as_str()),
            "default endpoint service is not in set",
        )?;
        ensure(
            service_ids.contains(endpoint.service.as_str()),
            "default endpoint references missing service",
        )?;
    }
    for link in &set.default_links {
        ensure(
            service_ids.contains(link.from.as_str()) && service_ids.contains(link.to.as_str()),
            "default link references missing service",
        )?;
    }
    validate_operation_order(
        &set.operations.install_order,
        &set_service_ids,
        "install_order",
    )?;
    validate_operation_order(&set.operations.start_order, &set_service_ids, "start_order")?;
    validate_operation_order(&set.operations.stop_order, &set_service_ids, "stop_order")?;
    Ok(())
}

pub fn validate_service_set(set: &ServiceSet) -> Result<()> {
    let id_re = Regex::new(r"^[a-z0-9][a-z0-9-]*$").expect("valid regex");
    ensure(
        set.schema_version == SET_SCHEMA_VERSION,
        "unsupported set schema_version",
    )?;
    ensure(id_re.is_match(&set.id), "set id is invalid")?;
    ensure(!set.name.trim().is_empty(), "set name is required")?;
    ensure(!set.services.is_empty(), "set services must not be empty")?;
    unique_by(
        set.services.iter().map(ServiceSetService::id),
        "duplicate set service",
    )?;
    for service in &set.services {
        ensure(id_re.is_match(service.id()), "set service id is invalid")?;
    }
    for link in &set.default_links {
        ensure(
            set.services.iter().any(|service| service.id() == link.from),
            "default link source is not in set",
        )?;
        ensure(
            set.services.iter().any(|service| service.id() == link.to),
            "default link target is not in set",
        )?;
        if !link.protocol.trim().is_empty() {
            ensure(
                is_supported_protocol(&link.protocol),
                "default link protocol is invalid",
            )?;
        }
    }
    Ok(())
}

pub fn validate_endpoint_id(value: &str) -> Result<()> {
    let Some((host, port)) = value.rsplit_once(':') else {
        return Err(OrchestratorError::InvalidManifest(
            "endpoint must be IP:Port".to_string(),
        ));
    };
    host.parse::<IpAddr>()
        .map_err(|_| OrchestratorError::InvalidManifest("endpoint IP is invalid".to_string()))?;
    let port = port
        .parse::<u16>()
        .map_err(|_| OrchestratorError::InvalidManifest("endpoint port is invalid".to_string()))?;
    ensure(port > 0, "endpoint port is invalid")
}

pub fn service_install_operation(
    operation_id: impl Into<String>,
    manifest: &ServiceManifest,
    installed_service_ids: &[String],
) -> Result<Operation> {
    let exists = installed_service_ids
        .iter()
        .any(|service_id| service_id == &manifest.id);
    let steps = if exists {
        serde_json::json!([
            {
                "action": "refresh_service_metadata",
                "target": manifest.id,
                "detail": "刷新 Service 元数据"
            }
        ])
    } else {
        serde_json::json!([
            {
                "action": "insert_service",
                "target": manifest.id,
                "detail": "写入 services 表"
            },
            {
                "action": "declare_default_endpoint",
                "target": format!("*:{}", manifest.endpoint.default_port),
                "detail": "实际 IP:Port 由 Orchestrator 绑定"
            }
        ])
    };
    let operation = plan_operation(
        operation_id,
        "service.install",
        "Service",
        manifest.id.clone(),
        serde_json::json!({
            "service_id": manifest.id,
            "version": manifest.version,
            "default_port": manifest.endpoint.default_port,
            "already_known": exists,
            "service_manifest": manifest
        }),
        serde_json::json!({
            "steps": steps,
            "requires_confirmation": true
        }),
        serde_json::json!({
            "steps": [
                {
                    "action": "restore_previous_service_definition",
                    "target": manifest.id
                }
            ]
        }),
    )?;
    debug_assert_eq!(operation.status, OperationStatus::Planned);
    Ok(operation)
}

pub fn service_lifecycle_operation(
    operation_id: impl Into<String>,
    action: &str,
    service_id: impl AsRef<str>,
) -> Result<Operation> {
    let service_id = service_id.as_ref().trim();
    ensure(!service_id.is_empty(), "service_id is required")?;
    let (step, rollback_step, requires_confirmation) = match action {
        "service.enable" => ("enable_service", "disable_service", true),
        "service.disable" => ("disable_service", "enable_service", true),
        "service.start" => ("start_service", "stop_service", false),
        "service.stop" => ("stop_service", "start_service", true),
        "service.restart" => ("restart_service", "restore_previous_service_state", true),
        "service.delete" => ("delete_service", "restore_service", true),
        _ => {
            return Err(OrchestratorError::InvalidManifest(format!(
                "unsupported service lifecycle action {action}"
            )));
        }
    };

    plan_operation(
        operation_id,
        action,
        "Service",
        service_id,
        serde_json::json!({
            "service_id": service_id,
        }),
        serde_json::json!({
            "steps": [
                {
                    "action": step,
                    "target": service_id
                }
            ],
            "requires_confirmation": requires_confirmation
        }),
        serde_json::json!({
            "steps": [
                {
                    "action": rollback_step,
                    "target": service_id
                }
            ]
        }),
    )
}

pub fn set_apply_operation(operation_id: impl Into<String>, set: &ServiceSet) -> Result<Operation> {
    validate_service_set(set)?;
    let expanded = expand_set(set);
    let mut steps = Vec::new();
    for service_id in &set.operations.install_order {
        steps.push(serde_json::json!({
            "action": "ensure_service",
            "target": service_id,
        }));
    }
    for endpoint in &set.default_endpoints {
        steps.push(serde_json::json!({
            "action": "declare_endpoint",
            "target": format!("0.0.0.0:{}", endpoint.port),
            "service_id": endpoint.service,
            "protocol": endpoint.protocol,
        }));
    }
    for link in &set.default_links {
        steps.push(serde_json::json!({
            "action": "declare_link",
            "target": format!("{} -> {}", link.from, link.to),
            "protocol": link.protocol,
            "auth_mode": link.auth_mode,
            "scope": link.scope,
        }));
    }
    ensure(!steps.is_empty(), "set apply plan must contain steps")?;

    plan_operation(
        operation_id,
        "set.apply",
        "Set",
        set.id.clone(),
        serde_json::json!({
            "set_id": set.id,
            "set_manifest": set,
            "services": expanded.services,
            "default_links": expanded.default_links,
        }),
        serde_json::json!({
            "steps": steps,
            "requires_confirmation": true
        }),
        serde_json::json!({
            "steps": [
                {
                    "action": "restore_previous_topology",
                    "target": set.id
                }
            ]
        }),
    )
}

pub fn endpoint_register_operation(
    operation_id: impl Into<String>,
    endpoint: &Endpoint,
) -> Result<Operation> {
    validate_endpoint(endpoint)?;
    plan_operation(
        operation_id,
        "endpoint.register",
        "Endpoint",
        endpoint.endpoint.clone(),
        serde_json::json!({
            "endpoint": endpoint.endpoint,
            "service_id": endpoint.service_id,
            "protocol": endpoint.protocol,
            "health_path": endpoint.health_path,
            "display_name": endpoint.display_name,
            "note": endpoint.note,
        }),
        serde_json::json!({
            "steps": [
                {
                    "action": "upsert_endpoint",
                    "target": endpoint.endpoint,
                    "service_id": endpoint.service_id
                }
            ],
            "requires_confirmation": false
        }),
        serde_json::json!({
            "steps": [
                {
                    "action": "remove_endpoint",
                    "target": endpoint.endpoint
                }
            ]
        }),
    )
}

pub fn endpoint_update_operation(
    operation_id: impl Into<String>,
    endpoint: &Endpoint,
) -> Result<Operation> {
    validate_endpoint(endpoint)?;
    plan_operation(
        operation_id,
        "endpoint.update",
        "Endpoint",
        endpoint.endpoint.clone(),
        serde_json::json!({
            "endpoint": endpoint.endpoint,
            "service_id": endpoint.service_id,
            "protocol": endpoint.protocol,
            "health_path": endpoint.health_path,
            "display_name": endpoint.display_name,
            "note": endpoint.note,
        }),
        serde_json::json!({
            "steps": [
                {
                    "action": "update_endpoint",
                    "target": endpoint.endpoint
                }
            ],
            "requires_confirmation": true
        }),
        serde_json::json!({
            "steps": [
                {
                    "action": "restore_previous_endpoint",
                    "target": endpoint.endpoint
                }
            ]
        }),
    )
}

pub fn endpoint_delete_operation(
    operation_id: impl Into<String>,
    endpoint: impl AsRef<str>,
) -> Result<Operation> {
    let endpoint = endpoint.as_ref().trim();
    validate_endpoint_id(endpoint)?;
    plan_operation(
        operation_id,
        "endpoint.delete",
        "Endpoint",
        endpoint,
        serde_json::json!({
            "endpoint": endpoint,
        }),
        serde_json::json!({
            "steps": [
                {
                    "action": "delete_endpoint",
                    "target": endpoint
                },
                {
                    "action": "delete_links_for_endpoint",
                    "target": endpoint
                }
            ],
            "requires_confirmation": true
        }),
        serde_json::json!({
            "steps": [
                {
                    "action": "restore_previous_endpoint_and_links",
                    "target": endpoint
                }
            ]
        }),
    )
}

pub fn endpoint_health_check_operation(
    operation_id: impl Into<String>,
    endpoint: impl AsRef<str>,
) -> Result<Operation> {
    let endpoint = endpoint.as_ref().trim();
    validate_endpoint_id(endpoint)?;
    plan_operation(
        operation_id,
        "endpoint.health.check",
        "Endpoint",
        endpoint,
        serde_json::json!({
            "endpoint": endpoint,
        }),
        serde_json::json!({
            "steps": [
                {
                    "action": "probe_endpoint_health",
                    "target": endpoint
                }
            ],
            "requires_confirmation": false
        }),
        serde_json::json!({
            "steps": []
        }),
    )
}

pub fn link_create_operation(
    operation_id: impl Into<String>,
    link: &Link,
    endpoints: &[Endpoint],
) -> Result<Operation> {
    validate_link(link, endpoints)?;
    let target_id = link_operation_target(link);
    plan_operation(
        operation_id,
        "link.create",
        "Link",
        target_id.clone(),
        serde_json::json!({
            "source_endpoint": link.source_endpoint,
            "target_endpoint": link.target_endpoint,
            "protocol": link.protocol,
            "auth_mode": link.auth_mode,
            "scope": link.scope,
            "config_ref": link.config_ref,
            "secret_ref": link.secret_ref,
        }),
        serde_json::json!({
            "steps": [
                {
                    "action": "upsert_link",
                    "target": target_id
                },
                {
                    "action": "deliver_link_config_to_source_endpoint",
                    "target": link.source_endpoint
                }
            ],
            "requires_confirmation": true
        }),
        serde_json::json!({
            "steps": [
                {
                    "action": "remove_link",
                    "target": link_operation_target(link)
                }
            ]
        }),
    )
}

pub fn link_update_operation(
    operation_id: impl Into<String>,
    link: &Link,
    endpoints: &[Endpoint],
) -> Result<Operation> {
    validate_link(link, endpoints)?;
    let target_id = link_operation_target(link);
    plan_operation(
        operation_id,
        "link.update",
        "Link",
        target_id.clone(),
        serde_json::json!({
            "source_endpoint": link.source_endpoint,
            "target_endpoint": link.target_endpoint,
            "protocol": link.protocol,
            "auth_mode": link.auth_mode,
            "scope": link.scope,
            "config_ref": link.config_ref,
            "secret_ref": link.secret_ref,
        }),
        serde_json::json!({
            "steps": [
                {
                    "action": "update_link",
                    "target": target_id
                },
                {
                    "action": "deliver_link_config_to_source_endpoint",
                    "target": link.source_endpoint
                }
            ],
            "requires_confirmation": true
        }),
        serde_json::json!({
            "steps": [
                {
                    "action": "restore_previous_link",
                    "target": link_operation_target(link)
                }
            ]
        }),
    )
}

pub fn link_delete_operation(operation_id: impl Into<String>, link: &Link) -> Result<Operation> {
    validate_endpoint_id(&link.source_endpoint)?;
    validate_endpoint_id(&link.target_endpoint)?;
    let target_id = link_operation_target(link);
    plan_operation(
        operation_id,
        "link.delete",
        "Link",
        target_id.clone(),
        serde_json::json!({
            "source_endpoint": link.source_endpoint,
            "target_endpoint": link.target_endpoint,
        }),
        serde_json::json!({
            "steps": [
                {
                    "action": "delete_link",
                    "target": target_id
                },
                {
                    "action": "remove_link_config_from_source_endpoint",
                    "target": link.source_endpoint
                }
            ],
            "requires_confirmation": true
        }),
        serde_json::json!({
            "steps": [
                {
                    "action": "restore_previous_link",
                    "target": link_operation_target(link)
                }
            ]
        }),
    )
}

pub fn link_health_check_operation(
    operation_id: impl Into<String>,
    link: &Link,
) -> Result<Operation> {
    validate_endpoint_id(&link.source_endpoint)?;
    validate_endpoint_id(&link.target_endpoint)?;
    let target_id = link_operation_target(link);
    plan_operation(
        operation_id,
        "link.health.check",
        "Link",
        target_id.clone(),
        serde_json::json!({
            "source_endpoint": link.source_endpoint,
            "target_endpoint": link.target_endpoint,
        }),
        serde_json::json!({
            "steps": [
                {
                    "action": "probe_link_health",
                    "target": target_id
                }
            ],
            "requires_confirmation": false
        }),
        serde_json::json!({
            "steps": []
        }),
    )
}

pub fn service_health_check_operation(
    operation_id: impl Into<String>,
    service_id: impl AsRef<str>,
    endpoint: Option<&str>,
) -> Result<Operation> {
    let service_id = service_id.as_ref().trim();
    ensure(!service_id.is_empty(), "service_id is required")?;
    if let Some(endpoint) = endpoint {
        validate_endpoint_id(endpoint)?;
    }
    plan_operation(
        operation_id,
        "service.health.check",
        "Service",
        service_id,
        serde_json::json!({
            "service_id": service_id,
            "endpoint": endpoint.unwrap_or(""),
        }),
        serde_json::json!({
            "steps": [
                {
                    "action": "probe_service_health",
                    "target": service_id
                }
            ],
            "requires_confirmation": false
        }),
        serde_json::json!({
            "steps": []
        }),
    )
}

pub fn service_logs_view_operation(
    operation_id: impl Into<String>,
    service_id: impl AsRef<str>,
    endpoint: Option<&str>,
) -> Result<Operation> {
    let service_id = service_id.as_ref().trim();
    ensure(!service_id.is_empty(), "service_id is required")?;
    if let Some(endpoint) = endpoint {
        validate_endpoint_id(endpoint)?;
    }
    plan_operation(
        operation_id,
        "service.logs.view",
        "LogView",
        service_id,
        serde_json::json!({
            "service_id": service_id,
            "endpoint": endpoint.unwrap_or(""),
        }),
        serde_json::json!({
            "steps": [
                {
                    "action": "open_log_view",
                    "target": service_id
                }
            ],
            "requires_confirmation": false
        }),
        serde_json::json!({
            "steps": []
        }),
    )
}

pub fn operation_logs_view_operation(
    operation_id: impl Into<String>,
    target_operation_id: impl AsRef<str>,
) -> Result<Operation> {
    let target_operation_id = target_operation_id.as_ref().trim();
    ensure(!target_operation_id.is_empty(), "operation_id is required")?;
    plan_operation(
        operation_id,
        "operation.logs.view",
        "LogView",
        target_operation_id,
        serde_json::json!({
            "operation_id": target_operation_id,
        }),
        serde_json::json!({
            "steps": [
                {
                    "action": "open_operation_log_view",
                    "target": target_operation_id
                }
            ],
            "requires_confirmation": false
        }),
        serde_json::json!({
            "steps": []
        }),
    )
}

pub fn diagnostics_export_operation(
    operation_id: impl Into<String>,
    report_id: impl AsRef<str>,
    format: impl AsRef<str>,
) -> Result<Operation> {
    let report_id = report_id.as_ref().trim();
    let format = format.as_ref().trim();
    ensure(!report_id.is_empty(), "report_id is required")?;
    ensure(
        matches!(format, "json" | "markdown"),
        "diagnostic export format must be json or markdown",
    )?;
    plan_operation(
        operation_id,
        "diagnostics.export",
        "DiagnosticReport",
        report_id,
        serde_json::json!({
            "report_id": report_id,
            "format": format,
        }),
        serde_json::json!({
            "steps": [
                {
                    "action": "export_diagnostic_report",
                    "target": report_id,
                    "format": format
                }
            ],
            "requires_confirmation": false
        }),
        serde_json::json!({
            "steps": []
        }),
    )
}

pub fn topology_apply_operation(
    operation_id: impl Into<String>,
    topology: &Topology,
) -> Result<Operation> {
    validate_topology(topology)?;
    plan_operation(
        operation_id,
        "topology.apply",
        "Topology",
        topology.root_endpoint.clone(),
        serde_json::json!({
            "root_host": topology.root_host,
            "root_endpoint": topology.root_endpoint,
            "services": topology.services,
            "sets": topology.sets,
            "endpoints": topology.endpoints.len(),
            "links": topology.links.len(),
            "topology_snapshot": topology,
        }),
        serde_json::json!({
            "steps": [
                {
                    "action": "validate_topology",
                    "target": topology.root_endpoint
                },
                {
                    "action": "persist_topology_snapshot",
                    "target": topology.root_endpoint
                },
                {
                    "action": "refresh_log_and_diagnostic_views",
                    "target": topology.root_endpoint
                }
            ],
            "requires_confirmation": true
        }),
        serde_json::json!({
            "steps": [
                {
                    "action": "restore_previous_topology_snapshot",
                    "target": topology.root_endpoint
                }
            ]
        }),
    )
}

pub fn expand_set(set: &ServiceSet) -> SetExpandResult {
    SetExpandResult {
        set_id: set.id.clone(),
        services: set
            .services
            .iter()
            .map(|item| item.id().to_string())
            .collect(),
        default_links: set.default_links.clone(),
    }
}

fn link_operation_target(link: &Link) -> String {
    format!("{} -> {}", link.source_endpoint, link.target_endpoint)
}

fn validate_link_requirements(items: &[RequiredLinkDecl], key_re: &Regex) -> Result<()> {
    unique_by(
        items.iter().map(|item| item.id.as_str()),
        "duplicate required link",
    )?;
    for item in items {
        ensure(key_re.is_match(&item.id), "required link id is invalid")?;
        ensure(
            is_supported_protocol(&item.protocol),
            "required link protocol is invalid",
        )?;
    }
    Ok(())
}

fn discover_service_ids(repo_root: &Path) -> Result<HashSet<String>> {
    let services_dir = repo_root.join("services");
    let mut service_ids = HashSet::new();
    if !services_dir.is_dir() {
        return Err(OrchestratorError::UnsafePath(
            "services directory is not available".to_string(),
        ));
    }
    for entry in fs::read_dir(&services_dir)? {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let rel = Path::new("services")
            .join(entry.file_name())
            .join("service.yaml");
        if !repo_root.join(&rel).is_file() {
            continue;
        }
        let manifest = validate_service_manifest_file(repo_root, &rel)?;
        service_ids.insert(manifest.id);
    }
    Ok(service_ids)
}

fn validate_operation_order(
    items: &[String],
    set_service_ids: &HashSet<&str>,
    field_name: &str,
) -> Result<()> {
    let mut seen = HashSet::new();
    for item in items {
        ensure(
            set_service_ids.contains(item.as_str()),
            &format!("{field_name} references service outside set"),
        )?;
        if !seen.insert(item.as_str()) {
            return Err(OrchestratorError::InvalidManifest(format!(
                "{field_name} contains duplicate service"
            )));
        }
    }
    Ok(())
}

fn validate_service_manifest_path(repo_root: &Path, manifest_path: &Path) -> Result<()> {
    ensure(
        !manifest_path.is_absolute(),
        "service manifest path must be relative",
    )?;
    reject_path_components(manifest_path)?;
    ensure(
        manifest_path.file_name().and_then(|v| v.to_str()) == Some("service.yaml"),
        "service manifest file must be service.yaml",
    )?;
    let services_dir = repo_root.join("services");
    let full = repo_root.join(manifest_path);
    let canonical_services = services_dir.canonicalize().map_err(|_| {
        OrchestratorError::UnsafePath("services directory is not available".to_string())
    })?;
    let canonical_manifest = full
        .canonicalize()
        .map_err(|_| OrchestratorError::UnsafePath(sanitize_path_for_error(manifest_path)))?;
    ensure(
        canonical_manifest.starts_with(&canonical_services),
        "service manifest must stay under services",
    )
}

fn validate_set_path(repo_root: &Path, set_path: &Path) -> Result<()> {
    ensure(!set_path.is_absolute(), "set path must be relative")?;
    reject_path_components(set_path)?;
    let sets_dir = repo_root.join("sets");
    let full = repo_root.join(set_path);
    let canonical_sets = sets_dir.canonicalize().map_err(|_| {
        OrchestratorError::UnsafePath("sets directory is not available".to_string())
    })?;
    let canonical_set = full
        .canonicalize()
        .map_err(|_| OrchestratorError::UnsafePath(sanitize_path_for_error(set_path)))?;
    ensure(
        canonical_set.starts_with(&canonical_sets),
        "set file must stay under sets",
    )
}

fn reject_path_components(path: &Path) -> Result<()> {
    let banned = [".tmp", ".env", "node_modules", "dist", "target", ".git"];
    for component in path.components() {
        match component {
            Component::ParentDir => {
                return Err(OrchestratorError::UnsafePath(
                    "path traversal is not allowed".to_string(),
                ));
            }
            Component::RootDir | Component::Prefix(_) => {
                return Err(OrchestratorError::UnsafePath(
                    "absolute path is not allowed".to_string(),
                ));
            }
            Component::Normal(value) => {
                let text = value.to_string_lossy();
                if banned.iter().any(|item| text.eq_ignore_ascii_case(item)) {
                    return Err(OrchestratorError::UnsafePath(format!(
                        "banned path segment {}",
                        text
                    )));
                }
            }
            Component::CurDir => {}
        }
    }
    Ok(())
}

fn reject_dangerous_service_values(value: &Value) -> Result<()> {
    let banned = [
        "secret",
        "token",
        "password",
        "private_key",
        "env",
        "command",
        "script",
        "hook",
        "image",
        "mount",
        "host_path",
        "privileged",
        "cap_add",
        "postinstall",
        "preinstall",
    ];
    match value {
        Value::Object(map) => {
            for (key, child) in map {
                let lower = key.to_ascii_lowercase();
                if lower == "secret_ref"
                    || lower == "config_schema"
                    || lower == "required_secrets"
                    || lower == "secrets"
                {
                    reject_dangerous_service_values(child)?;
                    continue;
                }
                if banned.iter().any(|item| lower == *item) {
                    return Err(OrchestratorError::InvalidManifest(format!(
                        "dangerous field {} is not allowed in service.yaml",
                        key
                    )));
                }
                reject_dangerous_service_values(child)?;
            }
        }
        Value::Array(items) => {
            for item in items {
                reject_dangerous_service_values(item)?;
            }
        }
        _ => {}
    }
    Ok(())
}

fn is_supported_protocol(value: &str) -> bool {
    matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "http" | "https" | "tcp" | "postgres" | "redis"
    )
}

fn is_supported_service_kind(value: &str) -> bool {
    matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "frontend"
            | "backend-api"
            | "backend-worker"
            | "gateway"
            | "database"
            | "cache"
            | "storage"
            | "external"
            | "agent"
    )
}

fn is_supported_source_type(value: &str) -> bool {
    matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "local" | "git" | "github" | "release" | "external"
    )
}

fn unique_by<'a>(items: impl Iterator<Item = &'a str>, msg: &str) -> Result<()> {
    let mut seen = HashSet::new();
    for item in items {
        let key = item.trim();
        ensure(!key.is_empty(), msg)?;
        if !seen.insert(key.to_string()) {
            return Err(OrchestratorError::InvalidManifest(msg.to_string()));
        }
    }
    Ok(())
}

fn default_schema_version() -> u32 {
    1
}

fn default_true() -> bool {
    true
}

fn default_count() -> u32 {
    1
}

fn ensure(ok: bool, msg: &str) -> Result<()> {
    if ok {
        Ok(())
    } else {
        Err(OrchestratorError::InvalidManifest(msg.to_string()))
    }
}
