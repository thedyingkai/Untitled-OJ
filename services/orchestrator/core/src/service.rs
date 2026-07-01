use crate::{
    Endpoint, Link, Operation, OperationStatus, OrchestratorError, Result, Topology,
    plan_operation, sanitize_path_for_error, validate_endpoint, validate_link, validate_topology,
};
use regex::Regex;
use semver::Version;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, HashSet};
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
pub struct ServiceReleaseManifest {
    #[serde(default = "default_schema_version")]
    pub schema_version: u32,
    pub service_name: String,
    pub version: String,
    pub description: String,
    pub service_type: String,
    pub source: ReleaseSourceDecl,
    pub runtime: ReleaseRuntimeDecl,
    #[serde(default)]
    pub frontend: ReleaseFrontendDecl,
    pub backend: ReleaseBackendDecl,
    #[serde(default)]
    pub migrations: Vec<ReleaseMigrationDecl>,
    #[serde(default)]
    pub permissions: Vec<String>,
    #[serde(default)]
    pub routes: Vec<ReleaseRouteDecl>,
    #[serde(default)]
    pub apis: Vec<ReleaseApiSurfaceDecl>,
    #[serde(default)]
    pub redis: Vec<ReleaseRedisDecl>,
    #[serde(default)]
    pub storage: Vec<ReleaseStorageDecl>,
    #[serde(default)]
    pub dependencies: Vec<String>,
    #[serde(default)]
    pub required_apis: Vec<String>,
    #[serde(default)]
    pub service_identity: ReleaseServiceIdentityDecl,
    #[serde(default)]
    pub config_schema: Value,
    #[serde(default)]
    pub secrets: Vec<String>,
    #[serde(default)]
    pub observability: ReleaseObservabilityDecl,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ReleaseSourceDecl {
    pub kind: String,
    pub url: String,
    #[serde(default)]
    pub checksum: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ReleaseRuntimeDecl {
    pub kind: String,
    #[serde(default)]
    pub image: String,
    #[serde(default)]
    pub binary: String,
    #[serde(default)]
    pub system_service: String,
    #[serde(default)]
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub working_dir: String,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ReleaseFrontendDecl {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub route_prefix: String,
    #[serde(default)]
    pub remote_entry: String,
    #[serde(default)]
    pub menu_items: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ReleaseBackendDecl {
    pub protocol: String,
    pub port: u16,
    #[serde(default)]
    pub health_path: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ReleaseMigrationDecl {
    pub version: String,
    pub path: String,
    #[serde(default)]
    pub checksum: String,
    #[serde(default)]
    pub destructive: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ReleaseRouteDecl {
    pub path: String,
    #[serde(default)]
    pub method: String,
    pub target_type: String,
    pub target: String,
    #[serde(default)]
    pub permission: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ReleaseApiSurfaceDecl {
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
    pub version: String,
    #[serde(default)]
    pub grpc_service: String,
    #[serde(default)]
    pub stream_name: String,
    #[serde(default)]
    pub rate_limit: String,
    #[serde(default)]
    pub timeout: String,
    #[serde(default)]
    pub allowed_callers: Vec<String>,
    #[serde(default)]
    pub denied_callers: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ReleaseRedisDecl {
    pub name: String,
    pub kind: String,
    pub usage: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ReleaseStorageDecl {
    pub object_type: String,
    pub bucket: String,
    #[serde(default)]
    pub path_prefix: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ReleaseServiceIdentityDecl {
    #[serde(default)]
    pub service_name: String,
    #[serde(default)]
    pub allowed_apis: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ReleaseObservabilityDecl {
    #[serde(default)]
    pub metrics: bool,
    #[serde(default)]
    pub jaeger: bool,
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
pub struct DeploymentTemplate {
    #[serde(default = "default_schema_version")]
    pub schema_version: u32,
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub scenario: Value,
    #[serde(default)]
    pub services: Vec<DeploymentTemplateService>,
    #[serde(default)]
    pub default_endpoints: Vec<DeploymentTemplateEndpoint>,
    #[serde(default)]
    pub default_links: Vec<DeploymentTemplateLink>,
    #[serde(default)]
    pub policies: Value,
    #[serde(default)]
    pub operations: DeploymentTemplateOperations,
    #[serde(default)]
    pub notes: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(untagged)]
pub enum DeploymentTemplateService {
    Id(String),
    Spec(DeploymentTemplateServiceSpec),
}

impl DeploymentTemplateService {
    pub fn id(&self) -> &str {
        match self {
            Self::Id(value) => value,
            Self::Spec(value) => &value.id,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct DeploymentTemplateServiceSpec {
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
pub struct DeploymentTemplateEndpoint {
    pub service: String,
    pub port: u16,
    pub protocol: String,
    #[serde(default)]
    pub expose: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct DeploymentTemplateLink {
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
pub struct DeploymentTemplateOperations {
    #[serde(default)]
    pub install_order: Vec<String>,
    #[serde(default)]
    pub start_order: Vec<String>,
    #[serde(default)]
    pub stop_order: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DeploymentTemplatePreview {
    pub template_id: String,
    pub services: Vec<String>,
    pub default_links: Vec<DeploymentTemplateLink>,
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

pub fn validate_service_release_file(
    repo_root: &Path,
    release_path: &Path,
) -> Result<ServiceReleaseManifest> {
    validate_release_path(repo_root, release_path)?;
    let text = fs::read_to_string(repo_root.join(release_path))?;
    let release: ServiceReleaseManifest = serde_yaml::from_str(&text)?;
    validate_service_release(&release)?;
    let service_path = release_path
        .parent()
        .unwrap_or_else(|| Path::new("services"))
        .join("service.yaml");
    if repo_root.join(&service_path).is_file() {
        let service = validate_service_manifest_file(repo_root, &service_path)?;
        ensure(
            service.id == release.service_name,
            "release service_name must match service.yaml id",
        )?;
        ensure(
            service.version == release.version,
            "release version must match service.yaml version",
        )?;
        ensure(
            service.kind == release.service_type,
            "release service_type must match service.yaml kind",
        )?;
        ensure(
            service.endpoint.protocol == release.backend.protocol,
            "release backend protocol must match service endpoint protocol",
        )?;
        ensure(
            service.endpoint.default_port == release.backend.port,
            "release backend port must match service endpoint default_port",
        )?;
        ensure(
            service.endpoint.health_path == release.backend.health_path,
            "release backend health_path must match service endpoint health_path",
        )?;
        for permission in &service.permissions {
            ensure(
                release.permissions.iter().any(|item| item == permission),
                "release permissions must cover service.yaml permissions",
            )?;
        }
        for permission in &service.ui.permissions {
            ensure(
                release.permissions.iter().any(|item| item == permission),
                "release permissions must cover service.yaml ui permissions",
            )?;
        }
        ensure(
            release.frontend.enabled == service.ui.enabled,
            "release frontend enabled must match service.yaml ui enabled",
        )?;
        if service.ui.enabled {
            for route in &service.ui.routes {
                ensure(
                    release.frontend.route_prefix == *route,
                    "release frontend route_prefix must cover service.yaml ui routes",
                )?;
            }
        }
        for bucket in &service.provides.storage_buckets {
            ensure(
                release.storage.iter().any(|item| item.bucket == *bucket),
                "release storage must cover service.yaml storage_buckets",
            )?;
        }
        for dependency in &service.requires.services {
            ensure(
                release.dependencies.iter().any(|item| item == dependency),
                "release dependencies must cover service.yaml requires.services",
            )?;
        }
        for queue in &service.requires.queue {
            ensure(
                release.redis.iter().any(|item| item.name == *queue),
                "release redis must cover service.yaml requires.queue",
            )?;
        }
        for secret in service
            .requires
            .secrets
            .iter()
            .chain(service.security.required_secrets.iter())
        {
            ensure(
                release.secrets.iter().any(|item| item == secret),
                "release secrets must cover service.yaml secrets",
            )?;
        }
        for route in service
            .endpoint
            .routes
            .iter()
            .chain(service.provides.routes.iter())
        {
            ensure(
                release.routes.iter().any(|release_route| {
                    release_route_covers_service_route(&release_route.path, route)
                }),
                "release routes must cover service.yaml routes",
            )?;
        }
    }
    Ok(release)
}

fn release_route_covers_service_route(release_route: &str, service_route: &str) -> bool {
    let release_base = release_route
        .trim_end_matches("/**")
        .trim_end_matches("/*")
        .trim_end_matches('*')
        .trim_end_matches('/');
    let service_base = service_route.trim_end_matches('/');
    release_base == service_base || service_base.starts_with(&format!("{release_base}/"))
}

pub fn validate_service_release(release: &ServiceReleaseManifest) -> Result<()> {
    let id_re = Regex::new(r"^[a-z0-9][a-z0-9-]*$").expect("valid regex");
    let key_re = Regex::new(r"^[a-z0-9][a-z0-9_.:-]*$").expect("valid regex");
    let path_re = Regex::new(r"^/[A-Za-z0-9_./:{}*-]*$").expect("valid regex");
    ensure(
        release.schema_version == SERVICE_SCHEMA_VERSION,
        "unsupported release schema_version",
    )?;
    ensure(
        id_re.is_match(&release.service_name),
        "release service_name is invalid",
    )?;
    Version::parse(release.version.trim()).map_err(|_| {
        OrchestratorError::InvalidManifest("release version must be semver".to_string())
    })?;
    ensure(
        is_supported_service_kind(&release.service_type),
        "release service_type is invalid",
    )?;
    ensure(
        is_supported_release_source_kind(&release.source.kind),
        "release source.kind is invalid",
    )?;
    ensure(
        !release.source.url.trim().is_empty(),
        "release source.url is required",
    )?;
    ensure(
        is_supported_release_runtime_kind(&release.runtime.kind),
        "release runtime.kind is invalid",
    )?;
    validate_release_runtime(&release.runtime)?;
    if release.frontend.enabled {
        ensure(
            path_re.is_match(&release.frontend.route_prefix),
            "release frontend route_prefix is invalid",
        )?;
        ensure(
            path_re.is_match(&release.frontend.remote_entry),
            "release frontend remote_entry is invalid",
        )?;
    } else {
        if !release.frontend.route_prefix.trim().is_empty() {
            ensure(
                path_re.is_match(&release.frontend.route_prefix),
                "release frontend route_prefix is invalid",
            )?;
        }
        if !release.frontend.remote_entry.trim().is_empty() {
            ensure(
                path_re.is_match(&release.frontend.remote_entry),
                "release frontend remote_entry is invalid",
            )?;
        }
    }
    for menu_item in &release.frontend.menu_items {
        ensure(
            key_re.is_match(menu_item),
            "release frontend menu is invalid",
        )?;
    }
    ensure(
        is_supported_protocol(&release.backend.protocol),
        "release backend protocol is invalid",
    )?;
    ensure(release.backend.port > 0, "release backend port is required")?;
    if !release.backend.health_path.trim().is_empty() {
        ensure(
            path_re.is_match(&release.backend.health_path),
            "release backend health_path is invalid",
        )?;
    }
    unique_by(
        release.dependencies.iter().map(String::as_str),
        "duplicate release dependency",
    )?;
    for dependency in &release.dependencies {
        ensure(id_re.is_match(dependency), "release dependency is invalid")?;
    }
    unique_by(
        release.required_apis.iter().map(String::as_str),
        "duplicate release required api",
    )?;
    for api_id in &release.required_apis {
        ensure(key_re.is_match(api_id), "release required api is invalid")?;
    }
    if !release.service_identity.service_name.trim().is_empty()
        || !release.service_identity.allowed_apis.is_empty()
    {
        ensure(
            release.service_identity.service_name == release.service_name,
            "release service_identity service_name must match service_name",
        )?;
        unique_by(
            release
                .service_identity
                .allowed_apis
                .iter()
                .map(String::as_str),
            "duplicate release service_identity allowed api",
        )?;
        for api_id in &release.service_identity.allowed_apis {
            ensure(
                key_re.is_match(api_id),
                "release service_identity allowed api is invalid",
            )?;
            ensure(
                release
                    .required_apis
                    .iter()
                    .any(|required| required == api_id),
                "release service_identity allowed api must be declared in required_apis",
            )?;
        }
    }
    unique_by(
        release.permissions.iter().map(String::as_str),
        "duplicate release permission",
    )?;
    for permission in &release.permissions {
        ensure(
            key_re.is_match(permission),
            "release permission key is invalid",
        )?;
    }
    unique_by(
        release.secrets.iter().map(String::as_str),
        "duplicate release secret",
    )?;
    for secret in &release.secrets {
        ensure(key_re.is_match(secret), "release secret key is invalid")?;
    }
    unique_by(
        release
            .migrations
            .iter()
            .map(|migration| migration.version.as_str()),
        "duplicate release migration version",
    )?;
    unique_by(
        release
            .migrations
            .iter()
            .map(|migration| migration.path.as_str()),
        "duplicate release migration path",
    )?;
    for migration in &release.migrations {
        ensure(
            !migration.version.trim().is_empty(),
            "release migration version is required",
        )?;
        ensure(
            !migration.path.trim().is_empty(),
            "release migration path is required",
        )?;
        reject_path_components(Path::new(&migration.path))?;
        ensure(
            migration
                .path
                .starts_with(&format!("services/{}/migrations/", release.service_name)),
            "release migration path must stay under its service migrations directory",
        )?;
    }
    let route_keys = release
        .routes
        .iter()
        .map(release_route_key)
        .collect::<Vec<_>>();
    unique_by(
        route_keys.iter().map(String::as_str),
        "duplicate release route",
    )?;
    for route in &release.routes {
        ensure(
            path_re.is_match(&route.path),
            "release route path is invalid",
        )?;
        if !route.method.trim().is_empty() {
            ensure(
                is_supported_route_method(&route.method),
                "release route method is invalid",
            )?;
        }
        ensure(
            matches!(
                route.target_type.as_str(),
                "endpoint" | "endpoint-group" | "frontend"
            ),
            "release route target_type is invalid",
        )?;
        ensure(
            !route.target.trim().is_empty(),
            "release route target is required",
        )?;
        validate_release_route_target(release, route)?;
        if !route.permission.trim().is_empty() && route.permission != "public" {
            ensure(
                key_re.is_match(&route.permission),
                "release route permission is invalid",
            )?;
        }
    }
    unique_by(
        release.apis.iter().map(|api| api.api_id.as_str()),
        "duplicate release api_id",
    )?;
    for api in &release.apis {
        ensure(key_re.is_match(&api.api_id), "release api_id is invalid")?;
        ensure(
            is_supported_protocol(&api.protocol),
            "release api protocol is invalid",
        )?;
        ensure(
            release_api_port_exists(release, &api.port_name),
            "release api port_name does not exist",
        )?;
        if matches!(api.protocol.as_str(), "http" | "https") {
            ensure(
                path_re.is_match(&api.path_prefix),
                "release api path_prefix is invalid",
            )?;
            ensure(!api.methods.is_empty(), "release api methods are required")?;
            for method in &api.methods {
                ensure(
                    is_supported_route_method(method),
                    "release api method is invalid",
                )?;
            }
        }
        ensure(
            matches!(
                api.visibility.as_str(),
                "private"
                    | "same-node"
                    | "descendants"
                    | "children"
                    | "ancestors"
                    | "global"
                    | "explicit"
            ),
            "release api visibility is invalid",
        )?;
        ensure(
            matches!(
                api.auth_mode.as_str(),
                "public" | "user" | "service" | "internal"
            ),
            "release api auth_mode is invalid",
        )?;
        if api.permission != "public" {
            ensure(
                release
                    .permissions
                    .iter()
                    .any(|item| item == &api.permission),
                "release api permission must be declared in permissions",
            )?;
            ensure(
                key_re.is_match(&api.permission),
                "release api permission is invalid",
            )?;
        }
        ensure(
            matches!(
                api.stability.as_str(),
                "stable" | "experimental" | "deprecated"
            ),
            "release api stability is invalid",
        )?;
        ensure(
            !api.version.trim().is_empty(),
            "release api version is required",
        )?;
        for caller in api.allowed_callers.iter().chain(api.denied_callers.iter()) {
            ensure(
                key_re.is_match(caller),
                "release api caller selector is invalid",
            )?;
        }
    }
    unique_by(
        release.redis.iter().map(|redis| redis.name.as_str()),
        "duplicate release redis resource",
    )?;
    for redis in &release.redis {
        ensure(
            key_re.is_match(&redis.name),
            "release redis name is invalid",
        )?;
        ensure(
            matches!(
                redis.kind.as_str(),
                "stream" | "consumer-group" | "pubsub" | "hash" | "string" | "zset" | "lock"
            ),
            "release redis kind is invalid",
        )?;
        ensure(
            !redis.usage.trim().is_empty(),
            "release redis usage is required",
        )?;
    }
    let storage_keys = release
        .storage
        .iter()
        .map(release_storage_key)
        .collect::<Vec<_>>();
    unique_by(
        storage_keys.iter().map(String::as_str),
        "duplicate release storage resource",
    )?;
    for storage in &release.storage {
        ensure(
            key_re.is_match(&storage.object_type),
            "release storage object_type is invalid",
        )?;
        ensure(
            key_re.is_match(&storage.bucket),
            "release storage bucket is invalid",
        )?;
        if !storage.path_prefix.trim().is_empty() {
            ensure(
                path_re.is_match(&storage.path_prefix),
                "release storage path_prefix is invalid",
            )?;
        }
    }
    Ok(())
}

fn release_api_port_exists(release: &ServiceReleaseManifest, port_name: &str) -> bool {
    let port_name = port_name.trim();
    port_name == "default" || port_name == release.backend.protocol
}

fn release_route_key(route: &ReleaseRouteDecl) -> String {
    let method = route.method.trim();
    let method = if method.is_empty() { "ANY" } else { method };
    format!("{} {}", method.to_ascii_uppercase(), route.path.trim())
}

fn release_storage_key(storage: &ReleaseStorageDecl) -> String {
    format!("{}:{}", storage.object_type.trim(), storage.bucket.trim())
}

fn validate_release_route_target(
    release: &ServiceReleaseManifest,
    route: &ReleaseRouteDecl,
) -> Result<()> {
    match route.target_type.as_str() {
        "endpoint" => {
            validate_endpoint_id(&route.target)?;
            let identity = parse_endpoint_id(&route.target)?;
            ensure(
                identity.service_name == release.service_name.as_str(),
                "release endpoint route target must match service_name",
            )
        }
        "endpoint-group" => {
            let expected_group = format!("{}[*]", release.service_name);
            ensure(
                route.target == expected_group,
                "release endpoint-group route target must be service-name[*]",
            )
        }
        "frontend" => {
            ensure(
                release.frontend.enabled,
                "release frontend route requires frontend enabled",
            )?;
            ensure(
                route.target.as_str() == release.service_name.as_str()
                    || route.target.as_str() == release.frontend.remote_entry.as_str(),
                "release frontend route target is invalid",
            )
        }
        _ => Ok(()),
    }
}

pub fn validate_service_manifest(manifest: &ServiceManifest) -> Result<()> {
    let id_re = Regex::new(r"^[a-z0-9][a-z0-9-]*$").expect("valid regex");
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
    ensure(
        manifest.provides.endpoints.is_empty(),
        "provides.endpoints must be empty; runtime endpoints are identified by ip:port:service-name",
    )?;
    validate_link_requirements(&manifest.requires.links, &key_re)?;
    validate_link_requirements(&manifest.requires.optional_links, &key_re)?;
    Ok(())
}

pub fn validate_deployment_template_file(
    repo_root: &Path,
    set_path: &Path,
) -> Result<DeploymentTemplate> {
    validate_set_path(repo_root, set_path)?;
    let text = fs::read_to_string(repo_root.join(set_path))?;
    let set: DeploymentTemplate = serde_yaml::from_str(&text)?;
    validate_deployment_template(&set)?;
    validate_deployment_template_references(repo_root, &set)?;
    Ok(set)
}

pub fn validate_deployment_template_references(
    repo_root: &Path,
    set: &DeploymentTemplate,
) -> Result<()> {
    let service_manifests = discover_service_manifests(repo_root)?;
    let service_ids = service_manifests
        .iter()
        .map(|service| service.id.as_str())
        .collect::<HashSet<_>>();
    let set_service_ids = set
        .services
        .iter()
        .map(DeploymentTemplateService::id)
        .collect::<HashSet<_>>();
    for service in &set.services {
        ensure(
            service_ids.contains(service.id()),
            &format!("set references missing service {}", service.id()),
        )?;
    }
    for endpoint in &set.default_endpoints {
        ensure(
            set_service_ids.contains(endpoint.service.as_str()),
            "default endpoint service is not in set",
        )?;
        ensure(
            service_ids.contains(endpoint.service.as_str()),
            &format!(
                "default endpoint references missing service {}",
                endpoint.service
            ),
        )?;
    }
    for link in &set.default_links {
        ensure(
            service_ids.contains(link.from.as_str()) && service_ids.contains(link.to.as_str()),
            &format!(
                "default link references missing service {} -> {}",
                link.from, link.to
            ),
        )?;
    }
    for service in &set.services {
        let Some(manifest) = service_manifests
            .iter()
            .find(|manifest| manifest.id == service.id())
        else {
            continue;
        };
        for required_link in &manifest.requires.links {
            let target = required_link.id.as_str();
            if set_service_ids.contains(target) {
                ensure(
                    set.default_links.iter().any(|link| {
                        link.from == manifest.id
                            && link.to == target
                            && link_protocol_matches(&link.protocol, &required_link.protocol)
                    }),
                    &format!(
                        "set default_links must cover required link {} -> {}",
                        manifest.id, target
                    ),
                )?;
            } else {
                ensure(
                    set_declares_external_required_link(set, &manifest.id, target),
                    &format!(
                        "set policies.network.required_external_links must cover required link {} -> {}",
                        manifest.id, target
                    ),
                )?;
            }
        }
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

fn link_protocol_matches(set_protocol: &str, required_protocol: &str) -> bool {
    set_protocol.trim().is_empty()
        || required_protocol.trim().is_empty()
        || set_protocol.eq_ignore_ascii_case(required_protocol)
}

fn set_declares_external_required_link(set: &DeploymentTemplate, from: &str, to: &str) -> bool {
    set.policies
        .get("network")
        .and_then(|network| network.get("required_external_links"))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .any(|item| item.trim() == format!("{from} -> {to}"))
}

pub fn validate_deployment_template(set: &DeploymentTemplate) -> Result<()> {
    let id_re = Regex::new(r"^[a-z0-9][a-z0-9-]*$").expect("valid regex");
    ensure(
        set.schema_version == SET_SCHEMA_VERSION,
        "unsupported set schema_version",
    )?;
    ensure(id_re.is_match(&set.id), "set id is invalid")?;
    ensure(!set.name.trim().is_empty(), "set name is required")?;
    ensure(!set.services.is_empty(), "set services must not be empty")?;
    unique_by(
        set.services.iter().map(DeploymentTemplateService::id),
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
    let identity = parse_endpoint_id(value)?;
    identity
        .host
        .parse::<IpAddr>()
        .map_err(|_| OrchestratorError::InvalidManifest("endpoint IP is invalid".to_string()))?;
    let port = identity
        .port
        .parse::<u16>()
        .map_err(|_| OrchestratorError::InvalidManifest("endpoint port is invalid".to_string()))?;
    ensure(port > 0, "endpoint port is invalid")?;
    let service_re = Regex::new(r"^[a-z0-9][a-z0-9-]*$").expect("valid service id regex");
    ensure(
        service_re.is_match(identity.service_name),
        "endpoint service name is invalid",
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EndpointIdentity<'a> {
    pub host: &'a str,
    pub port: &'a str,
    pub service_name: &'a str,
}

pub fn parse_endpoint_id(value: &str) -> Result<EndpointIdentity<'_>> {
    let Some((host_and_port, service_name)) = value.rsplit_once(':') else {
        return Err(OrchestratorError::InvalidManifest(
            "endpoint must be ip:port:service-name".to_string(),
        ));
    };
    let Some((host, port)) = host_and_port.rsplit_once(':') else {
        return Err(OrchestratorError::InvalidManifest(
            "endpoint must be ip:port:service-name".to_string(),
        ));
    };
    if host.trim().is_empty() || port.trim().is_empty() || service_name.trim().is_empty() {
        return Err(OrchestratorError::InvalidManifest(
            "endpoint must be ip:port:service-name".to_string(),
        ));
    }
    Ok(EndpointIdentity {
        host,
        port,
        service_name,
    })
}
pub fn endpoint_socket_addr(value: &str) -> Result<String> {
    let identity = parse_endpoint_id(value)?;
    validate_endpoint_id(value)?;
    let host = identity
        .host
        .parse::<IpAddr>()
        .map_err(|_| OrchestratorError::InvalidManifest("endpoint IP is invalid".to_string()))?;
    let rendered_host = match host {
        IpAddr::V4(_) => identity.host.to_string(),
        IpAddr::V6(_) => format!("[{}]", identity.host),
    };
    Ok(format!("{}:{}", rendered_host, identity.port))
}

pub fn validate_endpoint_service_name(endpoint: &str, service_id: &str) -> Result<()> {
    validate_endpoint_id(endpoint)?;
    let identity = parse_endpoint_id(endpoint)?;
    ensure(
        identity.service_name == service_id.trim(),
        "endpoint service name must match service_id",
    )
}

pub fn release_install_operation(
    operation_id: impl Into<String>,
    manifest: &ServiceManifest,
    installed_service_ids: &[String],
) -> Result<Operation> {
    release_install_operation_with_release(
        operation_id,
        manifest,
        None,
        installed_service_ids,
        "127.0.0.1",
        None,
        serde_json::json!({}),
    )
}

pub fn release_install_operation_with_release(
    operation_id: impl Into<String>,
    manifest: &ServiceManifest,
    release: Option<&ServiceReleaseManifest>,
    installed_service_ids: &[String],
    host_ip: &str,
    endpoint: Option<&str>,
    install_options: Value,
) -> Result<Operation> {
    let host_ip = host_ip.trim();
    ensure(!host_ip.is_empty(), "release install host_ip is required")?;
    host_ip.parse::<IpAddr>().map_err(|_| {
        OrchestratorError::InvalidManifest("host_ip must be an IP address".to_string())
    })?;
    if let Some(release) = release {
        validate_service_release(release)?;
        ensure(
            release.service_name == manifest.id,
            "release service_name must match service.yaml id",
        )?;
        ensure(
            release.version == manifest.version,
            "release version must match service.yaml version",
        )?;
        ensure(
            release.service_type == manifest.kind,
            "release service_type must match service.yaml kind",
        )?;
        ensure(
            release.backend.protocol == manifest.endpoint.protocol,
            "release backend protocol must match service endpoint protocol",
        )?;
        ensure(
            release.backend.port == manifest.endpoint.default_port,
            "release backend port must match service endpoint default_port",
        )?;
        ensure(
            release.backend.health_path == manifest.endpoint.health_path,
            "release backend health_path must match service endpoint health_path",
        )?;
    }
    let exists = installed_service_ids
        .iter()
        .any(|service_id| service_id == &manifest.id);
    let endpoint = endpoint
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| {
            format!(
                "{}:{}:{}",
                host_ip, manifest.endpoint.default_port, manifest.id
            )
        });
    validate_endpoint_service_name(&endpoint, &manifest.id)?;
    let mut steps = Vec::new();
    if let Some(release) = release {
        steps.push(serde_json::json!({
            "action": "fetch_or_load_release_package",
            "target": release.source.url,
            "status": "loader-gated"
        }));
        steps.push(serde_json::json!({
            "action": "validate_service_release",
            "target": release.service_name,
            "detail": "validate release.yaml against service.yaml"
        }));
    } else {
        steps.push(serde_json::json!({
            "action": "validate_service_manifest",
            "target": manifest.id
        }));
    }
    steps.push(serde_json::json!({
        "action": "select_host",
        "target": host_ip
    }));
    steps.push(serde_json::json!({
        "action": "create_host_service",
        "target": format!("{}:{}", host_ip, manifest.id),
        "detail": "host_ip + service-name is unique"
    }));
    steps.push(serde_json::json!({
        "action": "allocate_endpoint",
        "target": endpoint,
        "detail": "endpoint identity is ip:port:service-name"
    }));
    if let Some(release) = release {
        if !release.permissions.is_empty() {
            steps.push(serde_json::json!({
                "action": "register_permissions",
                "target": release.service_name,
                "count": release.permissions.len(),
                "status": "registry"
            }));
            steps.push(serde_json::json!({
                "action": "sync_auth_permissions",
                "target": release.service_name,
                "count": release.permissions.len(),
                "status": "runtime-capable"
            }));
        }
        if !release.routes.is_empty() {
            steps.push(serde_json::json!({
                "action": "register_gateway_routes",
                "target": release.service_name,
                "count": release.routes.len(),
                "status": "registry"
            }));
            steps.push(serde_json::json!({
                "action": "publish_gateway_routes",
                "target": release.service_name,
                "count": release.routes.len(),
                "status": "runtime-capable"
            }));
        }
        if !release.apis.is_empty() {
            steps.push(serde_json::json!({
                "action": "register_api_surface",
                "target": release.service_name,
                "count": release.apis.len(),
                "status": "registry"
            }));
            steps.push(serde_json::json!({
                "action": "refresh_effective_api_view",
                "target": release.service_name,
                "count": release.apis.len(),
                "status": "runtime-capable"
            }));
            steps.push(serde_json::json!({
                "action": "reload_gateway_effective_routes",
                "target": release.service_name,
                "count": release.apis.len(),
                "status": "runtime-capable"
            }));
        }
        if release.frontend.enabled {
            steps.push(serde_json::json!({
                "action": "register_frontend_entry",
                "target": release.frontend.route_prefix,
                "status": "registry-only"
            }));
        }
        if !release.migrations.is_empty() {
            steps.push(serde_json::json!({
                "action": "register_service_migrations",
                "target": release.service_name,
                "count": release.migrations.len(),
                "status": "registry"
            }));
            steps.push(serde_json::json!({
                "action": "run_service_migrations",
                "target": release.service_name,
                "count": release.migrations.len(),
                "status": "runtime-capable",
                "dry_run": install_options
                    .get("migration_dry_run")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
                "allow_destructive": install_options
                    .get("allow_destructive_migrations")
                    .and_then(Value::as_bool)
                    .unwrap_or(false)
            }));
        }
        if !release.redis.is_empty() {
            steps.push(serde_json::json!({
                "action": "register_redis_resources",
                "target": release.service_name,
                "count": release.redis.len(),
                "status": "registry"
            }));
            steps.push(serde_json::json!({
                "action": "provision_redis_resources",
                "target": release.service_name,
                "count": release.redis.len(),
                "status": "runtime-capable"
            }));
        }
        if !release.storage.is_empty() {
            steps.push(serde_json::json!({
                "action": "register_storage_resources",
                "target": release.service_name,
                "count": release.storage.len(),
                "status": "registry"
            }));
            steps.push(serde_json::json!({
                "action": "provision_storage_resources",
                "target": release.service_name,
                "count": release.storage.len(),
                "status": "runtime-capable"
            }));
        }
        steps.push(serde_json::json!({
            "action": "render_service_config",
            "target": release.service_name
        }));
    }
    steps.push(serde_json::json!({
        "action": "dispatch_to_node_or_standalone",
        "target": host_ip,
        "status": "driver-gated",
        "execute": install_options
            .get("execute_service_driver")
            .and_then(Value::as_bool)
            .unwrap_or(false)
    }));
    if exists {
        steps.push(serde_json::json!({
            "action": "refresh_service_metadata",
            "target": manifest.id,
            "detail": "refresh Service metadata"
        }));
    } else {
        steps.push(serde_json::json!({
            "action": "insert_service",
            "target": manifest.id,
            "detail": "insert service row"
        }));
    }
    steps.push(serde_json::json!({
        "action": "start_service",
        "target": endpoint,
        "status": "driver-gated",
        "execute": install_options
            .get("execute_service_driver")
            .and_then(Value::as_bool)
            .unwrap_or(false)
    }));
    steps.push(serde_json::json!({
        "action": "health_probe",
        "target": endpoint
    }));
    steps.push(serde_json::json!({
        "action": "mark_running_state",
        "target": format!("{}:{}", host_ip, manifest.id)
    }));
    let operation = plan_operation(
        operation_id,
        "release.install",
        "ServiceRelease",
        manifest.id.clone(),
        serde_json::json!({
            "service_id": manifest.id,
            "version": manifest.version,
            "default_port": manifest.endpoint.default_port,
            "already_known": exists,
            "host_ip": host_ip,
            "endpoint": endpoint,
            "service_manifest": manifest,
            "release_manifest": release,
            "migration_dry_run": install_options
                .get("migration_dry_run")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            "allow_destructive_migrations": install_options
                .get("allow_destructive_migrations")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            "execute_service_driver": install_options
                .get("execute_service_driver")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            "external_service_running": install_options
                .get("external_service_running")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            "gateway_node_id": install_options
                .get("gateway_node_id")
                .and_then(Value::as_str)
                .unwrap_or("")
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
                },
                {
                    "action": "remove_created_host_service_and_endpoint",
                    "target": endpoint
                }
            ]
        }),
    )?;
    debug_assert_eq!(operation.status, OperationStatus::Planned);
    Ok(operation)
}

pub fn release_create_operation(
    operation_id: impl Into<String>,
    release: &ServiceReleaseManifest,
    release_url: Option<&str>,
) -> Result<Operation> {
    release_upsert_operation(operation_id, "release.create", release, release_url, false)
}

pub fn release_update_operation(
    operation_id: impl Into<String>,
    release: &ServiceReleaseManifest,
    release_url: Option<&str>,
) -> Result<Operation> {
    release_upsert_operation(operation_id, "release.update", release, release_url, true)
}

fn release_upsert_operation(
    operation_id: impl Into<String>,
    action: &str,
    release: &ServiceReleaseManifest,
    release_url: Option<&str>,
    update: bool,
) -> Result<Operation> {
    validate_service_release(release)?;
    let action_step = if update {
        "update_release_record"
    } else {
        "create_release_record"
    };
    let rollback_step = if update {
        "restore_previous_release_record"
    } else {
        "delete_created_release_record"
    };
    let release_url = release_url
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or(release.source.url.as_str());
    plan_operation(
        operation_id,
        action,
        "ServiceRelease",
        format!("{}@{}", release.service_name, release.version),
        serde_json::json!({
            "service_id": release.service_name,
            "version": release.version,
            "release_url": release_url,
            "release_manifest": release,
        }),
        serde_json::json!({
            "steps": [
                {
                    "action": "validate_service_release",
                    "target": release.service_name
                },
                {
                    "action": action_step,
                    "target": format!("{}@{}", release.service_name, release.version)
                }
            ],
            "requires_confirmation": false
        }),
        serde_json::json!({
            "steps": [
                {
                    "action": rollback_step,
                    "target": format!("{}@{}", release.service_name, release.version)
                }
            ]
        }),
    )
}

pub fn release_delete_operation(
    operation_id: impl Into<String>,
    service_name: impl AsRef<str>,
    version: Option<&str>,
) -> Result<Operation> {
    let service_name = service_name.as_ref().trim();
    ensure(!service_name.is_empty(), "service_id is required")?;
    let version = version.unwrap_or("").trim();
    let target_id = if version.is_empty() {
        service_name.to_string()
    } else {
        format!("{service_name}@{version}")
    };
    plan_operation(
        operation_id,
        "release.delete",
        "ServiceRelease",
        target_id.clone(),
        serde_json::json!({
            "service_id": service_name,
            "version": version,
        }),
        serde_json::json!({
            "steps": [
                {
                    "action": "delete_release_record",
                    "target": target_id
                },
                {
                    "action": "clear_release_registry_resources",
                    "target": service_name
                }
            ],
            "requires_confirmation": true
        }),
        serde_json::json!({
            "steps": [
                {
                    "action": "restore_previous_release_registry",
                    "target": service_name
                }
            ]
        }),
    )
}

pub fn release_rollback_operation(
    operation_id: impl Into<String>,
    service_name: impl AsRef<str>,
    version: Option<&str>,
    target_operation_id: Option<&str>,
) -> Result<Operation> {
    let service_name = service_name.as_ref().trim();
    ensure(!service_name.is_empty(), "service_id is required")?;
    let version = version.unwrap_or("").trim();
    let target_operation_id = target_operation_id.unwrap_or("").trim();
    let target_id = if target_operation_id.is_empty() {
        if version.is_empty() {
            service_name.to_string()
        } else {
            format!("{service_name}@{version}")
        }
    } else {
        target_operation_id.to_string()
    };
    plan_operation(
        operation_id,
        "release.rollback",
        "ServiceRelease",
        target_id.clone(),
        serde_json::json!({
            "service_id": service_name,
            "version": version,
            "target_operation_id": target_operation_id,
        }),
        serde_json::json!({
            "steps": [
                {
                    "action": "find_release_install_operation",
                    "target": target_id
                },
                {
                    "action": "rollback_release_install_operation",
                    "target": target_id
                }
            ],
            "requires_confirmation": true
        }),
        serde_json::json!({
            "steps": []
        }),
    )
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

pub fn endpoint_create_operation(
    operation_id: impl Into<String>,
    endpoint: &Endpoint,
) -> Result<Operation> {
    validate_endpoint(endpoint)?;
    plan_operation(
        operation_id,
        "endpoint.create",
        "Endpoint",
        endpoint.endpoint.clone(),
        serde_json::json!({
            "endpoint": endpoint.endpoint,
            "service_id": endpoint.service_id,
            "protocol": endpoint.protocol,
            "health_path": endpoint.health_path,
            "display_name": endpoint.display_name,
            "note": endpoint.note,
            "config": endpoint.config,
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
            "config": endpoint.config,
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
            "policy": link.policy,
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
            "policy": link.policy,
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
        validate_endpoint_service_name(endpoint, service_id)?;
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

pub fn log_create_operation(
    operation_id: impl Into<String>,
    service_id: impl AsRef<str>,
    endpoint: Option<&str>,
) -> Result<Operation> {
    let service_id = service_id.as_ref().trim();
    ensure(!service_id.is_empty(), "service_id is required")?;
    if let Some(endpoint) = endpoint {
        validate_endpoint_service_name(endpoint, service_id)?;
    }
    plan_operation(
        operation_id,
        "log.create",
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

pub fn log_query_operation(
    operation_id: impl Into<String>,
    target_operation_id: impl AsRef<str>,
) -> Result<Operation> {
    let target_operation_id = target_operation_id.as_ref().trim();
    ensure(!target_operation_id.is_empty(), "operation_id is required")?;
    plan_operation(
        operation_id,
        "log.query",
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

pub fn diagnostic_export_operation(
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
        "diagnostic.export",
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

pub fn preview_deployment_template(template: &DeploymentTemplate) -> DeploymentTemplatePreview {
    DeploymentTemplatePreview {
        template_id: template.id.clone(),
        services: template
            .services
            .iter()
            .map(|item| item.id().to_string())
            .collect(),
        default_links: template.default_links.clone(),
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

fn discover_service_manifests(repo_root: &Path) -> Result<Vec<ServiceManifest>> {
    let services_dir = repo_root.join("services");
    let mut services = Vec::new();
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
        services.push(manifest);
    }
    Ok(services)
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

fn validate_release_path(repo_root: &Path, release_path: &Path) -> Result<()> {
    ensure(!release_path.is_absolute(), "release path must be relative")?;
    reject_path_components(release_path)?;
    ensure(
        release_path.file_name().and_then(|v| v.to_str()) == Some("release.yaml"),
        "release manifest file must be release.yaml",
    )?;
    let services_dir = repo_root.join("services");
    let full = repo_root.join(release_path);
    let canonical_services = services_dir.canonicalize().map_err(|_| {
        OrchestratorError::UnsafePath("services directory is not available".to_string())
    })?;
    let canonical_release = full
        .canonicalize()
        .map_err(|_| OrchestratorError::UnsafePath(sanitize_path_for_error(release_path)))?;
    ensure(
        canonical_release.starts_with(&canonical_services),
        "release manifest must stay under services",
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
                let text = value.to_str().ok_or_else(|| {
                    OrchestratorError::UnsafePath("path segment must be UTF-8".to_string())
                })?;
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

fn is_supported_route_method(value: &str) -> bool {
    matches!(
        value.trim().to_ascii_uppercase().as_str(),
        "ANY" | "GET" | "POST" | "PUT" | "PATCH" | "DELETE" | "HEAD" | "OPTIONS"
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

fn is_supported_release_source_kind(value: &str) -> bool {
    matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "github-release" | "repo" | "url" | "local"
    )
}

fn is_supported_release_runtime_kind(value: &str) -> bool {
    matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "image" | "binary" | "system-service" | "external" | "local-process"
    )
}

fn validate_release_runtime(runtime: &ReleaseRuntimeDecl) -> Result<()> {
    let env_re = Regex::new(r"^[A-Z_][A-Z0-9_]*$").expect("valid regex");
    let kind = runtime.kind.trim().to_ascii_lowercase();
    if kind == "local-process" {
        ensure(
            !runtime.command.trim().is_empty(),
            "release runtime.command is required for local-process",
        )?;
    }
    validate_runtime_text(&runtime.command, "release runtime.command")?;
    validate_runtime_text(&runtime.binary, "release runtime.binary")?;
    validate_runtime_text(&runtime.system_service, "release runtime.system_service")?;
    for arg in &runtime.args {
        validate_runtime_text(arg, "release runtime.args")?;
    }
    if !runtime.working_dir.trim().is_empty() {
        validate_relative_runtime_path(&runtime.working_dir)?;
    }
    for (key, value) in &runtime.env {
        ensure(env_re.is_match(key), "release runtime.env key is invalid")?;
        validate_runtime_text(value, "release runtime.env value")?;
    }
    Ok(())
}

fn validate_runtime_text(value: &str, label: &str) -> Result<()> {
    ensure(
        !value.contains('\n') && !value.contains('\r') && !value.contains('\0'),
        &format!("{label} is invalid"),
    )
}

fn validate_relative_runtime_path(path: &str) -> Result<()> {
    let path = Path::new(path.trim());
    ensure(
        !path.is_absolute()
            && path
                .components()
                .all(|component| !matches!(component, Component::ParentDir | Component::Prefix(_))),
        "release runtime.working_dir must stay inside repository",
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
