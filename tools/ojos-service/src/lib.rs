use regex::Regex;
use semver::Version;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Component, Path, PathBuf},
};
use thiserror::Error;

pub mod codegen;
pub mod compatibility;
pub mod publish;
pub mod seal;

pub const SERVICE_SOURCE_API_VERSION: &str = "ojos.dev/v1";
pub const SERVICE_CONTRACT_SCHEMA_VERSION: &str = "ojos.dev/service-contract/v3";
pub const COMPILER_VERSION: &str = env!("CARGO_PKG_VERSION");
const HTTP_METHODS: &[&str] = &["get", "put", "post", "delete", "head", "patch"];
const AUDIENCES: &[&str] = &["internal", "user", "public", "admin"];
const RESERVED_MOUNTS: &[&str] = &["/internal", "/health", "/healthz", "/metrics", "/__ojos"];

#[derive(Debug, Error)]
pub enum CompilerError {
    #[error("read {path}: {source}")]
    Read {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("write {path}: {source}")]
    Write {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("service source YAML is invalid: {0}")]
    Yaml(#[from] serde_yaml::Error),
    #[error("service source is invalid: {0}")]
    Invalid(String),
    #[error("OpenAPI {path} is invalid: {message}")]
    OpenApi { path: PathBuf, message: String },
    #[error("serialize compiled contract: {0}")]
    Json(#[from] serde_json::Error),
}

pub type Result<T> = std::result::Result<T, CompilerError>;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ServiceSource {
    pub api_version: String,
    pub kind: String,
    pub metadata: ServiceMetadata,
    pub runtime: RuntimeSource,
    #[serde(default)]
    pub provides: ProvidesSource,
    #[serde(default)]
    pub requires: RequiresSource,
    #[serde(default)]
    pub resources: Vec<ResourceSource>,
    #[serde(default)]
    pub migrations: Vec<MigrationSource>,
    #[serde(default)]
    pub events: EventsSource,
    #[serde(default)]
    pub permissions: Vec<PermissionSource>,
    #[serde(default)]
    pub exposures: Vec<ExposureSource>,
    #[serde(default)]
    pub frontends: Vec<FrontendSource>,
    #[serde(default)]
    pub config_schema: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ServiceMetadata {
    pub id: String,
    pub version: Version,
    pub display_name: String,
    #[serde(default)]
    pub permission_namespace: Option<String>,
    /// Permission keys owned by another service or by the platform. These
    /// references may guard operations and frontend routes, but are never
    /// projected as permission definitions owned by this release.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub permission_references: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RuntimeSource {
    pub profile: String,
    pub artifact: String,
    pub http_port: u16,
    pub health: HealthSource,
    /// Signed, platform-owned writable storage. v1 deliberately permits at
    /// most one RETAIN volume and never accepts a host path or Docker name.
    #[serde(default)]
    pub volumes: Vec<RuntimeVolumeSource>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RuntimeVolumeSource {
    pub name: String,
    pub kind: String,
    pub target: String,
    pub access: String,
    pub lifecycle: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct HealthSource {
    pub path: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ProvidesSource {
    #[serde(default)]
    pub apis: Vec<ApiDocumentSource>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ApiDocumentSource {
    pub document: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RequiresSource {
    #[serde(default)]
    pub apis: Vec<ApiRequirementSource>,
    #[serde(default)]
    pub packages: Vec<PackageRequirementSource>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ApiRequirementSource {
    pub id: String,
    pub version: String,
    #[serde(default)]
    pub optional: bool,
    #[serde(default = "default_selection")]
    pub selection: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PackageRequirementSource {
    pub id: String,
    pub version: String,
    #[serde(default)]
    pub development: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ResourceSource {
    pub name: String,
    #[serde(rename = "type")]
    pub resource_type: String,
    #[serde(default = "default_lifecycle")]
    pub lifecycle: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct MigrationSource {
    pub id: String,
    pub artifact: String,
    pub resource: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct EventsSource {
    #[serde(default)]
    pub publishes: Vec<EventSource>,
    #[serde(default)]
    pub subscribes: Vec<EventSource>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct EventSource {
    #[serde(rename = "type")]
    pub event_type: String,
    pub version: u32,
    pub schema: String,
    #[serde(default = "default_delivery")]
    pub delivery: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PermissionSource {
    pub key: String,
    pub title: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ExposureSource {
    pub id: String,
    pub api: String,
    pub audience: String,
    pub mount: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct FrontendSource {
    pub target: String,
    pub manifest: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FrontendManifestV1 {
    pub schema_version: String,
    pub module_id: String,
    pub target: String,
    pub artifact: String,
    pub host_api_range: String,
    #[serde(default)]
    pub routes: Vec<FrontendRouteV1>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FrontendRouteV1 {
    pub id: String,
    pub path: String,
    pub title: String,
    #[serde(default)]
    pub menu: bool,
    #[serde(default)]
    pub order: i32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub permission: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ServiceContractV3 {
    pub schema_version: String,
    pub compiler_version: String,
    pub service_id: String,
    pub service_version: Version,
    pub display_name: String,
    pub source_digest: String,
    pub runtime: RuntimeSource,
    pub api_surfaces: Vec<ApiSurfaceV3>,
    pub operations: Vec<ApiOperationV3>,
    pub api_requirements: Vec<ApiRequirementSource>,
    pub package_requirements: Vec<PackageRequirementSource>,
    pub resource_claims: Vec<ResourceSource>,
    pub migrations: Vec<MigrationSource>,
    pub events: EventsContractV1,
    pub permissions: Vec<PermissionSource>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub permission_references: Vec<String>,
    pub exposures: Vec<ExposureSource>,
    pub routes: Vec<RouteContributionV1>,
    pub frontends: Vec<FrontendContractV1>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub config_schema: Option<ArtifactFileV1>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct EventsContractV1 {
    #[serde(default)]
    pub publishes: Vec<EventContractV1>,
    #[serde(default)]
    pub subscribes: Vec<EventContractV1>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EventContractV1 {
    #[serde(rename = "type")]
    pub event_type: String,
    pub version: u32,
    pub schema: ArtifactFileV1,
    /// Canonical JSON Schema embedded in the signed contract so generators do
    /// not need to reopen mutable source files. Its digest must equal
    /// `schema.digest`.
    pub payload_schema: Value,
    pub delivery: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ArtifactFileV1 {
    pub path: String,
    pub digest: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RouteContributionV1 {
    pub exposure_id: String,
    pub audience: String,
    pub method: String,
    pub path: String,
    pub api_id: String,
    pub operation_id: String,
    pub provider_path: String,
    pub auth: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub permission: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub permission_scope: Option<PermissionScopeV1>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "lowercase")]
pub enum SystemPermissionScopeV1 {
    System,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PathParameterPermissionScopeV1 {
    #[serde(rename = "type")]
    pub scope_type: String,
    pub path_parameter: String,
}

/// The signed permission target for an operation. The wire representation is
/// deliberately restricted to either the scalar `"system"` or an object that
/// derives a resource id from one required path parameter.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(untagged)]
pub enum PermissionScopeV1 {
    System(SystemPermissionScopeV1),
    PathParameter(PathParameterPermissionScopeV1),
}

impl PermissionScopeV1 {
    pub const fn system() -> Self {
        Self::System(SystemPermissionScopeV1::System)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FrontendContractV1 {
    pub target: String,
    pub manifest: ArtifactFileV1,
    pub module: FrontendManifestV1,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ApiSurfaceV3 {
    pub api_id: String,
    pub version: Version,
    pub document: String,
    pub document_digest: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ApiOperationV3 {
    pub api_id: String,
    pub api_version: Version,
    pub operation_id: String,
    pub provider_path: String,
    pub method: String,
    pub audience: String,
    pub auth: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub permission: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub permission_scope: Option<PermissionScopeV1>,
    #[serde(default)]
    pub parameters: Vec<ParameterContractV3>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_body: Option<RequestBodyContractV3>,
    #[serde(default)]
    pub responses: Vec<ResponseContractV3>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ParameterContractV3 {
    pub name: String,
    pub location: String,
    pub required: bool,
    pub schema: Value,
    pub schema_digest: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RequestBodyContractV3 {
    pub required: bool,
    pub content: Vec<MediaSchemaContractV3>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ResponseContractV3 {
    pub status: String,
    pub content: Vec<MediaSchemaContractV3>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MediaSchemaContractV3 {
    pub media_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub schema: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub schema_digest: Option<String>,
}

fn default_selection() -> String {
    "unique-healthy".to_string()
}
fn default_lifecycle() -> String {
    "retain".to_string()
}
fn default_delivery() -> String {
    "durable".to_string()
}

pub fn load_source(path: &Path) -> Result<ServiceSource> {
    let bytes = read(path)?;
    let source: ServiceSource = serde_yaml::from_slice(&bytes)?;
    validate_source(path, &source)?;
    Ok(source)
}

pub fn compile(path: &Path) -> Result<ServiceContractV3> {
    let source_bytes = read(path)?;
    let source: ServiceSource = serde_yaml::from_slice(&source_bytes)?;
    validate_source(path, &source)?;
    let root = path.parent().unwrap_or_else(|| Path::new("."));
    let owned_permission_keys = source
        .permissions
        .iter()
        .map(|item| item.key.as_str())
        .collect::<BTreeSet<_>>();
    let permission_keys = owned_permission_keys
        .iter()
        .copied()
        .chain(
            source
                .metadata
                .permission_references
                .iter()
                .map(String::as_str),
        )
        .collect::<BTreeSet<_>>();
    let mut api_surfaces = Vec::new();
    let mut operations = Vec::new();
    let mut seen_api_ids = BTreeSet::new();
    let mut seen_operation_ids = BTreeSet::new();
    for api in &source.provides.apis {
        let api_path = resolve_local(root, &api.document)?;
        let bytes = read(&api_path)?;
        let document: Value =
            serde_yaml::from_slice(&bytes).map_err(|error| CompilerError::OpenApi {
                path: api_path.clone(),
                message: error.to_string(),
            })?;
        let (surface, mut api_operations) = compile_openapi(
            &source.metadata.id,
            &source.metadata.version,
            &api.document,
            &bytes,
            &api_path,
            &document,
            &permission_keys,
        )?;
        if !seen_api_ids.insert(surface.api_id.clone()) {
            return Err(invalid(format!("duplicate API id {}", surface.api_id)));
        }
        for operation in &api_operations {
            if !seen_operation_ids.insert(operation.operation_id.clone()) {
                return Err(invalid(format!(
                    "duplicate operationId {}",
                    operation.operation_id
                )));
            }
        }
        api_surfaces.push(surface);
        operations.append(&mut api_operations);
    }
    let mut routes = Vec::new();
    let mut exposed_operations = BTreeMap::<String, usize>::new();
    for exposure in &source.exposures {
        if !seen_api_ids.contains(&exposure.api) {
            return Err(invalid(format!(
                "exposure {} references unknown API {}",
                exposure.id, exposure.api
            )));
        }
        for operation in operations.iter().filter(|operation| {
            operation.api_id == exposure.api && operation.audience == exposure.audience
        }) {
            *exposed_operations
                .entry(operation.operation_id.clone())
                .or_default() += 1;
            let external_path = join_mount(&exposure.mount, &operation.provider_path);
            routes.push(RouteContributionV1 {
                exposure_id: exposure.id.clone(),
                audience: exposure.audience.clone(),
                method: operation.method.clone(),
                path: external_path,
                api_id: operation.api_id.clone(),
                operation_id: operation.operation_id.clone(),
                provider_path: operation.provider_path.clone(),
                auth: operation.auth.clone(),
                permission: operation.permission.clone(),
                permission_scope: operation.permission_scope.clone(),
            });
        }
        if !operations.iter().any(|operation| {
            operation.api_id == exposure.api && operation.audience == exposure.audience
        }) {
            return Err(invalid(format!(
                "exposure {} matches no {} operations in API {}",
                exposure.id, exposure.audience, exposure.api
            )));
        }
    }
    for operation in &operations {
        let exposure_count = exposed_operations
            .get(&operation.operation_id)
            .copied()
            .unwrap_or_default();
        if operation.audience == "internal" && exposure_count != 0 {
            return Err(invalid(format!(
                "internal operation {} cannot be exposed",
                operation.operation_id
            )));
        }
        if operation.audience != "internal" && exposure_count != 1 {
            return Err(invalid(format!(
                "external operation {} must be selected by exactly one exposure, found {}",
                operation.operation_id, exposure_count
            )));
        }
    }
    validate_route_collisions(&routes)?;
    api_surfaces.sort_by(|a, b| a.api_id.cmp(&b.api_id));
    operations.sort_by(|a, b| {
        a.api_id
            .cmp(&b.api_id)
            .then(a.provider_path.cmp(&b.provider_path))
            .then(a.method.cmp(&b.method))
            .then(a.operation_id.cmp(&b.operation_id))
    });
    routes.sort_by(|a, b| {
        a.audience
            .cmp(&b.audience)
            .then(a.path.cmp(&b.path))
            .then(a.method.cmp(&b.method))
            .then(a.operation_id.cmp(&b.operation_id))
    });
    let config_schema = match &source.config_schema {
        Some(reference) => Some(artifact_file(root, reference)?),
        None => None,
    };
    let events = compile_events(root, &source.events)?;
    let frontends = compile_frontends(
        root,
        &source.metadata.id,
        &source.frontends,
        &permission_keys,
    )?;
    let normalized_source = serde_json_canonicalizer::to_vec(&source)?;
    let source_digest = format!("sha256:{:x}", Sha256::digest(&normalized_source));
    let mut contract = ServiceContractV3 {
        schema_version: SERVICE_CONTRACT_SCHEMA_VERSION.to_string(),
        compiler_version: COMPILER_VERSION.to_string(),
        service_id: source.metadata.id.clone(),
        service_version: source.metadata.version.clone(),
        display_name: source.metadata.display_name.clone(),
        source_digest,
        runtime: source.runtime.clone(),
        api_surfaces,
        operations,
        api_requirements: source.requires.apis.clone(),
        package_requirements: source.requires.packages.clone(),
        resource_claims: source.resources.clone(),
        migrations: source.migrations.clone(),
        events,
        permissions: source.permissions.clone(),
        permission_references: source.metadata.permission_references.clone(),
        exposures: source.exposures.clone(),
        routes,
        frontends,
        config_schema,
    };
    sort_contract(&mut contract);
    Ok(contract)
}

pub fn contract_bytes(contract: &ServiceContractV3) -> Result<Vec<u8>> {
    validate_contract_event_schemas(contract)?;
    Ok(serde_json_canonicalizer::to_vec(contract)?)
}

/// Revalidates every embedded event schema against its signed digest.
///
/// Event schemas are embedded in v3 contracts so code generation never has to
/// reopen a mutable source file.  Treat both the supported-schema validation
/// and this canonical digest comparison as a trust-boundary check: callers
/// that deserialize a contract must not be able to seal or generate from a
/// schema document that differs from the separately signed schema artifact.
pub fn validate_contract_event_schemas(contract: &ServiceContractV3) -> Result<()> {
    for event in contract
        .events
        .publishes
        .iter()
        .chain(contract.events.subscribes.iter())
    {
        let reference = format!("{} v{}", event.event_type, event.version);
        validate_event_payload_schema(&event.payload_schema, &reference)?;
        let actual = value_digest(&event.payload_schema)?;
        if actual != event.schema.digest {
            return Err(invalid(format!(
                "event {reference} embedded payloadSchema digest {actual} does not match {}",
                event.schema.digest
            )));
        }
    }
    Ok(())
}

pub fn write_contract(contract: &ServiceContractV3, output: &Path) -> Result<()> {
    let mut bytes = serde_json::to_vec_pretty(contract)?;
    bytes.push(b'\n');
    if let Some(parent) = output.parent() {
        fs::create_dir_all(parent).map_err(|source| CompilerError::Write {
            path: parent.to_path_buf(),
            source,
        })?;
    }
    fs::write(output, bytes).map_err(|source| CompilerError::Write {
        path: output.to_path_buf(),
        source,
    })
}

pub fn discover(root: &Path) -> Result<Vec<PathBuf>> {
    let services = root.join("services");
    let mut manifests = Vec::new();
    let entries = fs::read_dir(&services).map_err(|source| CompilerError::Read {
        path: services.clone(),
        source,
    })?;
    for entry in entries {
        let entry = entry.map_err(|source| CompilerError::Read {
            path: services.clone(),
            source,
        })?;
        let path = entry.path().join("ojos.service.yaml");
        if path.is_file() {
            manifests.push(path);
        }
    }
    manifests.sort();
    Ok(manifests)
}

fn compile_openapi(
    service_id: &str,
    service_version: &Version,
    document_reference: &str,
    bytes: &[u8],
    path: &Path,
    document: &Value,
    permission_keys: &BTreeSet<&str>,
) -> Result<(ApiSurfaceV3, Vec<ApiOperationV3>)> {
    let object = document
        .as_object()
        .ok_or_else(|| openapi(path, "root must be an object"))?;
    let version = required_text(object.get("openapi"), path, "openapi")?;
    if !version.starts_with("3.1.") {
        return Err(openapi(path, "openapi must use version 3.1.x"));
    }
    let api_id = required_text(object.get("x-ojos-api-id"), path, "x-ojos-api-id")?.to_string();
    validate_key(&api_id, "x-ojos-api-id")?;
    let info = object
        .get("info")
        .and_then(Value::as_object)
        .ok_or_else(|| openapi(path, "info must be an object"))?;
    let api_version = Version::parse(required_text(info.get("version"), path, "info.version")?)
        .map_err(|error| openapi(path, format!("info.version must be semver: {error}")))?;
    let service_extension = object
        .get("x-ojos-service")
        .and_then(Value::as_object)
        .ok_or_else(|| openapi(path, "x-ojos-service must be an object"))?;
    if required_text(service_extension.get("id"), path, "x-ojos-service.id")? != service_id {
        return Err(openapi(
            path,
            "x-ojos-service.id does not match service metadata.id",
        ));
    }
    if Version::parse(required_text(
        service_extension.get("version"),
        path,
        "x-ojos-service.version",
    )?)
    .ok()
    .as_ref()
        != Some(service_version)
    {
        return Err(openapi(
            path,
            "x-ojos-service.version does not match service metadata.version",
        ));
    }
    reject_remote_refs(document, path)?;
    let paths = object
        .get("paths")
        .and_then(Value::as_object)
        .ok_or_else(|| openapi(path, "paths must be an object"))?;
    let mut operations = Vec::new();
    for (provider_path, item) in paths {
        validate_path(provider_path, "OpenAPI path")?;
        validate_template_path(provider_path, path)?;
        let item = item
            .as_object()
            .ok_or_else(|| openapi(path, format!("path {provider_path} must be an object")))?;
        for method in HTTP_METHODS {
            let Some(operation) = item.get(*method) else {
                continue;
            };
            let operation = operation.as_object().ok_or_else(|| {
                openapi(path, format!("{method} {provider_path} must be an object"))
            })?;
            let operation_id =
                required_text(operation.get("operationId"), path, "operationId")?.to_string();
            let audience =
                required_text(operation.get("x-ojos-audience"), path, "x-ojos-audience")?
                    .to_ascii_lowercase();
            if !AUDIENCES.contains(&audience.as_str()) {
                return Err(openapi(
                    path,
                    format!("operation {operation_id} has invalid audience {audience}"),
                ));
            }
            let permission = operation
                .get("x-ojos-permission")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|item| !item.is_empty())
                .map(str::to_string);
            if let Some(permission) = &permission
                && !permission_keys.contains(permission.as_str())
            {
                return Err(openapi(
                    path,
                    format!("operation {operation_id} permission {permission} is not declared"),
                ));
            }
            let auth = auth_mode(operation, object, path)?;
            if audience == "admin" && (auth != "required" || permission.is_none()) {
                return Err(openapi(
                    path,
                    format!(
                        "admin operation {operation_id} requires bearer security and permission"
                    ),
                ));
            }
            if permission.is_some() && auth != "required" {
                return Err(openapi(
                    path,
                    format!("operation {operation_id} with permission requires bearer security"),
                ));
            }
            let parameters = compile_parameters(item, operation, document, path)?;
            let permission_scope = compile_permission_scope(
                operation,
                permission.as_deref(),
                provider_path,
                &parameters,
                path,
            )?;
            let request_body = compile_request_body(operation, document, path)?;
            let responses = compile_responses(operation, document, path)?;
            operations.push(ApiOperationV3 {
                api_id: api_id.clone(),
                api_version: api_version.clone(),
                operation_id,
                provider_path: provider_path.clone(),
                method: method.to_ascii_uppercase(),
                audience,
                auth,
                permission,
                permission_scope,
                parameters,
                request_body,
                responses,
            });
        }
    }
    if operations.is_empty() {
        return Err(openapi(path, "OpenAPI must declare at least one operation"));
    }
    Ok((
        ApiSurfaceV3 {
            api_id,
            version: api_version,
            document: document_reference.to_string(),
            document_digest: format!("sha256:{:x}", Sha256::digest(bytes)),
        },
        operations,
    ))
}

fn auth_mode(
    operation: &Map<String, Value>,
    root: &Map<String, Value>,
    path: &Path,
) -> Result<String> {
    let security = operation.get("security").or_else(|| root.get("security"));
    let Some(security) = security else {
        return Ok("anonymous".to_string());
    };
    let entries = security
        .as_array()
        .ok_or_else(|| openapi(path, "security must be an array"))?;
    if entries.is_empty() {
        return Ok("anonymous".to_string());
    }
    let mut bearer = false;
    let mut anonymous = false;
    for entry in entries {
        let entry = entry
            .as_object()
            .ok_or_else(|| openapi(path, "security entry must be an object"))?;
        anonymous |= entry.is_empty();
        bearer |= entry.contains_key("ojosBearer");
        if !entry.is_empty() && !entry.contains_key("ojosBearer") {
            return Err(openapi(path, "only ojosBearer security is supported"));
        }
    }
    Ok(match (bearer, anonymous) {
        (true, true) => "optional",
        (true, false) => "required",
        (false, true) => "anonymous",
        _ => {
            return Err(openapi(
                path,
                "security does not declare ojosBearer or anonymous access",
            ));
        }
    }
    .to_string())
}

fn compile_parameters(
    path_item: &Map<String, Value>,
    operation: &Map<String, Value>,
    document: &Value,
    path: &Path,
) -> Result<Vec<ParameterContractV3>> {
    let mut by_identity = BTreeMap::new();
    for parameters in [path_item.get("parameters"), operation.get("parameters")]
        .into_iter()
        .flatten()
    {
        let parameters = parameters
            .as_array()
            .ok_or_else(|| openapi(path, "parameters must be an array"))?;
        for parameter in parameters {
            let parameter = resolve_internal_ref(document, parameter, path)?;
            let object = parameter
                .as_object()
                .ok_or_else(|| openapi(path, "parameter must be an object"))?;
            let name = required_text(object.get("name"), path, "parameter.name")?.to_string();
            let location = required_text(object.get("in"), path, "parameter.in")?.to_string();
            if !matches!(location.as_str(), "path" | "query" | "header" | "cookie") {
                return Err(openapi(
                    path,
                    format!("parameter {name} has invalid location"),
                ));
            }
            let required = object
                .get("required")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            if location == "path" && !required {
                return Err(openapi(
                    path,
                    format!("path parameter {name} must be required"),
                ));
            }
            let schema = object
                .get("schema")
                .ok_or_else(|| openapi(path, format!("parameter {name} requires schema")))?;
            let schema = normalize_schema(document, schema, path)?;
            let schema_digest = value_digest(&schema)?;
            by_identity.insert(
                (location.clone(), name.clone()),
                ParameterContractV3 {
                    name,
                    location,
                    required,
                    schema,
                    schema_digest,
                },
            );
        }
    }
    Ok(by_identity.into_values().collect())
}

fn compile_permission_scope(
    operation: &Map<String, Value>,
    permission: Option<&str>,
    provider_path: &str,
    parameters: &[ParameterContractV3],
    source: &Path,
) -> Result<Option<PermissionScopeV1>> {
    let declared = operation.get("x-ojos-scope");
    if permission.is_none() {
        if declared.is_some() {
            return Err(openapi(
                source,
                "x-ojos-scope cannot be declared without x-ojos-permission",
            ));
        }
        return Ok(None);
    }
    let Some(declared) = declared else {
        return Ok(Some(PermissionScopeV1::system()));
    };
    if declared.as_str() == Some("system") {
        return Ok(Some(PermissionScopeV1::system()));
    }
    let object = declared.as_object().ok_or_else(|| {
        openapi(
            source,
            "x-ojos-scope must be system or {type, pathParameter}",
        )
    })?;
    if object.len() != 2 || !object.contains_key("type") || !object.contains_key("pathParameter") {
        return Err(openapi(
            source,
            "x-ojos-scope object only accepts type and pathParameter",
        ));
    }
    let scope_type = required_text(object.get("type"), source, "x-ojos-scope.type")?;
    validate_key(scope_type, "x-ojos-scope.type")?;
    if scope_type.len() > 128 {
        return Err(openapi(
            source,
            "x-ojos-scope.type must be at most 128 bytes",
        ));
    }
    if scope_type == "system" {
        return Err(openapi(
            source,
            "resource x-ojos-scope.type cannot be system; use scalar system",
        ));
    }
    let path_parameter = required_text(
        object.get("pathParameter"),
        source,
        "x-ojos-scope.pathParameter",
    )?;
    let placeholder = format!("{{{path_parameter}}}");
    if !provider_path
        .split('/')
        .any(|segment| segment == placeholder)
    {
        return Err(openapi(
            source,
            format!(
                "x-ojos-scope.pathParameter {path_parameter} is not present in provider path {provider_path}"
            ),
        ));
    }
    if !parameters.iter().any(|parameter| {
        parameter.location == "path" && parameter.name == path_parameter && parameter.required
    }) {
        return Err(openapi(
            source,
            format!(
                "x-ojos-scope.pathParameter {path_parameter} must name a required path parameter"
            ),
        ));
    }
    Ok(Some(PermissionScopeV1::PathParameter(
        PathParameterPermissionScopeV1 {
            scope_type: scope_type.to_string(),
            path_parameter: path_parameter.to_string(),
        },
    )))
}

fn compile_request_body(
    operation: &Map<String, Value>,
    document: &Value,
    path: &Path,
) -> Result<Option<RequestBodyContractV3>> {
    let Some(request_body) = operation.get("requestBody") else {
        return Ok(None);
    };
    let request_body = resolve_internal_ref(document, request_body, path)?;
    let object = request_body
        .as_object()
        .ok_or_else(|| openapi(path, "requestBody must be an object"))?;
    Ok(Some(RequestBodyContractV3 {
        required: object
            .get("required")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        content: compile_content(object.get("content"), document, path)?,
    }))
}

fn compile_responses(
    operation: &Map<String, Value>,
    document: &Value,
    path: &Path,
) -> Result<Vec<ResponseContractV3>> {
    let responses = operation
        .get("responses")
        .and_then(Value::as_object)
        .ok_or_else(|| openapi(path, "responses must be an object"))?;
    if responses.is_empty() {
        return Err(openapi(path, "responses cannot be empty"));
    }
    let mut result = Vec::with_capacity(responses.len());
    for (status, response) in responses {
        if status != "default"
            && !(status.len() == 3 && status.chars().all(|character| character.is_ascii_digit()))
        {
            return Err(openapi(path, format!("invalid response status {status}")));
        }
        let response = resolve_internal_ref(document, response, path)?;
        let object = response
            .as_object()
            .ok_or_else(|| openapi(path, format!("response {status} must be an object")))?;
        result.push(ResponseContractV3 {
            status: status.clone(),
            content: compile_content(object.get("content"), document, path)?,
        });
    }
    result.sort_by(|a, b| a.status.cmp(&b.status));
    Ok(result)
}

fn compile_content(
    content: Option<&Value>,
    document: &Value,
    path: &Path,
) -> Result<Vec<MediaSchemaContractV3>> {
    let Some(content) = content else {
        return Ok(Vec::new());
    };
    let content = content
        .as_object()
        .ok_or_else(|| openapi(path, "content must be an object"))?;
    let mut result = Vec::with_capacity(content.len());
    for (media_type, media) in content {
        let media = media
            .as_object()
            .ok_or_else(|| openapi(path, format!("media type {media_type} must be an object")))?;
        let schema = media
            .get("schema")
            .map(|schema| normalize_schema(document, schema, path))
            .transpose()?;
        let schema_digest = schema.as_ref().map(value_digest).transpose()?;
        result.push(MediaSchemaContractV3 {
            media_type: media_type.clone(),
            schema,
            schema_digest,
        });
    }
    result.sort_by(|a, b| a.media_type.cmp(&b.media_type));
    Ok(result)
}

fn normalize_schema(document: &Value, schema: &Value, path: &Path) -> Result<Value> {
    let schema = resolve_internal_ref(document, schema, path)?;
    match schema {
        Value::Object(object) => {
            let mut normalized = Map::new();
            for (key, value) in object {
                if key == "$ref" {
                    continue;
                }
                normalized.insert(key.clone(), normalize_schema(document, value, path)?);
            }
            Ok(Value::Object(normalized))
        }
        Value::Array(items) => Ok(Value::Array(
            items
                .iter()
                .map(|value| normalize_schema(document, value, path))
                .collect::<Result<Vec<_>>>()?,
        )),
        value => Ok(value.clone()),
    }
}

fn resolve_internal_ref<'a>(
    document: &'a Value,
    value: &'a Value,
    path: &Path,
) -> Result<&'a Value> {
    let Some(reference) = value.get("$ref") else {
        return Ok(value);
    };
    let reference = reference
        .as_str()
        .ok_or_else(|| openapi(path, "$ref must be a string"))?;
    let pointer = reference
        .strip_prefix('#')
        .ok_or_else(|| openapi(path, "only local document $ref values are supported"))?;
    document
        .pointer(pointer)
        .ok_or_else(|| openapi(path, format!("unresolved $ref {reference}")))
}

fn validate_source(path: &Path, source: &ServiceSource) -> Result<()> {
    if source.api_version != SERVICE_SOURCE_API_VERSION {
        return Err(invalid(format!(
            "apiVersion must be {SERVICE_SOURCE_API_VERSION}"
        )));
    }
    if source.kind != "Service" {
        return Err(invalid("kind must be Service"));
    }
    let id_re = Regex::new(r"^[a-z0-9](?:[a-z0-9-]{0,61}[a-z0-9])?$").unwrap();
    if !id_re.is_match(&source.metadata.id) {
        return Err(invalid("metadata.id must be a lowercase DNS label"));
    }
    if source.metadata.display_name.trim().is_empty() {
        return Err(invalid("metadata.displayName is required"));
    }
    if !matches!(
        source.runtime.profile.as_str(),
        "standard-container-v1" | "judge-sandbox-v1"
    ) {
        return Err(invalid("runtime.profile is unsupported"));
    }
    if source.runtime.artifact.trim().is_empty() || source.runtime.http_port == 0 {
        return Err(invalid("runtime artifact and httpPort are required"));
    }
    validate_path(&source.runtime.health.path, "runtime.health.path")?;
    if source.runtime.volumes.len() > 1 {
        return Err(invalid(
            "runtime.volumes supports at most one managed RETAIN volume in v1",
        ));
    }
    if !source.runtime.volumes.is_empty() && source.runtime.profile != "standard-container-v1" {
        return Err(invalid(
            "runtime.volumes is supported only by standard-container-v1",
        ));
    }
    let mut volume_names = BTreeSet::new();
    let mut volume_targets = BTreeSet::new();
    for volume in &source.runtime.volumes {
        validate_key(&volume.name, "runtime volume name")?;
        validate_path(&volume.target, "runtime volume target")?;
        if !volume_names.insert(volume.name.as_str())
            || !volume_targets.insert(volume.target.as_str())
        {
            return Err(invalid("runtime volume names and targets must be unique"));
        }
        if volume.kind != "managed-volume" || volume.access != "rw" || volume.lifecycle != "retain"
        {
            return Err(invalid(format!(
                "runtime volume {} must use kind=managed-volume, access=rw, lifecycle=retain",
                volume.name
            )));
        }
        if runtime_volume_target_is_reserved(&volume.target) {
            return Err(invalid(format!(
                "runtime volume {} target {} is reserved by the platform",
                volume.name, volume.target
            )));
        }
    }
    let mut names = BTreeSet::new();
    for resource in &source.resources {
        validate_key(&resource.name, "resource name")?;
        if !names.insert(resource.name.as_str()) {
            return Err(invalid(format!(
                "duplicate resource name {}",
                resource.name
            )));
        }
        if resource.lifecycle != "retain" {
            return Err(invalid(format!(
                "resource {} lifecycle must be retain in v1",
                resource.name
            )));
        }
        if resource.resource_type != "postgresql.database/v1" {
            return Err(invalid(format!(
                "resource {} type {} is unsupported in v1",
                resource.name, resource.resource_type
            )));
        }
    }
    let mut migration_ids = BTreeSet::new();
    let mut migration_artifacts = BTreeSet::new();
    for migration in &source.migrations {
        validate_key(&migration.id, "migration id")?;
        if !migration_ids.insert(migration.id.as_str()) {
            return Err(invalid(format!("duplicate migration id {}", migration.id)));
        }
        if !migration_artifacts.insert(migration.artifact.as_str()) {
            return Err(invalid(format!(
                "duplicate migration artifact slot {}",
                migration.artifact
            )));
        }
        if !names.contains(migration.resource.as_str()) {
            return Err(invalid(format!(
                "migration {} references unknown resource {}",
                migration.id, migration.resource
            )));
        }
    }
    let permission_namespace = source
        .metadata
        .permission_namespace
        .as_deref()
        .unwrap_or(&source.metadata.id);
    validate_key(permission_namespace, "metadata.permissionNamespace")?;
    if permission_namespace != source.metadata.id {
        let compatible_namespace = source
            .metadata
            .id
            .strip_suffix("-service")
            .or_else(|| source.metadata.id.strip_suffix("-api"));
        if compatible_namespace != Some(permission_namespace) {
            return Err(invalid(
                "metadata.permissionNamespace may only preserve the service id without its -service or -api suffix",
            ));
        }
    }
    let mut permissions = BTreeSet::new();
    for permission in &source.permissions {
        validate_key(&permission.key, "permission key")?;
        if !permission
            .key
            .starts_with(&format!("{permission_namespace}."))
        {
            return Err(invalid(format!(
                "permission {} must start with {}.",
                permission.key, permission_namespace
            )));
        }
        if !permissions.insert(permission.key.as_str()) {
            return Err(invalid(format!("duplicate permission {}", permission.key)));
        }
    }
    let mut permission_references = BTreeSet::new();
    for permission in &source.metadata.permission_references {
        validate_key(permission, "metadata.permissionReferences entry")?;
        if permissions.contains(permission.as_str()) {
            return Err(invalid(format!(
                "permission reference {permission} is also declared as an owned permission"
            )));
        }
        if permission.starts_with(&format!("{permission_namespace}.")) {
            return Err(invalid(format!(
                "permission reference {permission} must be external to namespace {permission_namespace}"
            )));
        }
        if !permission_references.insert(permission.as_str()) {
            return Err(invalid(format!(
                "duplicate permission reference {permission}"
            )));
        }
    }
    let mut exposure_ids = BTreeSet::new();
    for exposure in &source.exposures {
        if !exposure_ids.insert(exposure.id.as_str()) {
            return Err(invalid(format!("duplicate exposure id {}", exposure.id)));
        }
        if !matches!(exposure.audience.as_str(), "user" | "public" | "admin") {
            return Err(invalid(format!(
                "exposure {} audience is invalid",
                exposure.id
            )));
        }
        validate_mount(&exposure.mount)?;
    }
    for frontend in &source.frontends {
        if !matches!(frontend.target.as_str(), "user-shell" | "admin-shell") {
            return Err(invalid(format!(
                "frontend target {} is unsupported",
                frontend.target
            )));
        }
    }
    let mut package_ids = BTreeSet::new();
    for package in &source.requires.packages {
        if !package_ids.insert(package.id.as_str()) {
            return Err(invalid(format!(
                "duplicate package requirement {}",
                package.id
            )));
        }
        semver::VersionReq::parse(&package.version).map_err(|error| {
            invalid(format!(
                "package requirement {} has invalid version: {error}",
                package.id
            ))
        })?;
    }
    let root = path.parent().unwrap_or_else(|| Path::new("."));
    for reference in source
        .provides
        .apis
        .iter()
        .map(|item| &item.document)
        .chain(source.events.publishes.iter().map(|item| &item.schema))
        .chain(source.events.subscribes.iter().map(|item| &item.schema))
        .chain(source.frontends.iter().map(|item| &item.manifest))
        .chain(source.config_schema.iter())
    {
        let target = resolve_local(root, reference)?;
        if !target.is_file() {
            return Err(invalid(format!(
                "referenced file {reference} does not exist"
            )));
        }
    }
    for requirement in &source.requires.apis {
        validate_key(&requirement.id, "API requirement id")?;
        semver::VersionReq::parse(&requirement.version).map_err(|error| {
            invalid(format!(
                "API requirement {} has invalid version: {error}",
                requirement.id
            ))
        })?;
        if requirement.selection != "unique-healthy" && requirement.selection != "explicit" {
            return Err(invalid(format!(
                "API requirement {} selection is unsupported",
                requirement.id
            )));
        }
    }
    let mut api_requirements = BTreeSet::new();
    for requirement in &source.requires.apis {
        if !api_requirements.insert(requirement.id.as_str()) {
            return Err(invalid(format!(
                "duplicate API requirement {}",
                requirement.id
            )));
        }
    }
    let mut published_events = BTreeSet::new();
    for event in &source.events.publishes {
        published_events.insert((event.event_type.as_str(), event.version));
    }
    for event in &source.events.subscribes {
        if published_events.contains(&(event.event_type.as_str(), event.version)) {
            return Err(invalid(format!(
                "event {} version {} cannot be both published and subscribed by one service",
                event.event_type, event.version
            )));
        }
    }
    Ok(())
}

fn sort_contract(contract: &mut ServiceContractV3) {
    contract.runtime.volumes.sort();
    contract.api_requirements.sort_by(|a, b| a.id.cmp(&b.id));
    contract
        .package_requirements
        .sort_by(|a, b| a.id.cmp(&b.id));
    contract.resource_claims.sort_by(|a, b| a.name.cmp(&b.name));
    contract.migrations.sort_by(|a, b| a.id.cmp(&b.id));
    contract.events.publishes.sort_by(|a, b| {
        a.event_type
            .cmp(&b.event_type)
            .then(a.version.cmp(&b.version))
    });
    contract.events.subscribes.sort_by(|a, b| {
        a.event_type
            .cmp(&b.event_type)
            .then(a.version.cmp(&b.version))
    });
    contract.permissions.sort_by(|a, b| a.key.cmp(&b.key));
    contract.permission_references.sort();
    contract.exposures.sort_by(|a, b| a.id.cmp(&b.id));
    contract.frontends.sort_by(|a, b| {
        a.target
            .cmp(&b.target)
            .then(a.manifest.path.cmp(&b.manifest.path))
    });
}

fn runtime_volume_target_is_reserved(target: &str) -> bool {
    target == "/"
        || ["/run/ojos", "/proc", "/sys", "/dev"]
            .iter()
            .any(|reserved| target == *reserved || target.starts_with(&format!("{reserved}/")))
}

fn compile_events(root: &Path, source: &EventsSource) -> Result<EventsContractV1> {
    fn compile_side(root: &Path, events: &[EventSource]) -> Result<Vec<EventContractV1>> {
        let mut identities = BTreeSet::new();
        let mut result = Vec::with_capacity(events.len());
        for event in events {
            validate_key(&event.event_type, "event type")?;
            if event.version == 0 {
                return Err(invalid(format!(
                    "event {} version must be greater than zero",
                    event.event_type
                )));
            }
            if !matches!(event.delivery.as_str(), "durable" | "signal") {
                return Err(invalid(format!(
                    "event {} delivery must be durable or signal",
                    event.event_type
                )));
            }
            if !identities.insert((event.event_type.as_str(), event.version)) {
                return Err(invalid(format!(
                    "duplicate event {} version {}",
                    event.event_type, event.version
                )));
            }
            let schema_path = resolve_local(root, &event.schema)?;
            let schema: Value = serde_json::from_slice(&read(&schema_path)?).map_err(|error| {
                invalid(format!(
                    "event schema {} is invalid JSON: {error}",
                    event.schema
                ))
            })?;
            validate_json_schema(&schema, &event.schema)?;
            validate_event_payload_schema(&schema, &event.schema)?;
            result.push(EventContractV1 {
                event_type: event.event_type.clone(),
                version: event.version,
                schema: ArtifactFileV1 {
                    path: event.schema.clone(),
                    digest: value_digest(&schema)?,
                },
                payload_schema: schema,
                delivery: event.delivery.clone(),
            });
        }
        result.sort_by(|a, b| {
            a.event_type
                .cmp(&b.event_type)
                .then(a.version.cmp(&b.version))
        });
        Ok(result)
    }

    Ok(EventsContractV1 {
        publishes: compile_side(root, &source.publishes)?,
        subscribes: compile_side(root, &source.subscribes)?,
    })
}

fn compile_frontends(
    root: &Path,
    service_id: &str,
    sources: &[FrontendSource],
    permission_keys: &BTreeSet<&str>,
) -> Result<Vec<FrontendContractV1>> {
    let mut targets = BTreeSet::new();
    let mut route_paths = BTreeSet::<(String, String)>::new();
    let mut result = Vec::with_capacity(sources.len());
    for source in sources {
        if !targets.insert(source.target.as_str()) {
            return Err(invalid(format!(
                "duplicate frontend target {}",
                source.target
            )));
        }
        let manifest_path = resolve_local(root, &source.manifest)?;
        let bytes = read(&manifest_path)?;
        let mut manifest: FrontendManifestV1 = serde_json::from_slice(&bytes).map_err(|error| {
            invalid(format!(
                "frontend manifest {} is invalid: {error}",
                source.manifest
            ))
        })?;
        if manifest.schema_version != "ojos.frontend/v1" {
            return Err(invalid(format!(
                "frontend manifest {} schemaVersion must be ojos.frontend/v1",
                source.manifest
            )));
        }
        if manifest.target != source.target {
            return Err(invalid(format!(
                "frontend manifest {} target does not match service source",
                source.manifest
            )));
        }
        if !manifest.module_id.starts_with(&format!("{service_id}.")) {
            return Err(invalid(format!(
                "frontend moduleId {} must start with {service_id}.",
                manifest.module_id
            )));
        }
        semver::VersionReq::parse(&manifest.host_api_range).map_err(|error| {
            invalid(format!(
                "frontend {} hostApiRange is invalid: {error}",
                manifest.module_id
            ))
        })?;
        if manifest.artifact.trim().is_empty() {
            return Err(invalid(format!(
                "frontend {} artifact slot is required",
                manifest.module_id
            )));
        }
        manifest.routes.sort_by(|a, b| a.id.cmp(&b.id));
        let mut route_ids = BTreeSet::new();
        for route in &manifest.routes {
            validate_key(&route.id, "frontend route id")?;
            if !route_ids.insert(route.id.as_str()) {
                return Err(invalid(format!("duplicate frontend route id {}", route.id)));
            }
            validate_mount(&route.path)?;
            if !route_paths.insert((source.target.clone(), route.path.clone())) {
                return Err(invalid(format!(
                    "duplicate frontend path {} for {}",
                    route.path, source.target
                )));
            }
            if let Some(permission) = &route.permission
                && !permission_keys.contains(permission.as_str())
            {
                return Err(invalid(format!(
                    "frontend route {} permission {} is not declared",
                    route.id, permission
                )));
            }
        }
        let normalized = serde_json_canonicalizer::to_vec(&manifest)?;
        result.push(FrontendContractV1 {
            target: source.target.clone(),
            manifest: ArtifactFileV1 {
                path: source.manifest.clone(),
                digest: format!("sha256:{:x}", Sha256::digest(normalized)),
            },
            module: manifest,
        });
    }
    result.sort_by(|a, b| a.target.cmp(&b.target));
    Ok(result)
}

fn validate_json_schema(schema: &Value, reference: &str) -> Result<()> {
    let object = schema
        .as_object()
        .ok_or_else(|| invalid(format!("schema {reference} must be an object")))?;
    if let Some(dialect) = object.get("$schema").and_then(Value::as_str)
        && dialect != "https://json-schema.org/draft/2020-12/schema"
    {
        return Err(invalid(format!(
            "schema {reference} must use JSON Schema 2020-12"
        )));
    }
    reject_nonlocal_schema_refs(schema, reference)
}

fn validate_event_payload_schema(schema: &Value, reference: &str) -> Result<()> {
    fn visit(schema: &Value, path: &str, reference: &str) -> Result<()> {
        let object = schema
            .as_object()
            .ok_or_else(|| invalid(format!("event schema {reference} {path} must be an object")))?;
        if object.contains_key("$ref")
            || object.contains_key("oneOf")
            || object.contains_key("anyOf")
            || object.contains_key("allOf")
        {
            return Err(invalid(format!(
                "event schema {reference} {path} uses a composition feature that typed SDK generation does not support"
            )));
        }
        if let Some(values) = object.get("enum") {
            let values = values.as_array().ok_or_else(|| {
                invalid(format!(
                    "event schema {reference} {path}.enum must be an array"
                ))
            })?;
            if values.is_empty() || !values.iter().all(Value::is_string) {
                return Err(invalid(format!(
                    "event schema {reference} {path}.enum must contain only strings"
                )));
            }
            return Ok(());
        }
        if let Some(value) = object.get("const") {
            if !value.is_string() {
                return Err(invalid(format!(
                    "event schema {reference} {path}.const must be a string"
                )));
            }
            return Ok(());
        }
        let schema_type = object.get("type").and_then(Value::as_str).ok_or_else(|| {
            invalid(format!(
                "event schema {reference} {path} must declare type, enum, or const"
            ))
        })?;
        match schema_type {
            "object" => {
                let properties = object
                    .get("properties")
                    .and_then(Value::as_object)
                    .ok_or_else(|| {
                        invalid(format!(
                            "event schema {reference} {path} object must declare properties"
                        ))
                    })?;
                if object.get("additionalProperties") != Some(&Value::Bool(false)) {
                    return Err(invalid(format!(
                        "event schema {reference} {path} must set additionalProperties to false"
                    )));
                }
                let required = object
                    .get("required")
                    .map(|value| {
                        value.as_array().ok_or_else(|| {
                            invalid(format!(
                                "event schema {reference} {path}.required must be an array"
                            ))
                        })
                    })
                    .transpose()?
                    .cloned()
                    .unwrap_or_default();
                let mut required_names = BTreeSet::<String>::new();
                for value in required {
                    let name = value.as_str().ok_or_else(|| {
                        invalid(format!(
                            "event schema {reference} {path}.required must contain strings"
                        ))
                    })?;
                    if !properties.contains_key(name) || !required_names.insert(name.to_string()) {
                        return Err(invalid(format!(
                            "event schema {reference} {path} has an invalid required property {name}"
                        )));
                    }
                }
                for (name, property) in properties {
                    if name.is_empty() || name.chars().any(char::is_control) {
                        return Err(invalid(format!(
                            "event schema {reference} {path} has an invalid property name"
                        )));
                    }
                    visit(property, &format!("{path}.properties.{name}"), reference)?;
                }
            }
            "array" => visit(
                object.get("items").ok_or_else(|| {
                    invalid(format!("event schema {reference} {path} array needs items"))
                })?,
                &format!("{path}.items"),
                reference,
            )?,
            "string" | "integer" | "number" | "boolean" => {}
            other => {
                return Err(invalid(format!(
                    "event schema {reference} {path} type {other} is unsupported for typed SDK generation"
                )));
            }
        }
        Ok(())
    }

    if schema.get("type").and_then(Value::as_str) != Some("object") {
        return Err(invalid(format!(
            "event schema {reference} root type must be object"
        )));
    }
    visit(schema, "$", reference)
}

fn reject_nonlocal_schema_refs(value: &Value, reference: &str) -> Result<()> {
    match value {
        Value::Object(object) => {
            for (key, value) in object {
                if key == "$ref"
                    && !value
                        .as_str()
                        .is_some_and(|item| item.starts_with("#/") || item.starts_with("sha256:"))
                {
                    return Err(invalid(format!(
                        "schema {reference} contains an unpinned or remote $ref"
                    )));
                }
                reject_nonlocal_schema_refs(value, reference)?;
            }
        }
        Value::Array(items) => {
            for item in items {
                reject_nonlocal_schema_refs(item, reference)?;
            }
        }
        _ => {}
    }
    Ok(())
}

fn artifact_file(root: &Path, reference: &str) -> Result<ArtifactFileV1> {
    let path = resolve_local(root, reference)?;
    let value: Value = serde_json::from_slice(&read(&path)?)
        .map_err(|error| invalid(format!("schema {reference} is invalid JSON: {error}")))?;
    validate_json_schema(&value, reference)?;
    Ok(ArtifactFileV1 {
        path: reference.to_string(),
        digest: value_digest(&value)?,
    })
}

fn value_digest(value: &Value) -> Result<String> {
    Ok(format!(
        "sha256:{:x}",
        Sha256::digest(serde_json_canonicalizer::to_vec(value)?)
    ))
}

fn join_mount(mount: &str, provider_path: &str) -> String {
    if provider_path == "/" {
        mount.to_string()
    } else if mount == "/" {
        provider_path.to_string()
    } else {
        format!("{mount}{provider_path}")
    }
}

fn validate_route_collisions(routes: &[RouteContributionV1]) -> Result<()> {
    for (index, left) in routes.iter().enumerate() {
        for right in &routes[index + 1..] {
            if left.audience == right.audience
                && methods_overlap(&left.method, &right.method)
                && paths_overlap(&left.path, &right.path)
            {
                return Err(invalid(format!(
                    "route collision between {} {} and {} {}",
                    left.method, left.path, right.method, right.path
                )));
            }
        }
    }
    Ok(())
}

fn methods_overlap(left: &str, right: &str) -> bool {
    left == right || (left == "GET" && right == "HEAD") || (left == "HEAD" && right == "GET")
}

fn paths_overlap(left: &str, right: &str) -> bool {
    let left = left.trim_matches('/').split('/').collect::<Vec<_>>();
    let right = right.trim_matches('/').split('/').collect::<Vec<_>>();
    left.len() == right.len()
        && left.iter().zip(right).all(|(left, right)| {
            left == &right
                || (left.starts_with('{') && left.ends_with('}'))
                || (right.starts_with('{') && right.ends_with('}'))
        })
}

fn resolve_local(root: &Path, reference: &str) -> Result<PathBuf> {
    let path = Path::new(reference);
    if path.is_absolute()
        || reference.contains("\0")
        || reference.contains("://")
        || path.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
    {
        return Err(invalid(format!(
            "reference {reference} must stay inside the service directory"
        )));
    }
    Ok(root.join(path))
}

fn reject_remote_refs(value: &Value, path: &Path) -> Result<()> {
    match value {
        Value::Object(object) => {
            for (key, value) in object {
                if key == "$ref" {
                    let reference = value
                        .as_str()
                        .ok_or_else(|| openapi(path, "$ref must be a string"))?;
                    if !reference.starts_with("#/") {
                        return Err(openapi(
                            path,
                            "only local document $ref values are supported",
                        ));
                    }
                }
                reject_remote_refs(value, path)?;
            }
        }
        Value::Array(items) => {
            for item in items {
                reject_remote_refs(item, path)?;
            }
        }
        _ => {}
    }
    Ok(())
}

fn validate_key(value: &str, field: &str) -> Result<()> {
    let re = Regex::new(r"^[a-z][a-z0-9]*(?:[._-][a-z0-9]+)*$").unwrap();
    if !re.is_match(value) {
        return Err(invalid(format!("{field} {value} is invalid")));
    }
    Ok(())
}

fn validate_path(value: &str, field: &str) -> Result<()> {
    if !value.starts_with('/')
        || value.contains("//")
        || value.contains('?')
        || value.contains('#')
        || (value.len() > 1 && value.ends_with('/'))
    {
        return Err(invalid(format!(
            "{field} must be an absolute normalized path"
        )));
    }
    Ok(())
}

fn validate_template_path(value: &str, source: &Path) -> Result<()> {
    for segment in value.trim_matches('/').split('/') {
        let contains_brace = segment.contains('{') || segment.contains('}');
        if contains_brace
            && !(segment.starts_with('{')
                && segment.ends_with('}')
                && segment.matches('{').count() == 1
                && segment.matches('}').count() == 1
                && segment.len() > 2)
        {
            return Err(openapi(
                source,
                format!("path {value} contains a malformed or greedy parameter"),
            ));
        }
    }
    Ok(())
}

fn validate_mount(value: &str) -> Result<()> {
    validate_path(value, "exposure.mount")?;
    if value.contains('{') || value.contains('}') {
        return Err(invalid("exposure.mount cannot contain parameters"));
    }
    if RESERVED_MOUNTS
        .iter()
        .any(|reserved| value == *reserved || value.starts_with(&format!("{reserved}/")))
    {
        return Err(invalid(format!(
            "exposure.mount {value} uses a reserved prefix"
        )));
    }
    Ok(())
}

fn required_text<'a>(value: Option<&'a Value>, path: &Path, field: &str) -> Result<&'a str> {
    value
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|item| !item.is_empty())
        .ok_or_else(|| openapi(path, format!("{field} must be a non-empty string")))
}

fn read(path: &Path) -> Result<Vec<u8>> {
    fs::read(path).map_err(|source| CompilerError::Read {
        path: path.to_path_buf(),
        source,
    })
}
fn invalid(message: impl Into<String>) -> CompilerError {
    CompilerError::Invalid(message.into())
}
fn openapi(path: &Path, message: impl Into<String>) -> CompilerError {
    CompilerError::OpenApi {
        path: path.to_path_buf(),
        message: message.into(),
    }
}

pub fn check_report(path: &Path) -> Result<Value> {
    let contract = compile(path)?;
    Ok(json!({
        "status": "ok",
        "serviceId": contract.service_id,
        "serviceVersion": contract.service_version,
        "apiCount": contract.api_surfaces.len(),
        "operationCount": contract.operations.len(),
        "resourceCount": contract.resource_claims.len(),
        "contractDigest": format!("sha256:{:x}", Sha256::digest(contract_bytes(&contract)?)),
    }))
}

pub fn discover_report(root: &Path) -> Result<Value> {
    let manifests = discover(root)?;
    let mut services = BTreeMap::new();
    for manifest in manifests {
        let contract = compile(&manifest)?;
        services.insert(contract.service_id, json!({"manifest": manifest.strip_prefix(root).unwrap_or(&manifest), "version": contract.service_version}));
    }
    Ok(json!({"schemaVersion": "ojos.dev/discovery/v1", "services": services}))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(root: &Path) -> PathBuf {
        fs::create_dir_all(root.join("api")).unwrap();
        fs::write(
            root.join("api/openapi.yaml"),
            r#"openapi: 3.1.0
info: {title: Contest, version: 1.0.0}
x-ojos-api-id: contest.api
x-ojos-service: {id: contest, version: 1.0.0}
components:
  securitySchemes:
    ojosBearer: {type: http, scheme: bearer}
paths:
  /contests:
    get:
      operationId: listContests
      x-ojos-audience: user
      x-ojos-permission: contest.read
      security: [{ojosBearer: []}]
      responses: {'200': {description: OK}}
  /contests/{contestId}:
    parameters:
      - name: contestId
        in: path
        required: true
        schema: {type: integer, format: int64}
    get:
      operationId: getContest
      x-ojos-audience: internal
      x-ojos-permission: contest.read
      x-ojos-scope: {type: contest, pathParameter: contestId}
      security: [{ojosBearer: []}]
      responses: {'200': {description: OK}}
"#,
        )
        .unwrap();
        let manifest = root.join("ojos.service.yaml");
        fs::write(
            &manifest,
            r#"apiVersion: ojos.dev/v1
kind: Service
metadata: {id: contest, version: 1.0.0, displayName: Contest}
runtime:
  profile: standard-container-v1
  artifact: runtime
  httpPort: 8080
  health: {path: /healthz}
provides:
  apis: [{document: api/openapi.yaml}]
permissions: [{key: contest.read, title: View contests}]
exposures: [{id: user-api, api: contest.api, audience: user, mount: /api/contest}]
"#,
        )
        .unwrap();
        manifest
    }

    #[test]
    fn compilation_is_deterministic_and_operation_scoped() {
        let temp = tempfile::tempdir().unwrap();
        let manifest = fixture(temp.path());
        let first = compile(&manifest).unwrap();
        let second = compile(&manifest).unwrap();
        assert_eq!(
            contract_bytes(&first).unwrap(),
            contract_bytes(&second).unwrap()
        );
        assert_eq!(first.operations[0].operation_id, "listContests");
        assert_eq!(
            first.operations[0].permission.as_deref(),
            Some("contest.read")
        );
        assert_eq!(first.operations[0].provider_path, "/contests");
        assert_eq!(
            first.operations[0].permission_scope,
            Some(PermissionScopeV1::system())
        );
        let scoped = first
            .operations
            .iter()
            .find(|operation| operation.operation_id == "getContest")
            .unwrap();
        assert_eq!(
            scoped.permission_scope,
            Some(PermissionScopeV1::PathParameter(
                PathParameterPermissionScopeV1 {
                    scope_type: "contest".to_string(),
                    path_parameter: "contestId".to_string(),
                }
            ))
        );
    }

    #[test]
    fn permission_scope_requires_permission_and_required_template_parameter() {
        let temp = tempfile::tempdir().unwrap();
        let manifest = fixture(temp.path());
        let openapi = temp.path().join("api/openapi.yaml");
        let original = fs::read_to_string(&openapi).unwrap();
        fs::write(
            &openapi,
            original.replace(
                "x-ojos-audience: user\n      x-ojos-permission: contest.read",
                "x-ojos-audience: user\n      x-ojos-scope: system\n      x-ojos-permission: contest.read",
            ),
        )
        .unwrap();
        assert!(compile(&manifest).is_ok());

        fs::write(
            &openapi,
            original.replace(
                "x-ojos-audience: user\n      x-ojos-permission: contest.read",
                "x-ojos-audience: user\n      x-ojos-scope: system",
            ),
        )
        .unwrap();
        assert!(
            compile(&manifest)
                .unwrap_err()
                .to_string()
                .contains("without x-ojos-permission")
        );

        fs::write(
            &openapi,
            original.replace("pathParameter: contestId", "pathParameter: missing"),
        )
        .unwrap();
        assert!(
            compile(&manifest)
                .unwrap_err()
                .to_string()
                .contains("not present in provider path")
        );
    }

    #[test]
    fn rejects_undeclared_permissions_and_path_escape() {
        let temp = tempfile::tempdir().unwrap();
        let manifest = fixture(temp.path());
        let openapi = temp.path().join("api/openapi.yaml");
        let value = fs::read_to_string(&openapi)
            .unwrap()
            .replace("contest.read", "contest.manage");
        fs::write(&openapi, value).unwrap();
        assert!(
            compile(&manifest)
                .unwrap_err()
                .to_string()
                .contains("not declared")
        );
        fs::write(
            &manifest,
            fs::read_to_string(&manifest)
                .unwrap()
                .replace("api/openapi.yaml", "../openapi.yaml"),
        )
        .unwrap();
        assert!(
            compile(&manifest)
                .unwrap_err()
                .to_string()
                .contains("stay inside")
        );
    }

    #[test]
    fn external_permission_references_are_explicit_and_not_owned() {
        let temp = tempfile::tempdir().unwrap();
        let manifest = fixture(temp.path());
        fs::create_dir_all(temp.path().join("frontend")).unwrap();
        fs::write(
            temp.path().join("frontend/manifest.json"),
            r#"{
  "schemaVersion": "ojos.frontend/v1",
  "moduleId": "contest.admin",
  "target": "admin-shell",
  "artifact": "frontend-admin",
  "hostApiRange": "^1",
  "routes": [{
    "id": "settings",
    "path": "/contest-admin",
    "title": "Contest admin",
    "permission": "system.admin"
  }]
}"#,
        )
        .unwrap();
        let openapi = temp.path().join("api/openapi.yaml");
        fs::write(
            &openapi,
            fs::read_to_string(&openapi)
                .unwrap()
                .replace("contest.read", "system.admin"),
        )
        .unwrap();
        fs::write(
            &manifest,
            fs::read_to_string(&manifest)
                .unwrap()
                .replace(
                    "metadata: {id: contest, version: 1.0.0, displayName: Contest}",
                    "metadata: {id: contest, version: 1.0.0, displayName: Contest, permissionReferences: [system.admin]}",
                )
                .replace(
                    "permissions: [{key: contest.read, title: View contests}]",
                    "permissions: [{key: contest.read, title: View contests}]\nfrontends: [{target: admin-shell, manifest: frontend/manifest.json}]",
                ),
        )
        .unwrap();

        let contract = compile(&manifest).unwrap();
        assert_eq!(
            contract.permission_references,
            vec!["system.admin".to_string()]
        );
        assert_eq!(contract.permissions[0].key, "contest.read");
        assert!(
            contract
                .operations
                .iter()
                .all(|operation| operation.permission.as_deref() == Some("system.admin"))
        );
        assert_eq!(
            contract.frontends[0].module.routes[0].permission.as_deref(),
            Some("system.admin")
        );
        assert!(
            serde_json::to_value(&contract).unwrap()["permissionReferences"]
                .as_array()
                .is_some_and(|items| items == &[Value::String("system.admin".to_string())])
        );
    }

    #[test]
    fn permission_references_cannot_overlap_owned_namespace() {
        let temp = tempfile::tempdir().unwrap();
        let manifest = fixture(temp.path());
        fs::write(
            &manifest,
            fs::read_to_string(&manifest)
                .unwrap()
                .replace(
                    "metadata: {id: contest, version: 1.0.0, displayName: Contest}",
                    "metadata: {id: contest, version: 1.0.0, displayName: Contest, permissionReferences: [contest.manage]}",
                ),
        )
        .unwrap();
        assert!(
            compile(&manifest)
                .unwrap_err()
                .to_string()
                .contains("must be external to namespace contest")
        );
    }

    #[test]
    fn legacy_permission_namespace_is_limited_to_service_suffix() {
        let root = tempfile::tempdir().unwrap();
        let manifest = root.path().join("ojos.service.yaml");
        fs::write(
            &manifest,
            r#"apiVersion: ojos.dev/v1
kind: Service
metadata:
  id: user-service
  version: 1.0.0
  displayName: User
  permissionNamespace: account
runtime:
  profile: standard-container-v1
  artifact: user-runtime
  httpPort: 8080
  health: {path: /readyz}
permissions: [{key: account.profile.read, title: Read profile}]
"#,
        )
        .unwrap();
        let error = compile(&manifest).unwrap_err().to_string();
        assert!(
            error.contains("without its -service or -api suffix"),
            "{error}"
        );

        let source = fs::read_to_string(&manifest)
            .unwrap()
            .replace("permissionNamespace: account", "permissionNamespace: user")
            .replace("account.profile.read", "user.profile.read");
        fs::write(&manifest, source).unwrap();
        assert!(compile(&manifest).is_ok());

        let api_source = fs::read_to_string(&manifest)
            .unwrap()
            .replace("id: user-service", "id: judge-api")
            .replace("displayName: User", "displayName: Judge")
            .replace("permissionNamespace: user", "permissionNamespace: judge")
            .replace("user.profile.read", "judge.submit");
        fs::write(&manifest, api_source).unwrap();
        assert!(compile(&manifest).is_ok());
    }

    #[test]
    fn discovery_is_sorted_and_requires_no_service_list() {
        let temp = tempfile::tempdir().unwrap();
        fixture(&temp.path().join("services/contest"));
        let another = fixture(&temp.path().join("services/another"));
        let openapi = temp.path().join("services/another/api/openapi.yaml");
        fs::write(
            &openapi,
            fs::read_to_string(&openapi)
                .unwrap()
                .replace("contest.api", "another.api")
                .replace("id: contest", "id: another")
                .replace("contest.read", "another.read"),
        )
        .unwrap();
        fs::write(
            &another,
            fs::read_to_string(&another)
                .unwrap()
                .replace("id: contest", "id: another")
                .replace("displayName: Contest", "displayName: Another")
                .replace("contest.api", "another.api")
                .replace("contest.read", "another.read"),
        )
        .unwrap();
        let report = discover_report(temp.path()).unwrap();
        assert_eq!(report["services"].as_object().unwrap().len(), 2);
        assert_eq!(
            report["services"]
                .as_object()
                .unwrap()
                .keys()
                .cloned()
                .collect::<Vec<_>>(),
            vec!["another", "contest"]
        );
    }

    #[test]
    fn exposure_compiles_operation_routes_without_target_urls() {
        let temp = tempfile::tempdir().unwrap();
        let contract = compile(&fixture(temp.path())).unwrap();
        assert_eq!(contract.routes.len(), 1);
        assert_eq!(contract.routes[0].path, "/api/contest/contests");
        assert_eq!(contract.routes[0].provider_path, "/contests");
        let encoded = String::from_utf8(contract_bytes(&contract).unwrap()).unwrap();
        assert!(!encoded.contains("targetUrl"));
        assert!(!encoded.contains("http://"));
        assert!(!encoded.contains("https://"));
    }

    #[test]
    fn route_template_overlap_and_get_head_are_rejected() {
        let routes = vec![
            RouteContributionV1 {
                exposure_id: "one".to_string(),
                audience: "user".to_string(),
                method: "GET".to_string(),
                path: "/api/contest/{id}".to_string(),
                api_id: "contest.api".to_string(),
                operation_id: "getContest".to_string(),
                provider_path: "/{id}".to_string(),
                auth: "required".to_string(),
                permission: Some("contest.read".to_string()),
                permission_scope: Some(PermissionScopeV1::system()),
            },
            RouteContributionV1 {
                exposure_id: "two".to_string(),
                audience: "user".to_string(),
                method: "HEAD".to_string(),
                path: "/api/contest/latest".to_string(),
                api_id: "contest.api".to_string(),
                operation_id: "headLatestContest".to_string(),
                provider_path: "/latest".to_string(),
                auth: "required".to_string(),
                permission: Some("contest.read".to_string()),
                permission_scope: Some(PermissionScopeV1::system()),
            },
        ];
        assert!(validate_route_collisions(&routes).is_err());
    }

    #[test]
    fn event_schema_is_embedded_and_digest_verified() {
        let temp = tempfile::tempdir().unwrap();
        let manifest = fixture(temp.path());
        fs::create_dir_all(temp.path().join("events")).unwrap();
        fs::write(
            temp.path().join("events/contest-created.json"),
            r#"{
  "$schema": "https://json-schema.org/draft/2020-12/schema",
  "type": "object",
  "properties": {
    "id": {"type": "integer"},
    "items": {"type": "array", "items": {"enum": ["one", "two"]}}
  },
  "required": ["id", "items"],
  "additionalProperties": false
}"#,
        )
        .unwrap();
        fs::write(
            &manifest,
            format!(
                "{}\nevents:\n  publishes:\n    - type: contest.created\n      version: 1\n      schema: events/contest-created.json\n      delivery: durable\n",
                fs::read_to_string(&manifest).unwrap()
            ),
        )
        .unwrap();

        let contract = compile(&manifest).unwrap();
        let event = &contract.events.publishes[0];
        assert_eq!(event.payload_schema["properties"]["id"]["type"], "integer");
        validate_contract_event_schemas(&contract).unwrap();

        let mut tampered = contract;
        tampered.events.publishes[0].payload_schema["properties"]["id"]["type"] = json!("string");
        assert!(
            contract_bytes(&tampered)
                .unwrap_err()
                .to_string()
                .contains("does not match")
        );
    }

    #[test]
    fn event_schema_rejects_unsupported_or_open_payloads() {
        for schema in [
            json!({"type": "object", "properties": {}, "additionalProperties": true}),
            json!({"type": "object", "properties": {"id": {"oneOf": [{"type": "integer"}, {"type": "string"}]}}, "additionalProperties": false}),
            json!({"type": "array", "items": {"type": "string"}}),
        ] {
            assert!(validate_event_payload_schema(&schema, "fixture").is_err());
        }
    }
}
