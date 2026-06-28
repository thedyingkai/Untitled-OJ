use crate::{InstallerError, Result, sanitize_path_for_error};
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
    pub endpoint: EndpointDecl,
    pub runtime: ServiceRuntimeDecl,
    #[serde(default)]
    pub config_schema: Value,
    #[serde(default)]
    pub requires: ServiceRequires,
    #[serde(default)]
    pub provides: ServiceProvides,
    #[serde(default)]
    pub ui: ServiceUiDecl,
    #[serde(default)]
    pub permissions: Vec<String>,
    #[serde(default)]
    pub security: ServiceSecurityDecl,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct EndpointDecl {
    pub protocol: String,
    pub default_port: u16,
    #[serde(default)]
    pub health_path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ServiceRuntimeDecl {
    pub mode: RuntimeMode,
    #[serde(default)]
    pub root_allowed: bool,
    #[serde(default)]
    pub non_root_allowed: bool,
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
    pub links: Vec<RequiredLinkDecl>,
    #[serde(default)]
    pub optional_links: Vec<RequiredLinkDecl>,
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
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ServiceUiDecl {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub menu_scope: String,
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
}

impl Default for ServiceSecurityDecl {
    fn default() -> Self {
        Self {
            allow_privileged: false,
            allow_host_mount: false,
            allow_arbitrary_command: false,
        }
    }
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
    pub services: Vec<String>,
    #[serde(default)]
    pub default_links: Vec<ServiceSetLink>,
    #[serde(default)]
    pub recommended_for: Vec<String>,
    #[serde(default)]
    pub non_root_only: bool,
    #[serde(default)]
    pub required_root_links: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ServiceSetLink {
    pub from: String,
    pub to: String,
    #[serde(default)]
    pub protocol: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SetExpandResult {
    pub set_id: String,
    pub services: Vec<String>,
    pub default_links: Vec<ServiceSetLink>,
    pub non_root_only: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ServiceState {
    pub service_id: String,
    pub version: String,
    pub enabled: bool,
    pub endpoint: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ServicePlan {
    pub kind: ServicePlanKind,
    pub service_id: String,
    pub version: String,
    pub can_apply: bool,
    #[serde(default)]
    pub actions: Vec<ServicePlanAction>,
    #[serde(default)]
    pub blocked_by: Vec<String>,
    #[serde(default)]
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ServicePlanKind {
    Install,
    Enable,
    Disable,
    Delete,
    Hotplug,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ServicePlanAction {
    pub action: String,
    pub target: String,
    #[serde(default)]
    pub detail: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum DeviceKind {
    Root,
    NonRoot,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LinkEndpointRef {
    pub endpoint: String,
    pub service_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LinkDecl {
    pub source: LinkEndpointRef,
    pub target: LinkEndpointRef,
    #[serde(default)]
    pub protocol: String,
    #[serde(default)]
    pub auth_mode: String,
    #[serde(default)]
    pub scope: String,
    #[serde(default)]
    pub secret_ref: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TopologySnapshot {
    #[serde(default)]
    pub devices: Vec<String>,
    #[serde(default)]
    pub services: Vec<String>,
    #[serde(default)]
    pub endpoints: Vec<String>,
    #[serde(default)]
    pub links: Vec<LinkDecl>,
    #[serde(default)]
    pub sets: Vec<String>,
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
        InstallerError::InvalidManifest("service version must be semver".to_string())
    })?;
    ensure(!manifest.kind.trim().is_empty(), "service kind is required")?;
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
        "service must allow root or non-root runtime",
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
    Ok(set)
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
        set.services.iter().map(String::as_str),
        "duplicate set service",
    )?;
    for service in &set.services {
        ensure(id_re.is_match(service), "set service id is invalid")?;
    }
    for link in &set.default_links {
        ensure(
            set.services.contains(&link.from),
            "default link source is not in set",
        )?;
        ensure(
            set.services.contains(&link.to),
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
        return Err(InstallerError::InvalidManifest(
            "endpoint must be IP:Port".to_string(),
        ));
    };
    host.parse::<IpAddr>()
        .map_err(|_| InstallerError::InvalidManifest("endpoint IP is invalid".to_string()))?;
    let port = port
        .parse::<u16>()
        .map_err(|_| InstallerError::InvalidManifest("endpoint port is invalid".to_string()))?;
    ensure(port > 0, "endpoint port is invalid")
}

pub fn service_install_plan(manifest: &ServiceManifest, installed: &[ServiceState]) -> ServicePlan {
    let exists = installed.iter().any(|item| item.service_id == manifest.id);
    let actions = if exists {
        vec![ServicePlanAction {
            action: "refresh_service_metadata".to_string(),
            target: manifest.id.clone(),
            detail: "refresh Service Registry metadata".to_string(),
        }]
    } else {
        vec![
            ServicePlanAction {
                action: "insert_service".to_string(),
                target: manifest.id.clone(),
                detail: "insert Service Registry row".to_string(),
            },
            ServicePlanAction {
                action: "declare_default_endpoint".to_string(),
                target: format!("*:{}", manifest.endpoint.default_port),
                detail: "Root Runtime Manager binds the actual IP:Port".to_string(),
            },
        ]
    };
    ServicePlan {
        kind: ServicePlanKind::Install,
        service_id: manifest.id.clone(),
        version: manifest.version.clone(),
        can_apply: true,
        actions,
        blocked_by: vec![],
        warnings: vec![],
    }
}

pub fn expand_set(set: &ServiceSet) -> SetExpandResult {
    SetExpandResult {
        set_id: set.id.clone(),
        services: set.services.clone(),
        default_links: set.default_links.clone(),
        non_root_only: set.non_root_only,
    }
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
        InstallerError::UnsafePath("services directory is not available".to_string())
    })?;
    let canonical_manifest = full
        .canonicalize()
        .map_err(|_| InstallerError::UnsafePath(sanitize_path_for_error(manifest_path)))?;
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
    let canonical_sets = sets_dir
        .canonicalize()
        .map_err(|_| InstallerError::UnsafePath("sets directory is not available".to_string()))?;
    let canonical_set = full
        .canonicalize()
        .map_err(|_| InstallerError::UnsafePath(sanitize_path_for_error(set_path)))?;
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
                return Err(InstallerError::UnsafePath(
                    "path traversal is not allowed".to_string(),
                ));
            }
            Component::RootDir | Component::Prefix(_) => {
                return Err(InstallerError::UnsafePath(
                    "absolute path is not allowed".to_string(),
                ));
            }
            Component::Normal(value) => {
                let text = value.to_string_lossy();
                if banned.iter().any(|item| text.eq_ignore_ascii_case(item)) {
                    return Err(InstallerError::UnsafePath(format!(
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
                if lower == "secret_ref" || lower == "config_schema" {
                    reject_dangerous_service_values(child)?;
                    continue;
                }
                if banned.iter().any(|item| lower == *item) {
                    return Err(InstallerError::InvalidManifest(format!(
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
        "http" | "https" | "tcp" | "postgres"
    )
}

fn unique_by<'a>(items: impl Iterator<Item = &'a str>, msg: &str) -> Result<()> {
    let mut seen = HashSet::new();
    for item in items {
        let key = item.trim();
        ensure(!key.is_empty(), msg)?;
        if !seen.insert(key.to_string()) {
            return Err(InstallerError::InvalidManifest(msg.to_string()));
        }
    }
    Ok(())
}

fn default_schema_version() -> u32 {
    1
}

fn ensure(ok: bool, msg: &str) -> Result<()> {
    if ok {
        Ok(())
    } else {
        Err(InstallerError::InvalidManifest(msg.to_string()))
    }
}
