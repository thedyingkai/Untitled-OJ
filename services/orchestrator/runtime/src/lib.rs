//! Runtime drivers used by node agents.
//!
//! The production container implementation talks to the Docker Engine API
//! directly.  No request field is interpolated into a shell command.

use async_trait::async_trait;
use bollard::Docker;
use bollard::auth::DockerCredentials;
use bollard::models::{ContainerCreateBody, HostConfig, PortBinding};
use bollard::query_parameters::{
    CreateContainerOptionsBuilder, CreateImageOptionsBuilder, ImportImageOptionsBuilder,
    RemoveContainerOptionsBuilder, RestartContainerOptionsBuilder, StopContainerOptionsBuilder,
    WaitContainerOptionsBuilder,
};
use futures_util::TryStreamExt;
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::fmt::{Display, Formatter};
use std::path::Path;
use std::sync::OnceLock;
use thiserror::Error;

pub const DEFAULT_HEALTH_TIMEOUT_MS: u64 = 60_000;
pub const DEFAULT_HEALTH_POLL_INTERVAL_MS: u64 = 1_000;
pub const DEFAULT_COMPENSATION_TIMEOUT_MS: u64 = 30_000;
pub const MAX_HEALTH_TIMEOUT_MS: u64 = 10 * 60_000;
pub const MAX_COMPENSATION_TIMEOUT_MS: u64 = 60_000;
const MAX_REGISTRY_CREDENTIALS_BYTES: u64 = 64 * 1024;
const MAX_REGISTRY_CREDENTIALS: usize = 32;

#[derive(Debug, Error)]
pub enum RuntimeError {
    #[error("invalid OCI image reference: {0}")]
    InvalidImageReference(String),
    #[error("invalid container health policy: {0}")]
    InvalidHealthPolicy(String),
    #[error("invalid release replacement payload: {0}")]
    InvalidReleaseReplacement(String),
    #[error("invalid published endpoint: {0}")]
    InvalidPublishedEndpoint(String),
    #[error("invalid Docker registry credentials: {0}")]
    InvalidRegistryCredentials(String),
    #[error("docker engine is unavailable: {0}")]
    EngineUnavailable(String),
    #[error("docker operation failed: {0}")]
    Engine(String),
    #[error("pulled image did not expose requested digest {requested}; found {actual:?}")]
    DigestMismatch {
        requested: String,
        actual: Vec<String>,
    },
    #[error("runtime instance does not contain a container id")]
    MissingContainerId,
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct DockerRegistryCredential {
    server_address: String,
    username: String,
    password: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DockerRegistryCredentialsDocument {
    schema_version: u32,
    registries: Vec<DockerRegistryCredential>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct OciImageReference {
    repository: String,
    digest: String,
}

impl<'de> Deserialize<'de> for OciImageReference {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct WireReference {
            repository: String,
            digest: String,
        }

        let wire = WireReference::deserialize(deserializer)?;
        Self::parse(&format!("{}@{}", wire.repository, wire.digest))
            .map_err(serde::de::Error::custom)
    }
}

impl OciImageReference {
    pub fn parse(value: &str) -> Result<Self, RuntimeError> {
        static REPOSITORY: OnceLock<Regex> = OnceLock::new();
        static DIGEST: OnceLock<Regex> = OnceLock::new();
        let repository_re = REPOSITORY.get_or_init(|| {
            Regex::new(r"^[a-zA-Z0-9][a-zA-Z0-9._/-]*(?::[0-9]+)?/[a-zA-Z0-9][a-zA-Z0-9._/-]*$")
                .expect("repository regex is valid")
        });
        let digest_re = DIGEST
            .get_or_init(|| Regex::new(r"^sha256:[0-9a-f]{64}$").expect("digest regex is valid"));
        let (repository, digest) = value.split_once('@').ok_or_else(|| {
            RuntimeError::InvalidImageReference(
                "production images must use repository@sha256:<64 lowercase hex>".to_string(),
            )
        })?;
        if value.matches('@').count() != 1
            || !repository_re.is_match(repository)
            || !digest_re.is_match(digest)
        {
            return Err(RuntimeError::InvalidImageReference(value.to_string()));
        }
        // A colon after the last slash is a mutable tag, not a registry port.
        if repository
            .rsplit_once('/')
            .is_some_and(|(_, name)| name.contains(':'))
        {
            return Err(RuntimeError::InvalidImageReference(
                "tags cannot be combined with the production digest reference".to_string(),
            ));
        }
        Ok(Self {
            repository: repository.to_string(),
            digest: digest.to_string(),
        })
    }

    pub fn repository(&self) -> &str {
        &self.repository
    }

    pub fn digest(&self) -> &str {
        &self.digest
    }
}

impl Display for OciImageReference {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}@{}", self.repository, self.digest)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum PublishedPortProtocol {
    Tcp,
}

impl PublishedPortProtocol {
    fn docker_name(&self) -> &'static str {
        match self {
            Self::Tcp => "tcp",
        }
    }
}

/// A Store-validated endpoint advertised by one managed runtime instance.
///
/// `endpoint` is the public `ip:port:service-id` identity persisted by the
/// control plane. Docker binds the typed container port to `host_port` on all
/// interfaces of the Engine namespace; the public host is deliberately not a
/// Docker bind address because nested/remote Engines do not own the Node's
/// advertised host IP.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PublishedEndpoint {
    pub endpoint: String,
    pub application_protocol: String,
    pub container_port: u16,
    pub host_port: u16,
    pub transport_protocol: PublishedPortProtocol,
}

impl PublishedEndpoint {
    pub fn validate(&self) -> Result<(), RuntimeError> {
        if self.endpoint.trim().is_empty() {
            return Err(RuntimeError::InvalidPublishedEndpoint(
                "endpoint is required".to_string(),
            ));
        }
        if self.container_port == 0 || self.host_port == 0 {
            return Err(RuntimeError::InvalidPublishedEndpoint(
                "container_port and host_port must be positive".to_string(),
            ));
        }
        let protocol = self.application_protocol.as_bytes();
        if protocol.is_empty()
            || !protocol[0].is_ascii_lowercase()
            || !protocol.iter().all(|byte| {
                byte.is_ascii_lowercase()
                    || byte.is_ascii_digit()
                    || matches!(byte, b'-' | b'.' | b'+')
            })
        {
            return Err(RuntimeError::InvalidPublishedEndpoint(
                "application_protocol must be a lowercase protocol token".to_string(),
            ));
        }
        Ok(())
    }

    fn docker_port(&self) -> String {
        format!(
            "{}/{}",
            self.container_port,
            self.transport_protocol.docker_name()
        )
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ContainerSpec {
    pub deployment_id: String,
    pub service_id: String,
    pub generation: u64,
    pub image: OciImageReference,
    #[serde(default)]
    pub command: Vec<String>,
    #[serde(default)]
    pub environment: Vec<String>,
    #[serde(default)]
    pub labels: HashMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub published_endpoint: Option<PublishedEndpoint>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum RuntimeDesiredState {
    Running,
    Stopped,
    Removed,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum RuntimeObservedState {
    Created,
    Running,
    Stopped,
    Exited,
    Missing,
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RuntimeInstance {
    pub deployment_id: String,
    pub service_id: String,
    /// Exact signed Release version that produced this instance. Legacy
    /// projections deserialize as empty and are rejected by Topology reference
    /// validation until they are deterministically rebound or reprovisioned.
    #[serde(default)]
    pub release_version: String,
    pub container_id: String,
    pub artifact_digest: String,
    pub desired_state: RuntimeDesiredState,
    pub observed_state: RuntimeObservedState,
    pub health: String,
}

/// Shared wire payload for an ordinary managed install.  It deliberately
/// lives beside the runtime contracts so the control plane and Agent decode
/// one schema instead of maintaining look-alike private structs.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RuntimeInstallPayload {
    pub spec: ContainerSpec,
    #[serde(default = "default_true")]
    pub start: bool,
    #[serde(default)]
    pub health_gate: HealthGatePolicy,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub offline_oci_artifact: Option<ArtifactReference>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ArtifactReference {
    /// Content-addressed identifier served only through the authenticated
    /// Agent protocol. Job payloads never contain artifact bytes.
    pub artifact_id: String,
    pub sha256: String,
    pub size_bytes: u64,
    #[serde(default = "default_artifact_chunk_bytes")]
    pub chunk_bytes: u32,
}

const fn default_artifact_chunk_bytes() -> u32 {
    1024 * 1024
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AuthServiceIdentitySpec {
    pub service_name: String,
    #[serde(default)]
    pub allowed_apis: Vec<String>,
    #[serde(default)]
    pub grants: Vec<AuthServiceIdentityGrantSpec>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AuthServiceIdentityGrantSpec {
    pub api_id: String,
    pub permission: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AuthPipelineStep {
    pub service_name: String,
    #[serde(default)]
    pub permissions: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub service_identity: Option<AuthServiceIdentitySpec>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct GatewayRouteSpec {
    pub route_id: String,
    pub path_prefix: String,
    pub upstream_base: String,
    #[serde(default)]
    pub methods: Vec<String>,
    pub auth_mode: String,
    #[serde(default)]
    pub required_permission: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct GatewayPipelineStep {
    pub operation_id: String,
    pub service_name: String,
    pub node_id: String,
    #[serde(default)]
    pub routes: Vec<GatewayRouteSpec>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RuntimeMaterializationStep {
    #[serde(default)]
    pub config: BTreeMap<String, String>,
    /// Maps the manifest secret name to a Node-local provider reference. The
    /// secret value itself is never persisted in the control-plane Job.
    #[serde(default)]
    pub secret_refs: BTreeMap<String, String>,
    #[serde(default)]
    pub environment_templates: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "provider", rename_all = "snake_case", deny_unknown_fields)]
pub enum TypedProvisionerStep {
    Redis {
        service_name: String,
        resources: Vec<RedisNamespaceSpec>,
    },
    Storage {
        service_name: String,
        resources: Vec<StorageResourceSpec>,
    },
    ApiRegistry {
        service_name: String,
        #[serde(default = "default_provider_connection_id")]
        registry_id: String,
        apis: Vec<ApiSurfaceSpec>,
        required_apis: Vec<String>,
    },
    Frontend {
        service_name: String,
        #[serde(default = "default_provider_connection_id")]
        asset_store_id: String,
        version: String,
        route_prefix: String,
        remote_entry: String,
        metadata_source_url: String,
        metadata_sha256: String,
    },
}

impl TypedProvisionerStep {
    pub fn provider_name(&self) -> &'static str {
        match self {
            Self::Redis { .. } => "redis",
            Self::Storage { .. } => "storage",
            Self::ApiRegistry { .. } => "api_registry",
            Self::Frontend { .. } => "frontend",
        }
    }

    pub fn service_name(&self) -> &str {
        match self {
            Self::Redis { service_name, .. }
            | Self::Storage { service_name, .. }
            | Self::ApiRegistry { service_name, .. }
            | Self::Frontend { service_name, .. } => service_name,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RedisNamespaceSpec {
    pub name: String,
    pub kind: String,
    #[serde(default = "default_provider_connection_id")]
    pub connection_id: String,
    pub namespace: String,
    pub consumer_group: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct StorageResourceSpec {
    pub object_type: String,
    pub bucket: String,
    pub prefix: String,
    #[serde(default = "default_storage_backend")]
    pub backend: String,
    #[serde(default = "default_provider_connection_id")]
    pub connection_id: String,
}

fn default_provider_connection_id() -> String {
    "default".to_string()
}

fn default_storage_backend() -> String {
    "node_directory".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ApiSurfaceSpec {
    pub api_id: String,
    pub protocol: String,
    pub path_prefix: String,
    #[serde(default)]
    pub methods: Vec<String>,
    pub visibility: String,
    pub auth_mode: String,
    pub permission: String,
    pub version: String,
}

/// Signed, immutable one-shot migration declaration sent to the selected
/// Node.  The Agent ledger keys the durable outcome by service/version and
/// refuses a checksum or image mismatch.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct OciMigrationStep {
    pub service_name: String,
    pub version: String,
    pub checksum: String,
    pub image: OciImageReference,
    #[serde(default)]
    pub command: Vec<String>,
    #[serde(default)]
    pub environment: Vec<String>,
    pub timeout_ms: u64,
    #[serde(default)]
    pub dry_run: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ReleasePipelinePayload {
    pub install: RuntimeInstallPayload,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub materialization: Option<RuntimeMaterializationStep>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth: Option<AuthPipelineStep>,
    #[serde(default)]
    pub provisioners: Vec<TypedProvisionerStep>,
    #[serde(default)]
    pub migrations: Vec<OciMigrationStep>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gateway: Option<GatewayPipelineStep>,
}

/// Complete declarative provider state owned by one signed Release revision.
/// Replacement jobs carry both sides so compensation restores the proven old
/// state instead of deleting resources that the old deployment still needs.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ReleaseProviderRevision {
    pub revision_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth: Option<AuthPipelineStep>,
    #[serde(default)]
    pub provisioners: Vec<TypedProvisionerStep>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gateway: Option<GatewayPipelineStep>,
}

impl ReleaseProviderRevision {
    pub fn has_managed_state(&self) -> bool {
        self.auth.is_some() || !self.provisioners.is_empty() || self.gateway.is_some()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ReplacementProviderSaga {
    pub previous: ReleaseProviderRevision,
    pub desired: ReleaseProviderRevision,
}

/// Successful result of an atomic single-node release replacement. The
/// control plane can update both runtime projections in one database
/// transaction from this self-contained value.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RuntimeReplacement {
    pub instance: RuntimeInstance,
    pub replaced_deployment_id: String,
    pub replaced_container_id: String,
}

/// Defines whether an image without a Docker `HEALTHCHECK` can satisfy an
/// install's health gate. Production callers must serialize this policy into
/// the install job so the control plane and Agent make the same decision.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MissingHealthcheckPolicy {
    #[default]
    Reject,
    AllowRunning,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct HealthGatePolicy {
    #[serde(default = "default_health_timeout_ms")]
    pub timeout_ms: u64,
    #[serde(default = "default_health_poll_interval_ms")]
    pub poll_interval_ms: u64,
    #[serde(default)]
    pub missing_healthcheck: MissingHealthcheckPolicy,
    #[serde(default = "default_compensation_timeout_ms")]
    pub compensation_timeout_ms: u64,
}

impl Default for HealthGatePolicy {
    fn default() -> Self {
        Self {
            timeout_ms: DEFAULT_HEALTH_TIMEOUT_MS,
            poll_interval_ms: DEFAULT_HEALTH_POLL_INTERVAL_MS,
            missing_healthcheck: MissingHealthcheckPolicy::Reject,
            compensation_timeout_ms: DEFAULT_COMPENSATION_TIMEOUT_MS,
        }
    }
}

impl HealthGatePolicy {
    pub fn validate(&self) -> Result<(), RuntimeError> {
        if self.timeout_ms == 0 || self.timeout_ms > MAX_HEALTH_TIMEOUT_MS {
            return Err(RuntimeError::InvalidHealthPolicy(format!(
                "timeout_ms must be between 1 and {MAX_HEALTH_TIMEOUT_MS}"
            )));
        }
        if self.poll_interval_ms == 0 || self.poll_interval_ms > self.timeout_ms {
            return Err(RuntimeError::InvalidHealthPolicy(
                "poll_interval_ms must be positive and no greater than timeout_ms".to_string(),
            ));
        }
        if self.compensation_timeout_ms == 0
            || self.compensation_timeout_ms > MAX_COMPENSATION_TIMEOUT_MS
        {
            return Err(RuntimeError::InvalidHealthPolicy(format!(
                "compensation_timeout_ms must be between 1 and {MAX_COMPENSATION_TIMEOUT_MS}"
            )));
        }
        Ok(())
    }
}

/// Wire payload shared by Upgrade and Rollback jobs. `start=false` is
/// represented for forward-compatible decoding but rejected by `validate` in
/// v1 because a stopped replacement cannot pass the cutover health gate.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ReleaseReplacementPayload {
    pub old_deployment_id: String,
    pub old_container_id: String,
    pub new_spec: ContainerSpec,
    #[serde(default = "default_true")]
    pub start: bool,
    #[serde(default)]
    pub health_gate: HealthGatePolicy,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub offline_oci_artifact: Option<ArtifactReference>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub materialization: Option<RuntimeMaterializationStep>,
    #[serde(default)]
    pub migrations: Vec<OciMigrationStep>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_saga: Option<ReplacementProviderSaga>,
}

impl ReleaseReplacementPayload {
    pub fn validate(&self) -> Result<(), RuntimeError> {
        if self.old_deployment_id.trim().is_empty()
            || self.old_container_id.trim().is_empty()
            || self.new_spec.deployment_id.trim().is_empty()
        {
            return Err(RuntimeError::InvalidReleaseReplacement(
                "old_deployment_id, old_container_id, and new_spec.deployment_id are required"
                    .to_string(),
            ));
        }
        if self.old_deployment_id == self.new_spec.deployment_id {
            return Err(RuntimeError::InvalidReleaseReplacement(
                "new_spec.deployment_id must differ from old_deployment_id so both containers can coexist before cutover"
                    .to_string(),
            ));
        }
        if !self.start {
            return Err(RuntimeError::InvalidReleaseReplacement(
                "start must be true because cutover is gated on new-instance health".to_string(),
            ));
        }
        self.health_gate.validate()?;
        let mut migration_versions = BTreeMap::new();
        for migration in &self.migrations {
            if migration.service_name != self.new_spec.service_id
                || migration.version.trim().is_empty()
                || migration_versions
                    .insert(migration.version.as_str(), ())
                    .is_some()
            {
                return Err(RuntimeError::InvalidReleaseReplacement(
                    "migration service_name must match new_spec.service_id and versions must be unique"
                        .to_string(),
                ));
            }
        }
        if let Some(saga) = &self.provider_saga {
            if saga.previous.revision_id.trim().is_empty()
                || saga.desired.revision_id.trim().is_empty()
                || saga.previous.revision_id == saga.desired.revision_id
            {
                return Err(RuntimeError::InvalidReleaseReplacement(
                    "provider saga requires distinct non-empty previous and desired revision ids"
                        .to_string(),
                ));
            }
            let service_id = self.new_spec.service_id.as_str();
            for revision in [&saga.previous, &saga.desired] {
                if let Some(auth) = &revision.auth
                    && auth.service_name != service_id
                {
                    return Err(RuntimeError::InvalidReleaseReplacement(
                        "provider auth state must match new_spec.service_id".to_string(),
                    ));
                }
                if let Some(gateway) = &revision.gateway
                    && gateway.service_name != service_id
                {
                    return Err(RuntimeError::InvalidReleaseReplacement(
                        "provider Gateway state must match new_spec.service_id".to_string(),
                    ));
                }
                let mut provider_names = BTreeMap::new();
                for provisioner in &revision.provisioners {
                    if provisioner.service_name() != service_id {
                        return Err(RuntimeError::InvalidReleaseReplacement(
                            "provider state must match new_spec.service_id".to_string(),
                        ));
                    }
                    if provider_names
                        .insert(provisioner.provider_name(), ())
                        .is_some()
                    {
                        return Err(RuntimeError::InvalidReleaseReplacement(format!(
                            "provider revision contains duplicate {} state",
                            provisioner.provider_name()
                        )));
                    }
                }
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HealthGateDecision {
    Ready,
    Pending(String),
    Failed(String),
}

/// Evaluates one Docker inspection result without performing I/O. `NONE`
/// explicitly means Docker reported no healthcheck; `UNKNOWN` remains
/// unproven and is allowed to wait until the bounded deadline.
pub fn evaluate_health_gate(
    instance: &RuntimeInstance,
    policy: &HealthGatePolicy,
) -> HealthGateDecision {
    if instance.observed_state != RuntimeObservedState::Running {
        return match instance.observed_state {
            RuntimeObservedState::Exited
            | RuntimeObservedState::Stopped
            | RuntimeObservedState::Missing => HealthGateDecision::Failed(format!(
                "container is {:?}, not RUNNING",
                instance.observed_state
            )),
            RuntimeObservedState::Created | RuntimeObservedState::Unknown => {
                HealthGateDecision::Pending(format!(
                    "container is {:?}, waiting for RUNNING",
                    instance.observed_state
                ))
            }
            RuntimeObservedState::Running => unreachable!("RUNNING was handled above"),
        };
    }

    match instance.health.trim().to_ascii_uppercase().as_str() {
        "HEALTHY" => HealthGateDecision::Ready,
        "STARTING" | "UNKNOWN" | "" => HealthGateDecision::Pending(format!(
            "container health is {}",
            normalized_health(&instance.health)
        )),
        "NONE" if policy.missing_healthcheck == MissingHealthcheckPolicy::AllowRunning => {
            HealthGateDecision::Ready
        }
        "NONE" => HealthGateDecision::Failed(
            "image has no Docker HEALTHCHECK and policy requires one".to_string(),
        ),
        "UNHEALTHY" => HealthGateDecision::Failed("Docker health status is UNHEALTHY".to_string()),
        other => HealthGateDecision::Pending(format!(
            "Docker returned unrecognized health status {other:?}"
        )),
    }
}

fn normalized_health(value: &str) -> &str {
    if value.trim().is_empty() {
        "UNKNOWN"
    } else {
        value
    }
}

const fn default_health_timeout_ms() -> u64 {
    DEFAULT_HEALTH_TIMEOUT_MS
}

const fn default_health_poll_interval_ms() -> u64 {
    DEFAULT_HEALTH_POLL_INTERVAL_MS
}

const fn default_compensation_timeout_ms() -> u64 {
    DEFAULT_COMPENSATION_TIMEOUT_MS
}

const fn default_true() -> bool {
    true
}

#[async_trait]
pub trait ContainerRuntime: Send + Sync {
    async fn pull_image(&self, image: &OciImageReference) -> Result<(), RuntimeError>;
    async fn import_image_archive(
        &self,
        _archive: &[u8],
        _expected_image: &OciImageReference,
    ) -> Result<(), RuntimeError> {
        Err(RuntimeError::Engine(
            "runtime does not support OCI archive import".to_string(),
        ))
    }
    async fn import_image_archive_path(
        &self,
        archive_path: &std::path::Path,
        expected_image: &OciImageReference,
    ) -> Result<(), RuntimeError> {
        let bytes = std::fs::read(archive_path)
            .map_err(|error| RuntimeError::Engine(format!("read OCI archive: {error}")))?;
        self.import_image_archive(&bytes, expected_image).await
    }
    async fn create_container(&self, spec: &ContainerSpec)
    -> Result<RuntimeInstance, RuntimeError>;
    async fn start_container(&self, container_id: &str) -> Result<(), RuntimeError>;
    async fn stop_container(
        &self,
        container_id: &str,
        timeout_seconds: i32,
    ) -> Result<(), RuntimeError>;
    async fn restart_container(
        &self,
        container_id: &str,
        timeout_seconds: i32,
    ) -> Result<(), RuntimeError>;
    async fn remove_container(&self, container_id: &str, force: bool) -> Result<(), RuntimeError>;
    async fn inspect_container(&self, container_id: &str) -> Result<RuntimeInstance, RuntimeError>;

    /// Waits for a one-shot container to terminate and returns its exit code.
    /// Custom runtimes must opt in; v1 production uses the Docker adapter.
    async fn wait_container(&self, _container_id: &str) -> Result<i64, RuntimeError> {
        Err(RuntimeError::Engine(
            "runtime does not support waiting for one-shot containers".to_string(),
        ))
    }
}

#[derive(Clone)]
pub struct DockerEngineRuntime {
    docker: Docker,
    registry_credentials: BTreeMap<String, DockerCredentials>,
}

impl DockerEngineRuntime {
    pub fn connect_local() -> Result<Self, RuntimeError> {
        Docker::connect_with_local_defaults()
            .map(|docker| Self {
                docker,
                registry_credentials: BTreeMap::new(),
            })
            .map_err(|error| RuntimeError::EngineUnavailable(error.to_string()))
    }

    pub fn from_client(docker: Docker) -> Self {
        Self {
            docker,
            registry_credentials: BTreeMap::new(),
        }
    }

    /// Loads a bounded, strict credential document materialized by the Agent
    /// supervisor. Credentials stay in memory and are sent only to the exact
    /// registry selected by the immutable OCI reference.
    pub fn with_registry_credentials_file(mut self, path: &Path) -> Result<Self, RuntimeError> {
        let metadata = std::fs::metadata(path).map_err(|error| {
            RuntimeError::InvalidRegistryCredentials(format!(
                "cannot inspect credential file: {error}"
            ))
        })?;
        if !metadata.is_file()
            || metadata.len() == 0
            || metadata.len() > MAX_REGISTRY_CREDENTIALS_BYTES
        {
            return Err(RuntimeError::InvalidRegistryCredentials(
                "credential file must be a non-empty regular file no larger than 64 KiB"
                    .to_string(),
            ));
        }
        let document = std::fs::read_to_string(path).map_err(|error| {
            RuntimeError::InvalidRegistryCredentials(format!(
                "cannot read credential file as UTF-8: {error}"
            ))
        })?;
        self.registry_credentials = parse_registry_credentials(&document)?;
        Ok(self)
    }

    fn credentials_for(&self, image: &OciImageReference) -> Option<DockerCredentials> {
        select_registry_credentials(&self.registry_credentials, image)
    }

    /// Verifies that the configured local Docker Engine is reachable before a
    /// node advertises itself as runtime-ready.
    pub async fn ping(&self) -> Result<(), RuntimeError> {
        let response = self
            .docker
            .ping()
            .await
            .map_err(|error| RuntimeError::EngineUnavailable(error.to_string()))?;
        validate_ping_response(&response)
    }

    async fn ensure_digest(&self, image: &OciImageReference) -> Result<(), RuntimeError> {
        let inspected = self
            .docker
            .inspect_image(&image.to_string())
            .await
            .map_err(|error| RuntimeError::Engine(error.to_string()))?;
        let actual = inspected.repo_digests.unwrap_or_default();
        if actual.iter().any(|digest| digest == &image.to_string()) {
            Ok(())
        } else {
            Err(RuntimeError::DigestMismatch {
                requested: image.to_string(),
                actual,
            })
        }
    }
}

fn validate_ping_response(response: &str) -> Result<(), RuntimeError> {
    if response.trim().eq_ignore_ascii_case("OK") {
        Ok(())
    } else {
        Err(RuntimeError::EngineUnavailable(format!(
            "unexpected Docker ping response: {response:?}"
        )))
    }
}

#[async_trait]
impl ContainerRuntime for DockerEngineRuntime {
    async fn pull_image(&self, image: &OciImageReference) -> Result<(), RuntimeError> {
        let image_name = image.to_string();
        let options = CreateImageOptionsBuilder::default()
            .from_image(&image_name)
            .build();
        self.docker
            .create_image(Some(options), None, self.credentials_for(image))
            .try_collect::<Vec<_>>()
            .await
            .map_err(|error| RuntimeError::Engine(error.to_string()))?;
        self.ensure_digest(image).await
    }

    async fn import_image_archive(
        &self,
        archive: &[u8],
        expected_image: &OciImageReference,
    ) -> Result<(), RuntimeError> {
        let options = ImportImageOptionsBuilder::default().build();
        self.docker
            .import_image(
                options,
                bollard::body_full(bytes::Bytes::copy_from_slice(archive)),
                None,
            )
            .try_collect::<Vec<_>>()
            .await
            .map_err(|error| RuntimeError::Engine(error.to_string()))?;
        self.ensure_digest(expected_image).await
    }

    async fn import_image_archive_path(
        &self,
        archive_path: &std::path::Path,
        expected_image: &OciImageReference,
    ) -> Result<(), RuntimeError> {
        use tokio_util::codec::{BytesCodec, FramedRead};

        let file = tokio::fs::File::open(archive_path)
            .await
            .map_err(|error| RuntimeError::Engine(format!("open OCI archive: {error}")))?;
        let stream = FramedRead::new(file, BytesCodec::new()).map_ok(|bytes| bytes.freeze());
        let options = ImportImageOptionsBuilder::default().build();
        self.docker
            .import_image_stream(options, stream, None)
            .try_collect::<Vec<_>>()
            .await
            .map_err(|error| RuntimeError::Engine(error.to_string()))?;
        self.ensure_digest(expected_image).await
    }

    async fn create_container(
        &self,
        spec: &ContainerSpec,
    ) -> Result<RuntimeInstance, RuntimeError> {
        self.ensure_digest(&spec.image).await?;
        let body = container_create_body(spec)?;
        let name = stable_container_name(&spec.deployment_id);
        let options = CreateContainerOptionsBuilder::default().name(&name).build();
        let response = self
            .docker
            .create_container(Some(options), body)
            .await
            .map_err(|error| RuntimeError::Engine(error.to_string()))?;
        Ok(RuntimeInstance {
            deployment_id: spec.deployment_id.clone(),
            service_id: spec.service_id.clone(),
            release_version: spec
                .labels
                .get("ojos.release_version")
                .cloned()
                .unwrap_or_default(),
            container_id: response.id,
            artifact_digest: spec.image.to_string(),
            desired_state: RuntimeDesiredState::Stopped,
            observed_state: RuntimeObservedState::Created,
            health: "UNKNOWN".to_string(),
        })
    }

    async fn start_container(&self, container_id: &str) -> Result<(), RuntimeError> {
        self.docker
            .start_container(
                container_id,
                None::<bollard::query_parameters::StartContainerOptions>,
            )
            .await
            .map_err(|error| RuntimeError::Engine(error.to_string()))
    }

    async fn stop_container(
        &self,
        container_id: &str,
        timeout_seconds: i32,
    ) -> Result<(), RuntimeError> {
        let options = StopContainerOptionsBuilder::default()
            .t(timeout_seconds)
            .build();
        self.docker
            .stop_container(container_id, Some(options))
            .await
            .map_err(|error| RuntimeError::Engine(error.to_string()))
    }

    async fn restart_container(
        &self,
        container_id: &str,
        timeout_seconds: i32,
    ) -> Result<(), RuntimeError> {
        let options = RestartContainerOptionsBuilder::default()
            .t(timeout_seconds)
            .build();
        self.docker
            .restart_container(container_id, Some(options))
            .await
            .map_err(|error| RuntimeError::Engine(error.to_string()))
    }

    async fn remove_container(&self, container_id: &str, force: bool) -> Result<(), RuntimeError> {
        let options = RemoveContainerOptionsBuilder::default()
            .force(force)
            .build();
        self.docker
            .remove_container(container_id, Some(options))
            .await
            .or_else(|error| match error {
                // Removal is an idempotent compensating action. A missing
                // deterministic container name already satisfies the goal.
                bollard::errors::Error::DockerResponseServerError {
                    status_code: 404, ..
                } => Ok(()),
                other => Err(other),
            })
            .map_err(|error| RuntimeError::Engine(error.to_string()))
    }

    async fn inspect_container(&self, container_id: &str) -> Result<RuntimeInstance, RuntimeError> {
        let inspected = self
            .docker
            .inspect_container(
                container_id,
                None::<bollard::query_parameters::InspectContainerOptions>,
            )
            .await
            .map_err(|error| RuntimeError::Engine(error.to_string()))?;
        let labels = inspected
            .config
            .as_ref()
            .and_then(|config| config.labels.as_ref());
        let deployment_id = labels
            .and_then(|labels| labels.get("ojos.deployment_id"))
            .cloned()
            .unwrap_or_default();
        let service_id = labels
            .and_then(|labels| labels.get("ojos.service_id"))
            .cloned()
            .unwrap_or_default();
        let release_version = labels
            .and_then(|labels| labels.get("ojos.release_version"))
            .cloned()
            .unwrap_or_default();
        let artifact_digest = labels
            .and_then(|labels| labels.get("ojos.artifact_digest"))
            .cloned()
            .unwrap_or_default();
        if !artifact_digest.is_empty() {
            let expected = OciImageReference::parse(&artifact_digest)?;
            self.ensure_digest(&expected).await?;
        }
        let state = inspected.state.unwrap_or_default();
        let observed_state = if state.running == Some(true) {
            RuntimeObservedState::Running
        } else {
            match state.status.as_ref().map(AsRef::as_ref) {
                Some("created") => RuntimeObservedState::Created,
                Some("exited") | Some("dead") => RuntimeObservedState::Exited,
                Some("paused") | Some("restarting") => RuntimeObservedState::Unknown,
                Some(_) => RuntimeObservedState::Stopped,
                None => RuntimeObservedState::Unknown,
            }
        };
        let health = state
            .health
            .and_then(|health| health.status)
            .map(|status| status.to_string().to_ascii_uppercase())
            .unwrap_or_else(|| "NONE".to_string());
        Ok(RuntimeInstance {
            deployment_id,
            service_id,
            release_version,
            container_id: inspected.id.unwrap_or_else(|| container_id.to_string()),
            artifact_digest,
            desired_state: if observed_state == RuntimeObservedState::Running {
                RuntimeDesiredState::Running
            } else {
                RuntimeDesiredState::Stopped
            },
            observed_state,
            health,
        })
    }

    async fn wait_container(&self, container_id: &str) -> Result<i64, RuntimeError> {
        let options = WaitContainerOptionsBuilder::default()
            .condition("not-running")
            .build();
        match self
            .docker
            .wait_container(container_id, Some(options))
            .try_next()
            .await
        {
            Ok(Some(response)) => Ok(response.status_code),
            Ok(None) => Err(RuntimeError::Engine(
                "Docker wait stream ended without an exit status".to_string(),
            )),
            Err(bollard::errors::Error::DockerContainerWaitError { code, .. }) => Ok(code),
            Err(error) => Err(RuntimeError::Engine(error.to_string())),
        }
    }
}

fn parse_registry_credentials(
    document: &str,
) -> Result<BTreeMap<String, DockerCredentials>, RuntimeError> {
    if document.is_empty() || document.len() as u64 > MAX_REGISTRY_CREDENTIALS_BYTES {
        return Err(RuntimeError::InvalidRegistryCredentials(
            "credential document must be between 1 byte and 64 KiB".to_string(),
        ));
    }
    let document: DockerRegistryCredentialsDocument =
        serde_json::from_str(document).map_err(|_| {
            RuntimeError::InvalidRegistryCredentials(
                "credential document is not strict schema-version 1 JSON".to_string(),
            )
        })?;
    if document.schema_version != 1
        || document.registries.is_empty()
        || document.registries.len() > MAX_REGISTRY_CREDENTIALS
    {
        return Err(RuntimeError::InvalidRegistryCredentials(
            "schema_version must be 1 and registries must contain 1-32 entries".to_string(),
        ));
    }

    let mut parsed = BTreeMap::new();
    for credential in document.registries {
        let server = normalize_registry_server(&credential.server_address)?;
        let username = credential.username.trim();
        if username.is_empty()
            || username.len() > 256
            || username.chars().any(|character| character.is_control())
            || credential.password.is_empty()
            || credential.password.len() > 16 * 1024
            || credential
                .password
                .chars()
                .any(|character| matches!(character, '\r' | '\n' | '\0'))
        {
            return Err(RuntimeError::InvalidRegistryCredentials(
                "registry username or password violates the bounded credential contract"
                    .to_string(),
            ));
        }
        if parsed
            .insert(
                server.clone(),
                DockerCredentials {
                    username: Some(username.to_string()),
                    password: Some(credential.password),
                    serveraddress: Some(server),
                    ..Default::default()
                },
            )
            .is_some()
        {
            return Err(RuntimeError::InvalidRegistryCredentials(
                "registry servers must be unique".to_string(),
            ));
        }
    }
    Ok(parsed)
}

fn select_registry_credentials(
    credentials: &BTreeMap<String, DockerCredentials>,
    image: &OciImageReference,
) -> Option<DockerCredentials> {
    credentials
        .get(&registry_server_for_repository(image.repository()))
        .cloned()
}

fn registry_server_for_repository(repository: &str) -> String {
    let first_component = repository.split('/').next().unwrap_or_default();
    if first_component.contains('.')
        || first_component.contains(':')
        || first_component.eq_ignore_ascii_case("localhost")
    {
        first_component.to_ascii_lowercase()
    } else {
        "docker.io".to_string()
    }
}

fn normalize_registry_server(value: &str) -> Result<String, RuntimeError> {
    static REGISTRY_SERVER: OnceLock<Regex> = OnceLock::new();
    let normalized = value.trim().to_ascii_lowercase();
    let pattern = REGISTRY_SERVER.get_or_init(|| {
        Regex::new(
            r"^[a-z0-9](?:[a-z0-9.-]*[a-z0-9])?(?::(?:[1-9][0-9]{0,3}|[1-5][0-9]{4}|6[0-4][0-9]{3}|65[0-4][0-9]{2}|655[0-2][0-9]|6553[0-5]))?$",
        )
        .expect("registry-server regex is valid")
    });
    if normalized != value.trim()
        || normalized.len() > 253
        || !pattern.is_match(&normalized)
        || normalized.contains("..")
    {
        return Err(RuntimeError::InvalidRegistryCredentials(
            "registry server must be a lowercase hostname with an optional valid port".to_string(),
        ));
    }
    Ok(normalized)
}

fn container_create_body(spec: &ContainerSpec) -> Result<ContainerCreateBody, RuntimeError> {
    let mut labels = spec.labels.clone();
    labels.insert("ojos.deployment_id".to_string(), spec.deployment_id.clone());
    labels.insert("ojos.service_id".to_string(), spec.service_id.clone());
    labels.insert("ojos.generation".to_string(), spec.generation.to_string());
    labels.insert("ojos.artifact_digest".to_string(), spec.image.to_string());
    let (exposed_ports, host_config) = if let Some(endpoint) = &spec.published_endpoint {
        endpoint.validate()?;
        let docker_port = endpoint.docker_port();
        let port_bindings = HashMap::from([(
            docker_port.clone(),
            Some(vec![PortBinding {
                // An advertised Node IP commonly does not exist inside a
                // remote or nested Docker Engine namespace. Binding all Engine
                // interfaces is deterministic; outer networking controls the
                // public address and ordinary requests cannot override it.
                host_ip: Some("0.0.0.0".to_string()),
                host_port: Some(endpoint.host_port.to_string()),
            }]),
        )]);
        (
            Some(vec![docker_port]),
            Some(HostConfig {
                port_bindings: Some(port_bindings),
                ..Default::default()
            }),
        )
    } else {
        (None, None)
    };
    Ok(ContainerCreateBody {
        image: Some(spec.image.to_string()),
        cmd: (!spec.command.is_empty()).then(|| spec.command.clone()),
        env: (!spec.environment.is_empty()).then(|| spec.environment.clone()),
        labels: Some(labels),
        exposed_ports,
        host_config,
        ..Default::default()
    })
}

pub fn stable_container_name(deployment_id: &str) -> String {
    let sanitized = deployment_id
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '.' | '_' | '-') {
                character.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect::<String>();
    format!("ojos-{sanitized}")
}

#[cfg(test)]
mod tests {
    use super::*;

    const DIGEST: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

    #[test]
    fn accepts_immutable_digest_reference() {
        let parsed =
            OciImageReference::parse(&format!("ghcr.io/owner/service@sha256:{DIGEST}")).unwrap();
        assert_eq!(parsed.repository(), "ghcr.io/owner/service");
        assert_eq!(parsed.digest(), format!("sha256:{DIGEST}"));
    }

    #[test]
    fn rejects_tags_bare_names_and_uppercase_digests() {
        for invalid in [
            "ghcr.io/owner/service:latest",
            "service@sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "ghcr.io/owner/service:latest@sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "ghcr.io/owner/service@sha256:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
        ] {
            assert!(OciImageReference::parse(invalid).is_err(), "{invalid}");
        }
    }

    #[test]
    fn registry_credentials_are_strict_bounded_and_selected_by_docker_registry_rules() {
        let credentials = parse_registry_credentials(
            r#"{
                "schema_version": 1,
                "registries": [
                    {
                        "server_address": "docker.io",
                        "username": "docker-hub-reader",
                        "password": "docker-hub-token"
                    },
                    {
                        "server_address": "ghcr.io",
                        "username": "capacity-reader",
                        "password": "secret-token"
                    }
                ]
            }"#,
        )
        .unwrap();
        let docker_hub =
            OciImageReference::parse(&format!("owner/service@sha256:{DIGEST}")).unwrap();
        let explicit_registry =
            OciImageReference::parse(&format!("ghcr.io/owner/service@sha256:{DIGEST}")).unwrap();
        let other =
            OciImageReference::parse(&format!("registry.example/owner/service@sha256:{DIGEST}"))
                .unwrap();

        let docker_hub_selected =
            select_registry_credentials(&credentials, &docker_hub).expect("Docker Hub credentials");
        assert_eq!(
            docker_hub_selected.username.as_deref(),
            Some("docker-hub-reader")
        );
        assert_eq!(
            docker_hub_selected.serveraddress.as_deref(),
            Some("docker.io")
        );

        let explicit_selected = select_registry_credentials(&credentials, &explicit_registry)
            .expect("explicit registry credentials");
        assert_eq!(
            explicit_selected.username.as_deref(),
            Some("capacity-reader")
        );
        assert_eq!(explicit_selected.password.as_deref(), Some("secret-token"));
        assert_eq!(explicit_selected.serveraddress.as_deref(), Some("ghcr.io"));
        assert!(select_registry_credentials(&credentials, &other).is_none());

        assert_eq!(registry_server_for_repository("owner/service"), "docker.io");
        assert_eq!(
            registry_server_for_repository("registry.example:5000/owner/service"),
            "registry.example:5000"
        );
        assert_eq!(
            registry_server_for_repository("LOCALHOST/owner/service"),
            "localhost"
        );
    }

    #[test]
    fn registry_credentials_reject_duplicates_unknown_fields_and_secret_leaks() {
        let duplicate = r#"{
            "schema_version": 1,
            "registries": [
                {"server_address":"ghcr.io","username":"u","password":"p1"},
                {"server_address":"ghcr.io","username":"u","password":"p2"}
            ]
        }"#;
        assert!(matches!(
            parse_registry_credentials(duplicate),
            Err(RuntimeError::InvalidRegistryCredentials(_))
        ));

        let unknown = r#"{
            "schema_version": 1,
            "registries": [{
                "server_address":"https://ghcr.io/secret-token",
                "username":"u",
                "password":"secret-token",
                "extra":true
            }]
        }"#;
        let error = parse_registry_credentials(unknown).unwrap_err().to_string();
        assert!(!error.contains("secret-token"));
    }

    #[test]
    fn container_names_are_stable_and_safe() {
        assert_eq!(stable_container_name("Deploy/ABC:1"), "ojos-deploy-abc-1");
    }

    #[test]
    fn docker_create_body_has_one_exact_typed_published_port_binding() {
        let spec = ContainerSpec {
            deployment_id: "deployment-1".to_string(),
            service_id: "capacity-00".to_string(),
            generation: 1,
            image: OciImageReference::parse(&format!("ghcr.io/acme/capacity@sha256:{DIGEST}"))
                .unwrap(),
            command: Vec::new(),
            environment: Vec::new(),
            labels: HashMap::new(),
            published_endpoint: Some(PublishedEndpoint {
                endpoint: "192.0.2.10:20037:capacity-00".to_string(),
                application_protocol: "http".to_string(),
                container_port: 8080,
                host_port: 20037,
                transport_protocol: PublishedPortProtocol::Tcp,
            }),
        };

        let body = container_create_body(&spec).unwrap();

        assert_eq!(body.exposed_ports, Some(vec!["8080/tcp".to_string()]));
        let bindings = body
            .host_config
            .unwrap()
            .port_bindings
            .expect("typed Docker PortBindings");
        assert_eq!(bindings.len(), 1);
        assert_eq!(
            bindings.get("8080/tcp"),
            Some(&Some(vec![PortBinding {
                host_ip: Some("0.0.0.0".to_string()),
                host_port: Some("20037".to_string()),
            }]))
        );
    }

    #[test]
    fn docker_ping_requires_the_engine_ok_response() {
        assert!(validate_ping_response("OK").is_ok());
        assert!(validate_ping_response(" ok\n").is_ok());
        assert!(matches!(
            validate_ping_response("maintenance"),
            Err(RuntimeError::EngineUnavailable(_))
        ));
    }

    fn runtime_instance(observed_state: RuntimeObservedState, health: &str) -> RuntimeInstance {
        RuntimeInstance {
            deployment_id: "deployment-1".to_string(),
            service_id: "service-1".to_string(),
            release_version: "1.0.0".to_string(),
            container_id: "container-1".to_string(),
            artifact_digest: format!("ghcr.io/acme/service@sha256:{DIGEST}"),
            desired_state: RuntimeDesiredState::Running,
            observed_state,
            health: health.to_string(),
        }
    }

    #[test]
    fn health_gate_requires_running_and_healthy_by_default() {
        let policy = HealthGatePolicy::default();
        assert_eq!(
            evaluate_health_gate(
                &runtime_instance(RuntimeObservedState::Running, "HEALTHY"),
                &policy
            ),
            HealthGateDecision::Ready
        );
        assert!(matches!(
            evaluate_health_gate(
                &runtime_instance(RuntimeObservedState::Running, "STARTING"),
                &policy
            ),
            HealthGateDecision::Pending(_)
        ));
        assert!(matches!(
            evaluate_health_gate(
                &runtime_instance(RuntimeObservedState::Exited, "HEALTHY"),
                &policy
            ),
            HealthGateDecision::Failed(_)
        ));
        assert!(matches!(
            evaluate_health_gate(
                &runtime_instance(RuntimeObservedState::Running, "NONE"),
                &policy
            ),
            HealthGateDecision::Failed(_)
        ));
    }

    #[test]
    fn explicit_policy_can_accept_running_image_without_healthcheck() {
        let policy = HealthGatePolicy {
            missing_healthcheck: MissingHealthcheckPolicy::AllowRunning,
            ..HealthGatePolicy::default()
        };
        assert_eq!(
            evaluate_health_gate(
                &runtime_instance(RuntimeObservedState::Running, "NONE"),
                &policy
            ),
            HealthGateDecision::Ready
        );
    }

    #[test]
    fn health_policy_is_bounded() {
        assert!(HealthGatePolicy::default().validate().is_ok());
        assert!(
            HealthGatePolicy {
                timeout_ms: 0,
                ..HealthGatePolicy::default()
            }
            .validate()
            .is_err()
        );
        assert!(
            HealthGatePolicy {
                poll_interval_ms: DEFAULT_HEALTH_TIMEOUT_MS + 1,
                ..HealthGatePolicy::default()
            }
            .validate()
            .is_err()
        );
    }

    #[test]
    fn replacement_payload_has_a_strict_shared_wire_schema() {
        let payload: ReleaseReplacementPayload = serde_json::from_value(serde_json::json!({
            "old_deployment_id": "deployment-old",
            "old_container_id": "container-old",
            "new_spec": {
                "deployment_id": "deployment-new",
                "service_id": "service-1",
                "generation": 2,
                "image": {
                    "repository": "ghcr.io/acme/service",
                    "digest": format!("sha256:{DIGEST}")
                }
            }
        }))
        .unwrap();

        assert!(payload.start);
        assert_eq!(payload.health_gate, HealthGatePolicy::default());
        assert!(payload.validate().is_ok());
        assert!(
            serde_json::from_value::<ReleaseReplacementPayload>(serde_json::json!({
                "old_deployment_id": "deployment-old",
                "old_container_id": "container-old",
                "new_spec": {
                    "deployment_id": "deployment-new",
                    "service_id": "service-1",
                    "generation": 2,
                    "image": {
                        "repository": "ghcr.io/acme/service",
                        "digest": format!("sha256:{DIGEST}")
                    }
                },
                "unexpected": true
            }))
            .is_err()
        );
    }
}
