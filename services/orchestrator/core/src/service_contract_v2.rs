use crate::{
    ContributionApiSurfaceV1, ContributionFrontendModuleV1, ContributionOperationRouteV1,
    ContributionPermissionDefinitionV1, ContributionRevisionV1, OrchestratorError,
    ReleaseApiSurfaceDecl, Result, ServiceReleaseManifest, validate_service_release,
};
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

pub const SERVICE_CONTRACT_VERSION: u32 = 2;
pub const RELEASE_PLATFORM_SCHEMA_VERSION: &str = "ojos.dev/release-platform/v1";
pub const STANDARD_CONTAINER_RUNTIME_ID: &str = "standard-container-v1";
pub const STANDARD_CONTAINER_RUNTIME_SHA256: &str =
    "sha256:56c8ec1e421205dbebb97ad40cbda30bf468d198dd8c3fc50151e39465ea573f";
const LEGACY_STANDARD_RUNTIME_ID: &str = "standard-v1";
const LEGACY_STANDARD_RUNTIME_SHA256: &str =
    "sha256:6d80096a6119d715b7e7c46b5a23afc5cc6b213409cc2af7e64ae7b5b6b386f2";

/// Strict, signed projection of Service Contract v3 data into a Catalog v2
/// Release document.  Catalog v2 remains the trust root while older readers
/// can continue consuming the normalized v1/v2 release fields.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReleasePlatformContractV1 {
    pub schema_version: String,
    pub contract_digest: String,
    pub source_digest: String,
    pub release_lock_digest: String,
    #[serde(default)]
    pub artifact_subjects: Vec<ReleaseArtifactSubjectV1>,
    #[serde(default)]
    pub package_requirements: Vec<ReleasePackageRequirementV1>,
    #[serde(default)]
    pub resource_claims: Vec<ReleaseResourceClaimV1>,
    #[serde(default)]
    pub runtime_volumes: Vec<ReleaseRuntimeVolumeV1>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config_schema: Option<ReleaseConfigSchemaV1>,
    #[serde(default)]
    pub contribution: ReleaseContributionSpecV1,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReleaseArtifactSubjectV1 {
    pub slot: String,
    #[serde(default)]
    pub roles: Vec<String>,
    pub media_type: String,
    pub digest: String,
    pub size: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reference: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReleasePackageRequirementV1 {
    pub service_id: String,
    pub version_requirement: String,
    #[serde(default)]
    pub development: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReleaseResourceClaimV1 {
    pub name: String,
    pub resource_type: String,
    #[serde(default = "default_resource_lifecycle")]
    pub lifecycle: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReleaseRuntimeVolumeV1 {
    pub name: String,
    pub kind: String,
    pub target: String,
    pub access: String,
    pub lifecycle: String,
}

fn default_resource_lifecycle() -> String {
    "retain".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReleaseConfigSchemaV1 {
    pub digest: String,
    pub schema: Value,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReleaseContributionSpecV1 {
    #[serde(default)]
    pub api_surfaces: Vec<ContributionApiSurfaceV1>,
    #[serde(default)]
    pub operation_routes: Vec<ContributionOperationRouteV1>,
    #[serde(default)]
    pub permission_definitions: Vec<ContributionPermissionDefinitionV1>,
    #[serde(default)]
    pub user_frontend_modules: Vec<ContributionFrontendModuleV1>,
    #[serde(default)]
    pub admin_frontend_modules: Vec<ContributionFrontendModuleV1>,
}

/// Versioned provider declarations. `apis` deliberately reuses the signed v1
/// API surface shape so a v1 release can be normalized without losing any
/// authorization or routing fields.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ReleaseProvidesContract {
    #[serde(default)]
    pub apis: Vec<ReleaseProvidedApiDecl>,
    #[serde(default)]
    pub events: Vec<ReleaseEventDecl>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(untagged)]
pub enum ReleaseProvidedApiDecl {
    Legacy(ReleaseApiSurfaceDecl),
    Contract(ReleaseProvidedApiContractDecl),
}

impl ReleaseProvidedApiDecl {
    pub fn api_id(&self) -> &str {
        match self {
            Self::Legacy(api) => &api.api_id,
            Self::Contract(api) => &api.id,
        }
    }

    fn normalized_surface(&self, release: &ServiceReleaseManifest) -> ReleaseApiSurfaceDecl {
        match self {
            Self::Legacy(api) => api.clone(),
            Self::Contract(api) => {
                let protocol = if api.protocol.trim().is_empty() {
                    release.backend.protocol.clone()
                } else {
                    api.protocol.clone()
                };
                let auth_mode = api.auth.mode().to_string();
                ReleaseApiSurfaceDecl {
                    api_id: api.id.clone(),
                    protocol: protocol.clone(),
                    port_name: if api.port_name.trim().is_empty() {
                        protocol
                    } else {
                        api.port_name.clone()
                    },
                    path_prefix: api.path.clone(),
                    methods: api.methods.clone(),
                    visibility: if api.visibility.trim().is_empty() {
                        "explicit".to_string()
                    } else {
                        api.visibility.clone()
                    },
                    auth_mode,
                    permission: api.permission.clone(),
                    stability: if api.stability.trim().is_empty() {
                        "stable".to_string()
                    } else {
                        api.stability.clone()
                    },
                    version: api.version.clone(),
                    grpc_service: String::new(),
                    stream_name: String::new(),
                    rate_limit: String::new(),
                    timeout: api
                        .timeout_ms
                        .map(|value| format!("{value}ms"))
                        .unwrap_or_default(),
                    allowed_callers: Vec::new(),
                    denied_callers: Vec::new(),
                }
            }
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ReleaseProvidedApiContractDecl {
    pub id: String,
    pub version: String,
    #[serde(default)]
    pub protocol: String,
    #[serde(default)]
    pub port_name: String,
    pub path: String,
    #[serde(default)]
    pub methods: Vec<String>,
    #[serde(default)]
    pub visibility: String,
    pub auth: ReleaseApiAuthDecl,
    pub permission: String,
    #[serde(default)]
    pub stability: String,
    #[serde(default)]
    pub timeout_ms: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(untagged)]
pub enum ReleaseApiAuthDecl {
    Mode(String),
    Contract { mode: String },
}

impl Default for ReleaseApiAuthDecl {
    fn default() -> Self {
        Self::Mode("workload".to_string())
    }
}

impl ReleaseApiAuthDecl {
    pub fn mode(&self) -> &str {
        match self {
            Self::Mode(mode) | Self::Contract { mode } => mode,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ReleaseRequiresContract {
    #[serde(default)]
    pub apis: Vec<ReleaseRequiredApiDecl>,
    #[serde(default)]
    pub events: Vec<ReleaseRequiredEventDecl>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(untagged)]
pub enum ReleaseRequiredApiDecl {
    ApiId(String),
    Binding(ReleaseRequiredApiBindingDecl),
}

impl ReleaseRequiredApiDecl {
    pub fn binding_name(&self) -> &str {
        match self {
            Self::ApiId(api_id) => api_id,
            Self::Binding(binding) if binding.name.trim().is_empty() => &binding.api_id,
            Self::Binding(binding) => &binding.name,
        }
    }

    pub fn api_id(&self) -> &str {
        match self {
            Self::ApiId(api_id) => api_id,
            Self::Binding(binding) => &binding.api_id,
        }
    }

    pub fn version_requirement(&self) -> &str {
        match self {
            Self::ApiId(_) => "*",
            Self::Binding(binding) if binding.version.trim().is_empty() => "*",
            Self::Binding(binding) => &binding.version,
        }
    }

    pub fn optional(&self) -> bool {
        matches!(self, Self::Binding(binding) if binding.optional)
    }

    pub fn selection(&self) -> &str {
        match self {
            Self::ApiId(_) => "nearest-healthy",
            Self::Binding(binding) if binding.selection.trim().is_empty() => "nearest-healthy",
            Self::Binding(binding) => &binding.selection,
        }
    }

    pub fn timeout_ms(&self) -> Option<u64> {
        match self {
            Self::ApiId(_) => None,
            Self::Binding(binding) => binding.timeout_ms,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ReleaseRequiredApiBindingDecl {
    #[serde(default)]
    pub name: String,
    #[serde(rename = "id", alias = "api_id")]
    pub api_id: String,
    #[serde(default)]
    pub version: String,
    #[serde(default)]
    pub optional: bool,
    #[serde(default)]
    pub selection: String,
    #[serde(default)]
    pub timeout_ms: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(untagged)]
pub enum ReleaseEventDecl {
    EventId(String),
    Contract(ReleaseEventContractDecl),
}

impl ReleaseEventDecl {
    pub fn event_id(&self) -> &str {
        match self {
            Self::EventId(event_id) => event_id,
            Self::Contract(event) => &event.event_id,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ReleaseEventContractDecl {
    #[serde(rename = "id", alias = "event_id")]
    pub event_id: String,
    #[serde(default)]
    pub version: String,
    #[serde(default)]
    pub schema_ref: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(untagged)]
pub enum ReleaseRequiredEventDecl {
    EventId(String),
    Contract(ReleaseRequiredEventContractDecl),
}

impl ReleaseRequiredEventDecl {
    pub fn event_id(&self) -> &str {
        match self {
            Self::EventId(event_id) => event_id,
            Self::Contract(event) => &event.event_id,
        }
    }

    pub fn consumer_group(&self) -> &str {
        match self {
            Self::EventId(_) => "",
            Self::Contract(event) => &event.consumer_group,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ReleaseRequiredEventContractDecl {
    /// The subscribed event type is its stable event identity. `id` and
    /// `event_id` remain accepted for pre-v2 candidate documents.
    #[serde(rename = "type", alias = "id", alias = "event_id")]
    pub event_id: String,
    #[serde(default)]
    pub version: String,
    #[serde(default)]
    pub optional: bool,
    pub consumer_group: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ReleaseEventsContract {
    #[serde(default)]
    pub publishes: Vec<ReleaseEventDecl>,
    #[serde(default)]
    pub subscribes: Vec<ReleaseRequiredEventDecl>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ReleaseRuntimeContractDecl {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub sha256: String,
    #[serde(default = "default_binding_directory")]
    pub binding_directory: String,
    #[serde(default = "default_identity_mode")]
    pub identity_mode: String,
    #[serde(default = "default_credential_delivery")]
    pub credential_delivery: String,
    #[serde(default)]
    pub restart_on_change: bool,
    /// Explicit compatibility projections for workloads which have not yet
    /// adopted the binding descriptor directory. The map key is a requirement
    /// name and the value is an environment variable name.
    #[serde(default)]
    pub env_projection: BTreeMap<String, String>,
}

impl Default for ReleaseRuntimeContractDecl {
    fn default() -> Self {
        Self {
            id: String::new(),
            sha256: String::new(),
            binding_directory: default_binding_directory(),
            identity_mode: default_identity_mode(),
            credential_delivery: default_credential_delivery(),
            restart_on_change: false,
            env_projection: BTreeMap::new(),
        }
    }
}

fn default_binding_directory() -> String {
    "/run/ojos/service".to_string()
}

fn default_identity_mode() -> String {
    "workload".to_string()
}

fn default_credential_delivery() -> String {
    "file".to_string()
}

fn normalize_legacy_runtime_contract(runtime: &mut ReleaseRuntimeContractDecl) -> Result<()> {
    if runtime.id != LEGACY_STANDARD_RUNTIME_ID {
        return Ok(());
    }
    if runtime.sha256 != LEGACY_STANDARD_RUNTIME_SHA256 {
        return Err(OrchestratorError::InvalidManifest(
            "legacy standard-v1 runtime contract has an unknown digest".to_string(),
        ));
    }
    runtime.id = STANDARD_CONTAINER_RUNTIME_ID.to_string();
    runtime.sha256 = STANDARD_CONTAINER_RUNTIME_SHA256.to_string();
    Ok(())
}

/// Parsed release document retaining v2-only declarations while exposing a
/// normalized v1 release to the existing install pipeline. This avoids a flag
/// day: old `apis`/`required_apis` and new `provides.apis`/`requires.apis` may
/// be read by the same binary, but conflicting duplicate declarations fail.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceReleaseContract {
    pub contract_version: u32,
    pub release: ServiceReleaseManifest,
    pub provides: ReleaseProvidesContract,
    pub requires: ReleaseRequiresContract,
    pub events: ReleaseEventsContract,
    pub runtime_contract: ReleaseRuntimeContractDecl,
    /// Optional Service Contract v3 projection. Its complete contents are
    /// covered by the Catalog v2 metadata digest and Catalog signature.
    pub platform: Option<ReleasePlatformContractV1>,
}

impl ServiceReleaseContract {
    pub fn from_json_value(value: Value) -> Result<Self> {
        let mut object = value.as_object().cloned().ok_or_else(|| {
            OrchestratorError::InvalidManifest(
                "release contract document must be a JSON object".to_string(),
            )
        })?;
        let contract_version = object
            .get("schema_version")
            .and_then(Value::as_u64)
            .unwrap_or(1) as u32;
        if !matches!(contract_version, 1 | SERVICE_CONTRACT_VERSION) {
            return Err(OrchestratorError::InvalidManifest(format!(
                "unsupported release contract schema_version {contract_version}"
            )));
        }

        if contract_version >= SERVICE_CONTRACT_VERSION {
            for section in ["provides", "requires", "events", "runtime_contract"] {
                if !object.contains_key(section) {
                    return Err(OrchestratorError::InvalidManifest(format!(
                        "service contract v2 requires the {section} section"
                    )));
                }
            }
        }

        let provides_value = object.remove("provides");
        let requires_value = object.remove("requires");
        let events_value = object.remove("events");
        let runtime_contract_value = object.remove("runtime_contract");
        let platform_value = object.remove("platform");
        // The normalized release remains schema v1 for the already-published
        // runtime/install contract. The outer document owns the v2 version.
        object.insert("schema_version".to_string(), Value::from(1_u64));
        let mut release: ServiceReleaseManifest = serde_json::from_value(Value::Object(object))?;

        let mut provides = provides_value
            .map(serde_json::from_value)
            .transpose()?
            .unwrap_or_default();
        let mut requires = requires_value
            .map(serde_json::from_value)
            .transpose()?
            .unwrap_or_default();
        let mut events = parse_events(events_value)?;
        let mut runtime_contract = runtime_contract_value
            .map(serde_json::from_value)
            .transpose()?
            .unwrap_or_default();
        normalize_legacy_runtime_contract(&mut runtime_contract)?;
        let platform: Option<ReleasePlatformContractV1> =
            platform_value.map(serde_json::from_value).transpose()?;

        merge_provided_apis(&mut release, &mut provides)?;
        merge_required_apis(&mut release, &mut requires)?;
        merge_events(&provides, &requires, &mut events);
        validate_service_release(&release)?;
        validate_contract_extensions(
            contract_version,
            &requires,
            &provides,
            &events,
            &runtime_contract,
        )?;
        if let Some(platform) = &platform {
            validate_platform_extension(&release, &runtime_contract, platform)?;
        }

        Ok(Self {
            contract_version,
            release,
            provides,
            requires,
            events,
            runtime_contract,
            platform,
        })
    }

    pub fn from_yaml_str(value: &str) -> Result<Self> {
        let yaml: serde_yaml::Value = serde_yaml::from_str(value)?;
        let json = serde_json::to_value(yaml)?;
        Self::from_json_value(json)
    }

    pub fn to_json_value(&self) -> Result<Value> {
        let mut object = serde_json::to_value(&self.release)?
            .as_object()
            .cloned()
            .ok_or_else(|| {
                OrchestratorError::InvalidManifest(
                    "normalized release did not serialize as an object".to_string(),
                )
            })?;
        object.insert(
            "schema_version".to_string(),
            Value::from(self.contract_version),
        );
        if self.contract_version >= SERVICE_CONTRACT_VERSION {
            object.insert(
                "provides".to_string(),
                serde_json::to_value(&self.provides)?,
            );
            object.insert(
                "requires".to_string(),
                serde_json::to_value(&self.requires)?,
            );
            object.insert("events".to_string(), serde_json::to_value(&self.events)?);
            object.insert(
                "runtime_contract".to_string(),
                serde_json::to_value(&self.runtime_contract)?,
            );
        }
        if let Some(platform) = &self.platform {
            object.insert("platform".to_string(), serde_json::to_value(platform)?);
        }
        Ok(Value::Object(object))
    }

    pub fn requirements(&self) -> &[ReleaseRequiredApiDecl] {
        &self.requires.apis
    }
}

fn merge_provided_apis(
    release: &mut ServiceReleaseManifest,
    provides: &mut ReleaseProvidesContract,
) -> Result<()> {
    if provides.apis.is_empty() {
        provides.apis = release
            .apis
            .iter()
            .cloned()
            .map(ReleaseProvidedApiDecl::Legacy)
            .collect();
        return Ok(());
    }
    let mut by_id = release
        .apis
        .iter()
        .cloned()
        .map(|api| (api.api_id.clone(), api))
        .collect::<BTreeMap<_, _>>();
    for api in &provides.apis {
        let surface = api.normalized_surface(release);
        if let Some(existing) = by_id.get(api.api_id()) {
            if existing != &surface {
                return Err(OrchestratorError::InvalidManifest(format!(
                    "provided API {} conflicts with legacy apis declaration",
                    api.api_id()
                )));
            }
        } else {
            release.apis.push(surface.clone());
            by_id.insert(surface.api_id.clone(), surface);
        }
    }
    release
        .apis
        .sort_by(|left, right| left.api_id.cmp(&right.api_id));
    Ok(())
}

fn merge_required_apis(
    release: &mut ServiceReleaseManifest,
    requires: &mut ReleaseRequiresContract,
) -> Result<()> {
    if requires.apis.is_empty() {
        requires.apis = release
            .required_apis
            .iter()
            .cloned()
            .map(ReleaseRequiredApiDecl::ApiId)
            .collect();
        return Ok(());
    }
    let mut ids = release
        .required_apis
        .iter()
        .cloned()
        .collect::<BTreeSet<_>>();
    for requirement in &requires.apis {
        ids.insert(requirement.api_id().to_string());
    }
    release.required_apis = ids.into_iter().collect();
    Ok(())
}

fn parse_events(value: Option<Value>) -> Result<ReleaseEventsContract> {
    let Some(value) = value else {
        return Ok(ReleaseEventsContract::default());
    };
    if value.is_array() {
        return Ok(ReleaseEventsContract {
            publishes: serde_json::from_value(value)?,
            subscribes: Vec::new(),
        });
    }
    Ok(serde_json::from_value(value)?)
}

fn merge_events(
    provides: &ReleaseProvidesContract,
    requires: &ReleaseRequiresContract,
    events: &mut ReleaseEventsContract,
) {
    for event in &provides.events {
        if !events
            .publishes
            .iter()
            .any(|existing| existing.event_id() == event.event_id())
        {
            events.publishes.push(event.clone());
        }
    }
    for event in &requires.events {
        if !events
            .subscribes
            .iter()
            .any(|existing| existing.event_id() == event.event_id())
        {
            events.subscribes.push(event.clone());
        }
    }
    events
        .publishes
        .sort_by(|left, right| left.event_id().cmp(right.event_id()));
    events
        .subscribes
        .sort_by(|left, right| left.event_id().cmp(right.event_id()));
}

fn validate_platform_extension(
    release: &ServiceReleaseManifest,
    runtime: &ReleaseRuntimeContractDecl,
    platform: &ReleasePlatformContractV1,
) -> Result<()> {
    if platform.schema_version != RELEASE_PLATFORM_SCHEMA_VERSION {
        return Err(OrchestratorError::InvalidManifest(format!(
            "platform.schemaVersion must be {RELEASE_PLATFORM_SCHEMA_VERSION}"
        )));
    }
    for (field, digest) in [
        ("platform.contractDigest", platform.contract_digest.as_str()),
        ("platform.sourceDigest", platform.source_digest.as_str()),
        (
            "platform.releaseLockDigest",
            platform.release_lock_digest.as_str(),
        ),
    ] {
        validate_canonical_digest(field, digest)?;
    }

    let stable = Regex::new(r"^[a-zA-Z0-9][a-zA-Z0-9_.:-]*$").expect("valid regex");
    let mut slots = BTreeSet::new();
    for subject in &platform.artifact_subjects {
        if !stable.is_match(&subject.slot) || !slots.insert(subject.slot.as_str()) {
            return Err(OrchestratorError::InvalidManifest(format!(
                "platform artifact slot {} is invalid or duplicated",
                subject.slot
            )));
        }
        if subject.roles.is_empty()
            || subject.media_type.trim().is_empty()
            || subject.size == 0
            || subject.roles.iter().any(|role| !stable.is_match(role))
        {
            return Err(OrchestratorError::InvalidManifest(format!(
                "platform artifact {} has invalid roles, media type, or size",
                subject.slot
            )));
        }
        let mut roles = subject.roles.clone();
        roles.sort();
        roles.dedup();
        if roles != subject.roles {
            return Err(OrchestratorError::InvalidManifest(format!(
                "platform artifact {} roles must be sorted and unique",
                subject.slot
            )));
        }
        validate_canonical_digest("platform.artifact.digest", &subject.digest)?;
        if let Some(reference) = &subject.reference
            && (reference.trim() != reference
                || reference.is_empty()
                || reference.chars().any(char::is_whitespace))
        {
            return Err(OrchestratorError::InvalidManifest(format!(
                "platform artifact {} reference is invalid",
                subject.slot
            )));
        }
    }
    if !slots.contains("contract") || !slots.contains("sbom") || !slots.contains("provenance") {
        return Err(OrchestratorError::InvalidManifest(
            "platform artifact graph must contain contract, sbom, and provenance slots".to_string(),
        ));
    }

    let mut package_ids = BTreeSet::new();
    for package in &platform.package_requirements {
        if !stable.is_match(&package.service_id)
            || !package_ids.insert(package.service_id.as_str())
            || semver::VersionReq::parse(&package.version_requirement).is_err()
        {
            return Err(OrchestratorError::InvalidManifest(format!(
                "platform package requirement {} is invalid or duplicated",
                package.service_id
            )));
        }
    }
    let mut resource_names = BTreeSet::new();
    for resource in &platform.resource_claims {
        if !stable.is_match(&resource.name)
            || !resource_names.insert(resource.name.as_str())
            || resource.resource_type != "postgresql.database/v1"
            || resource.lifecycle != "retain"
        {
            return Err(OrchestratorError::InvalidManifest(format!(
                "platform resource claim {} is invalid, duplicated, or not RETAIN-only PostgreSQL v1",
                resource.name
            )));
        }
    }
    if platform.runtime_volumes.len() > 1 {
        return Err(OrchestratorError::InvalidManifest(
            "platform runtimeVolumes supports at most one volume in v1".to_string(),
        ));
    }
    if !platform.runtime_volumes.is_empty() && runtime.id != STANDARD_CONTAINER_RUNTIME_ID {
        return Err(OrchestratorError::InvalidManifest(
            "platform runtimeVolumes requires standard-container-v1".to_string(),
        ));
    }
    let mut volume_names = BTreeSet::new();
    let mut volume_targets = BTreeSet::new();
    for volume in &platform.runtime_volumes {
        let target = volume.target.as_str();
        let target_reserved = target == "/"
            || ["/run/ojos", "/proc", "/sys", "/dev"]
                .iter()
                .any(|reserved| target == *reserved || target.starts_with(&format!("{reserved}/")));
        if !stable.is_match(&volume.name)
            || !volume_names.insert(volume.name.as_str())
            || !volume_targets.insert(target)
            || volume.kind != "managed-volume"
            || volume.access != "rw"
            || volume.lifecycle != "retain"
            || !target.starts_with('/')
            || (target.len() > 1 && target.ends_with('/'))
            || target.contains("//")
            || target.contains('?')
            || target.contains('#')
            || target_reserved
        {
            return Err(OrchestratorError::InvalidManifest(format!(
                "platform runtime volume {} is invalid, duplicated, or outside the managed RETAIN v1 contract",
                volume.name
            )));
        }
    }
    if let Some(config) = &platform.config_schema {
        validate_canonical_digest("platform.configSchema.digest", &config.digest)?;
        if !config.schema.is_object() {
            return Err(OrchestratorError::InvalidManifest(
                "platform.configSchema.schema must be an object".to_string(),
            ));
        }
    }

    // Reuse the Contribution domain constructor as the authoritative content
    // validator. A deterministic dummy deployment identity is sufficient: the
    // signed Release stores contribution content, while installation binds it
    // to the real deployment/generation and computes the final revision ID.
    ContributionRevisionV1::stage(
        "release-validation",
        "release-validation",
        release.service_name.clone(),
        platform.release_lock_digest.clone(),
        platform.contract_digest.clone(),
        1,
        None,
        platform.contribution.api_surfaces.clone(),
        platform.contribution.operation_routes.clone(),
        platform.contribution.permission_definitions.clone(),
        platform.contribution.user_frontend_modules.clone(),
        platform.contribution.admin_frontend_modules.clone(),
    )
    .map_err(|error| {
        OrchestratorError::InvalidManifest(format!(
            "platform contribution content is invalid: {error}"
        ))
    })?;
    Ok(())
}

fn validate_canonical_digest(field: &str, value: &str) -> Result<()> {
    let hex = value.strip_prefix("sha256:").unwrap_or_default();
    if hex.len() != 64
        || !hex
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(OrchestratorError::InvalidManifest(format!(
            "{field} must be sha256:<64 lowercase hex>"
        )));
    }
    Ok(())
}

fn validate_contract_extensions(
    contract_version: u32,
    requires: &ReleaseRequiresContract,
    provides: &ReleaseProvidesContract,
    events: &ReleaseEventsContract,
    runtime: &ReleaseRuntimeContractDecl,
) -> Result<()> {
    let key = Regex::new(r"^[a-zA-Z0-9][a-zA-Z0-9_.:-]*$").expect("valid regex");
    let env = Regex::new(r"^[A-Z_][A-Z0-9_]*$").expect("valid regex");
    let mut names = BTreeSet::new();
    for requirement in &requires.apis {
        if !key.is_match(requirement.binding_name()) || !key.is_match(requirement.api_id()) {
            return Err(OrchestratorError::InvalidManifest(
                "required API name and api_id must be stable identifiers".to_string(),
            ));
        }
        if !names.insert(requirement.binding_name()) {
            return Err(OrchestratorError::InvalidManifest(format!(
                "duplicate required API binding {}",
                requirement.binding_name()
            )));
        }
        if !matches!(
            requirement.selection(),
            "nearest-healthy" | "same-node" | "explicit"
        ) {
            return Err(OrchestratorError::InvalidManifest(format!(
                "required API {} has unsupported selection policy {}",
                requirement.binding_name(),
                requirement.selection()
            )));
        }
        if requirement
            .timeout_ms()
            .is_some_and(|timeout| timeout == 0 || timeout > 300_000)
        {
            return Err(OrchestratorError::InvalidManifest(format!(
                "required API {} timeout_ms must be between 1 and 300000",
                requirement.binding_name()
            )));
        }
        if contract_version >= SERVICE_CONTRACT_VERSION
            && let ReleaseRequiredApiDecl::Binding(binding) = requirement
        {
            if binding.name.trim().is_empty() {
                return Err(OrchestratorError::InvalidManifest(
                    "required API name is required for Service Contract v2".to_string(),
                ));
            }
            if binding.version.trim().is_empty() {
                return Err(OrchestratorError::InvalidManifest(format!(
                    "required API {} version is required for Service Contract v2",
                    binding.name
                )));
            }
            if binding.timeout_ms.is_none() {
                return Err(OrchestratorError::InvalidManifest(format!(
                    "required API {} timeout_ms is required for Service Contract v2",
                    binding.name
                )));
            }
        }
        if contract_version >= SERVICE_CONTRACT_VERSION
            && !valid_api_version_requirement(requirement.version_requirement())
        {
            return Err(OrchestratorError::InvalidManifest(format!(
                "required API {} version must be a SemVer requirement",
                requirement.binding_name()
            )));
        }
    }

    for provided in &provides.apis {
        if !key.is_match(provided.api_id()) {
            return Err(OrchestratorError::InvalidManifest(format!(
                "provided API id {} is invalid",
                provided.api_id()
            )));
        }
        if let ReleaseProvidedApiDecl::Contract(api) = provided {
            if semver::Version::parse(api.version.trim()).is_err() {
                return Err(OrchestratorError::InvalidManifest(format!(
                    "provided API {} version must be SemVer",
                    api.id
                )));
            }
            if !api.path.starts_with('/') {
                return Err(OrchestratorError::InvalidManifest(format!(
                    "provided API {} path must start with /",
                    api.id
                )));
            }
            if !matches!(api.auth.mode(), "public" | "user" | "service" | "workload") {
                return Err(OrchestratorError::InvalidManifest(format!(
                    "provided API {} auth must be public, user, service, or workload",
                    api.id
                )));
            }
            if api.permission.trim().is_empty() {
                return Err(OrchestratorError::InvalidManifest(format!(
                    "provided API {} permission is required",
                    api.id
                )));
            }
        }
    }

    let mut published_event_ids = BTreeSet::new();
    for event in &events.publishes {
        let event_id = event.event_id();
        if !key.is_match(event_id) {
            return Err(OrchestratorError::InvalidManifest(format!(
                "event id {event_id} is invalid"
            )));
        }
        if !published_event_ids.insert(event_id) {
            return Err(OrchestratorError::InvalidManifest(format!(
                "duplicate published event {event_id}"
            )));
        }
        if let ReleaseEventDecl::Contract(event) = event
            && semver::Version::parse(event.version.trim()).is_err()
        {
            return Err(OrchestratorError::InvalidManifest(format!(
                "published event {} version must be SemVer",
                event.event_id
            )));
        }
    }
    let mut subscribed_event_ids = BTreeSet::new();
    for event in &events.subscribes {
        let event_id = event.event_id();
        if !key.is_match(event_id) {
            return Err(OrchestratorError::InvalidManifest(format!(
                "event id {event_id} is invalid"
            )));
        }
        if !subscribed_event_ids.insert(event_id) {
            return Err(OrchestratorError::InvalidManifest(format!(
                "duplicate subscribed event {event_id}"
            )));
        }
        if let ReleaseRequiredEventDecl::Contract(event) = event {
            if !event.version.trim().is_empty()
                && semver::Version::parse(event.version.trim()).is_err()
            {
                return Err(OrchestratorError::InvalidManifest(format!(
                    "subscribed event {} version must be SemVer",
                    event.event_id
                )));
            }
            if event.consumer_group.trim().is_empty() {
                return Err(OrchestratorError::InvalidManifest(format!(
                    "event subscriber {} requires consumer_group",
                    event.event_id
                )));
            }
        }
    }
    if contract_version >= SERVICE_CONTRACT_VERSION {
        if runtime.id.trim().is_empty() {
            return Err(OrchestratorError::InvalidManifest(
                "runtime_contract.id is required for Service Contract v2".to_string(),
            ));
        }
        if !matches!(
            runtime.id.as_str(),
            STANDARD_CONTAINER_RUNTIME_ID | "judge-sandbox-v1"
        ) {
            return Err(OrchestratorError::InvalidManifest(format!(
                "runtime_contract.id {} is not a published runtime profile",
                runtime.id
            )));
        }
        let digest = runtime.sha256.strip_prefix("sha256:").unwrap_or_default();
        if digest.len() != 64
            || !digest
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err(OrchestratorError::InvalidManifest(
                "runtime_contract.sha256 must be sha256:<64 lowercase hex>".to_string(),
            ));
        }
    }
    if !runtime.binding_directory.starts_with('/') {
        return Err(OrchestratorError::InvalidManifest(
            "runtime_contract.binding_directory must be absolute".to_string(),
        ));
    }
    if !matches!(runtime.identity_mode.as_str(), "workload" | "none") {
        return Err(OrchestratorError::InvalidManifest(
            "runtime_contract.identity_mode must be workload or none".to_string(),
        ));
    }
    if !matches!(runtime.credential_delivery.as_str(), "file" | "local-proxy") {
        return Err(OrchestratorError::InvalidManifest(
            "runtime_contract.credential_delivery must be file or local-proxy".to_string(),
        ));
    }
    for (binding, variable) in &runtime.env_projection {
        if !names.contains(binding.as_str()) {
            return Err(OrchestratorError::InvalidManifest(format!(
                "runtime_contract env projection references unknown binding {binding}"
            )));
        }
        if !env.is_match(variable) {
            return Err(OrchestratorError::InvalidManifest(format!(
                "runtime_contract env projection {binding} has invalid environment name"
            )));
        }
    }
    Ok(())
}

fn valid_api_version_requirement(value: &str) -> bool {
    let value = value.trim();
    if value == "*" {
        return true;
    }
    if semver::VersionReq::parse(value).is_ok() || semver::Version::parse(value).is_ok() {
        return true;
    }
    value
        .strip_prefix('v')
        .is_some_and(|value| semver::Version::parse(value).is_ok())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn base_release() -> Value {
        json!({
            "schema_version": 1,
            "service_name": "consumer",
            "version": "1.0.0",
            "description": "consumer",
            "service_type": "backend-api",
            "source": {"kind": "url", "url": "https://example.invalid/release.yaml"},
            "runtime": {"kind": "image", "image": "example.invalid/consumer@sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"},
            "backend": {"protocol": "http", "port": 8080},
            "permissions": [],
            "apis": [],
            "required_apis": ["storage.object.get"]
        })
    }

    #[test]
    fn v1_release_normalizes_into_v2_views() {
        let contract = ServiceReleaseContract::from_json_value(base_release()).unwrap();
        assert_eq!(contract.contract_version, 1);
        assert_eq!(contract.requirements()[0].api_id(), "storage.object.get");
        assert_eq!(
            contract.requirements()[0].binding_name(),
            "storage.object.get"
        );
    }

    #[test]
    fn v2_release_merges_provided_and_required_apis() {
        let mut value = base_release();
        value["schema_version"] = json!(2);
        value["permissions"] = json!(["consumer.status.read"]);
        value["provides"] = json!({
            "apis": [{
                "id": "consumer.status.get",
                "version": "1.0.0",
                "path": "/status",
                "methods": ["GET"],
                "auth": "workload",
                "permission": "consumer.status.read"
            }]
        });
        value["requires"] = json!({
            "apis": [{
                "name": "STORAGE_GET",
                "id": "storage.object.get",
                "version": ">=1.0.0, <2.0.0",
                "selection": "nearest-healthy",
                "timeout_ms": 5000
            }]
        });
        value["events"] = json!({
            "publishes": [{"id": "consumer.started", "version": "1.0.0"}],
            "subscribes": [{
                "type": "storage.updated",
                "consumer_group": "consumer-v1"
            }]
        });
        value["runtime_contract"] = json!({
            "id": "judge-sandbox-v1",
            "sha256": "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            "binding_directory": "/run/ojos/service",
            "identity_mode": "workload",
            "credential_delivery": "file",
            "env_projection": {"STORAGE_GET": "OJOS_STORAGE_GET_BINDING"}
        });
        let contract = ServiceReleaseContract::from_json_value(value).unwrap();
        assert_eq!(contract.contract_version, 2);
        assert_eq!(contract.release.apis.len(), 1);
        assert_eq!(contract.requirements()[0].binding_name(), "STORAGE_GET");
        assert_eq!(contract.events.publishes[0].event_id(), "consumer.started");
        assert_eq!(contract.events.subscribes[0].event_id(), "storage.updated");

        let canonical = contract.to_json_value().unwrap();
        assert_eq!(canonical["schema_version"], 2);
        assert_eq!(
            canonical["provides"]["apis"][0]["id"],
            "consumer.status.get"
        );
        assert_eq!(canonical["requires"]["apis"][0]["id"], "storage.object.get");
        assert_eq!(
            canonical["events"]["subscribes"][0]["type"],
            "storage.updated"
        );
        assert_eq!(canonical["runtime_contract"]["id"], "judge-sandbox-v1");

        let reparsed = ServiceReleaseContract::from_json_value(canonical.clone()).unwrap();
        assert_eq!(reparsed.to_json_value().unwrap(), canonical);
    }

    #[test]
    fn formal_v2_yaml_is_accepted_without_legacy_contract_fields() {
        let yaml = r#"
schema_version: 2
service_name: consumer
version: 1.0.0
description: consumer
service_type: backend-api
source:
  kind: url
  url: https://example.invalid/release.yaml
runtime:
  kind: image
  image: example.invalid/consumer@sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa
backend:
  protocol: http
  port: 8080
permissions:
  - consumer.status.read
provides:
  apis:
    - id: consumer.status.get
      version: 1.0.0
      path: /status
      methods: [GET]
      auth:
        mode: workload
      permission: consumer.status.read
requires:
  apis:
    - name: STORAGE_GET
      id: storage.object.get
      version: ">=1.0.0, <2.0.0"
      timeout_ms: 5000
events:
  publishes:
    - id: consumer.started
      version: 1.0.0
  subscribes:
    - type: storage.updated
      consumer_group: consumer-v1
runtime_contract:
  id: judge-sandbox-v1
  sha256: sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb
"#;
        let contract = ServiceReleaseContract::from_yaml_str(yaml).unwrap();
        assert_eq!(contract.release.apis[0].api_id, "consumer.status.get");
        assert_eq!(contract.release.apis[0].auth_mode, "workload");
        assert_eq!(contract.requirements()[0].timeout_ms(), Some(5000));
        assert_eq!(contract.runtime_contract.id, "judge-sandbox-v1");
    }

    #[test]
    fn checked_in_v2_yaml_has_a_stable_formal_projection_and_parseable_schema() {
        let yaml = include_str!("../tests/fixtures/service-contract-v2.yaml");
        let expected: Value = serde_json::from_str(include_str!(
            "../tests/fixtures/service-contract-v2.canonical.json"
        ))
        .unwrap();
        let contract = ServiceReleaseContract::from_yaml_str(yaml).unwrap();
        let document = contract.to_json_value().unwrap();
        let projection = json!({
            "provides": document["provides"],
            "requires": document["requires"],
            "events": document["events"],
            "runtime_contract": document["runtime_contract"],
        });
        assert_eq!(projection, expected);

        let schema: Value = serde_json::from_str(include_str!(
            "../../../../platform/schemas/orchestrator/service-contract-v2.schema.json"
        ))
        .unwrap();
        assert_eq!(schema["properties"]["schema_version"]["const"], 2);
        assert_eq!(
            schema["$defs"]["runtimeContract"]["required"],
            json!(["id", "sha256"])
        );
        assert_eq!(
            schema["$defs"]["subscribedEvent"]["required"],
            json!(["type", "consumer_group"])
        );
    }

    #[test]
    fn v2_required_api_needs_name_version_and_timeout() {
        let mut value = base_release();
        value["schema_version"] = json!(2);
        value["provides"] = json!({"apis": []});
        value["requires"] = json!({"apis": [{
            "id": "storage.object.get",
            "version": "1.0.0"
        }]});
        value["events"] = json!({"publishes": [], "subscribes": []});
        value["runtime_contract"] = json!({
            "id": "standard-container-v1",
            "sha256": "sha256:56c8ec1e421205dbebb97ad40cbda30bf468d198dd8c3fc50151e39465ea573f"
        });
        let error = ServiceReleaseContract::from_json_value(value).unwrap_err();
        assert!(error.to_string().contains("required API name is required"));
    }

    #[test]
    fn v2_requires_every_formal_contract_section() {
        for missing in ["provides", "requires", "events", "runtime_contract"] {
            let mut value = base_release();
            value["schema_version"] = json!(2);
            value["provides"] = json!({"apis": []});
            value["requires"] = json!({"apis": []});
            value["events"] = json!({"publishes": [], "subscribes": []});
            value["runtime_contract"] = json!({
                "id": "standard-container-v1",
                "sha256": STANDARD_CONTAINER_RUNTIME_SHA256
            });
            value.as_object_mut().unwrap().remove(missing);

            let error = ServiceReleaseContract::from_json_value(value).unwrap_err();
            assert!(
                error
                    .to_string()
                    .contains(&format!("requires the {missing} section")),
                "unexpected error for missing {missing}: {error}"
            );
        }
    }
}
