//! Runtime drivers used by node agents.
//!
//! The production container implementation talks to the Docker Engine API
//! directly.  No request field is interpolated into a shell command.

use async_trait::async_trait;
use bollard::Docker;
use bollard::auth::DockerCredentials;
use bollard::models::{
    ContainerCreateBody, ContainerSummaryStateEnum, HostConfig, HostConfigCgroupnsModeEnum, Mount,
    MountBindOptions, MountBindOptionsPropagationEnum, MountTmpfsOptions, MountType,
    MountVolumeOptions, PortBinding, VolumeCreateRequest,
};
use bollard::query_parameters::{
    CreateContainerOptionsBuilder, CreateImageOptionsBuilder, ImportImageOptionsBuilder,
    ListContainersOptionsBuilder, RemoveContainerOptionsBuilder, RemoveVolumeOptionsBuilder,
    RestartContainerOptionsBuilder, StopContainerOptionsBuilder, WaitContainerOptionsBuilder,
};
use futures_util::TryStreamExt;
use regex::Regex;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
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
pub const STANDARD_RUNTIME_PROFILE_ID: &str = "standard-container-v1";
pub const STANDARD_RUNTIME_PROFILE_SHA256: &str =
    "sha256:56c8ec1e421205dbebb97ad40cbda30bf468d198dd8c3fc50151e39465ea573f";
pub const STANDARD_RUNTIME_PROFILE_CANONICAL_JSON: &str =
    r#"{"id":"standard-container-v1","schema_version":1}"#;
pub const JUDGE_SANDBOX_V1_PROFILE_ID: &str = "judge-sandbox-v1";
pub const JUDGE_SANDBOX_V1_PROFILE_SHA256: &str =
    "sha256:a6b35a495f88bd8e723e395d748de40fbb4dcc08619d02cf92fa580fef2a18ec";
pub const JUDGE_SANDBOX_V1_CANONICAL_JSON: &str = r#"{"cgroup":{"mount_access":"rw","mount_source":"/sys/fs/cgroup","namespace":"host","target":"/sys/fs/cgroup"},"health":{"missing_healthcheck":"reject","poll_interval_ms":2000,"source":"docker-healthcheck","timeout_ms":120000},"id":"judge-sandbox-v1","identity":{"context_file":"/run/ojos/service/context.json","credential_file":"/run/ojos/service/token","mode":"workload-file"},"mounts":[{"access":"rw","kind":"managed-scratch","lifecycle":"deployment","target":"/var/lib/ojos-worker/work"},{"access":"rw","kind":"managed-volume","lifecycle":"release","logical_name":"artifact-cache","target":"/var/lib/ojos-worker/cache"},{"access":"rw","kind":"runtime-resource","resource":"host-cgroup-root","target":"/sys/fs/cgroup"},{"kind":"tmpfs","size_bytes":268435456,"target":"/tmp"},{"access":"ro","kind":"agent-service-context","lifecycle":"deployment","target":"/run/ojos/service"}],"platform":"linux","resources":{"memory_bytes":2147483648,"pids_limit":512},"schema_version":1,"security":{"apparmor_profile":"unconfined","cap_add":["SYS_ADMIN","SYS_CHROOT","NET_ADMIN"],"privileged":true,"user":"0:0"}}"#;
pub const JUDGE_SANDBOX_V1_MEMORY_BYTES: i64 = 2 * 1024 * 1024 * 1024;
pub const JUDGE_SANDBOX_V1_PIDS_LIMIT: i64 = 512;
pub const JUDGE_SANDBOX_V1_TMPFS_BYTES: i64 = 256 * 1024 * 1024;
pub const JUDGE_SANDBOX_V1_HEALTH_TIMEOUT_MS: u64 = 120_000;
pub const JUDGE_SANDBOX_V1_HEALTH_POLL_INTERVAL_MS: u64 = 2_000;
pub const JUDGE_SANDBOX_V1_APPARMOR_PROFILE: &str = "unconfined";
const JUDGE_SANDBOX_V1_APPARMOR_SECURITY_OPT: &str = "apparmor=unconfined";
// Docker 29.5.2 adds this option while normalizing a privileged create. Older
// Engines may omit it from inspect, so attestation accepts it at most once but
// never sends it as caller-controlled policy.
const JUDGE_SANDBOX_V1_PRIVILEGED_LABEL_SECURITY_OPT: &str = "label=disable";
pub const MANAGED_SERVICE_CONTEXT_TARGET: &str = "/run/ojos/service";
pub const MANAGED_SERVICE_CONTEXT_FILE: &str = "/run/ojos/service/context.json";
pub const MANAGED_SERVICE_CREDENTIAL_FILE: &str = "/run/ojos/service/token";
pub const MANAGED_SERVICE_GATEWAY_CA_FILE: &str = "/run/ojos/service/ca.pem";
pub const MANAGED_EVENT_CONTEXT_FILE: &str = "/run/ojos/service/events.json";
pub const MANAGED_EVENT_CONNECTION_FILE: &str = "/run/ojos/service/event-redis.url";
pub const MANAGED_EVENT_STREAM_V1: &str = "ojos:events:v1";
pub const JUDGE_CACHE_VOLUME_LOGICAL_NAME: &str = "artifact-cache";
pub const RELEASE_VOLUME_LIFECYCLE: &str = "release";
const MANAGED_VOLUME_OWNER_LABEL: &str = "ojos.managed_by";
const MANAGED_VOLUME_OWNER: &str = "orchestrator-agent";
const MANAGED_VOLUME_DEPLOYMENT_LABEL: &str = "ojos.deployment_id";
const MANAGED_VOLUME_SERVICE_LABEL: &str = "ojos.service_id";
const MANAGED_VOLUME_ARTIFACT_LABEL: &str = "ojos.artifact_digest";
const MANAGED_VOLUME_PROFILE_LABEL: &str = "ojos.runtime_profile_sha256";
const MANAGED_VOLUME_LOGICAL_NAME_LABEL: &str = "ojos.volume_logical_name";
const MANAGED_VOLUME_LIFECYCLE_LABEL: &str = "ojos.volume_lifecycle";
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
    #[error("invalid runtime contract: {0}")]
    InvalidRuntimeContract(String),
    #[error("invalid materialized runtime context: {0}")]
    InvalidRuntimeContext(String),
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

/// Published runtime profiles are closed, versioned product contracts. A
/// Release may select one of these identifiers, but cannot send Docker
/// capabilities, security options, cgroup modes, or host paths of its own.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
pub enum RuntimeProfile {
    #[default]
    #[serde(rename = "standard-container-v1")]
    StandardV1,
    #[serde(rename = "judge-sandbox-v1")]
    JudgeSandboxV1,
}

impl RuntimeProfile {
    pub const fn id(self) -> &'static str {
        match self {
            Self::StandardV1 => STANDARD_RUNTIME_PROFILE_ID,
            Self::JudgeSandboxV1 => JUDGE_SANDBOX_V1_PROFILE_ID,
        }
    }

    pub const fn expected_sha256(self) -> &'static str {
        match self {
            Self::StandardV1 => STANDARD_RUNTIME_PROFILE_SHA256,
            Self::JudgeSandboxV1 => JUDGE_SANDBOX_V1_PROFILE_SHA256,
        }
    }
}

impl Display for RuntimeProfile {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.id())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RuntimeContract {
    pub id: RuntimeProfile,
    pub profile_sha256: String,
}

impl Default for RuntimeContract {
    fn default() -> Self {
        Self::standard_v1()
    }
}

impl RuntimeContract {
    pub fn standard_v1() -> Self {
        Self::for_profile(RuntimeProfile::StandardV1)
    }

    pub fn judge_sandbox_v1() -> Self {
        Self::for_profile(RuntimeProfile::JudgeSandboxV1)
    }

    pub fn for_profile(id: RuntimeProfile) -> Self {
        Self {
            id,
            profile_sha256: id.expected_sha256().to_string(),
        }
    }

    pub fn validate(&self) -> Result<(), RuntimeError> {
        if self.profile_sha256 != self.id.expected_sha256() {
            return Err(RuntimeError::InvalidRuntimeContract(format!(
                "profile {} requires digest {}, got {}",
                self.id,
                self.id.expected_sha256(),
                self.profile_sha256
            )));
        }
        Ok(())
    }

    pub fn requires_local_context(&self) -> bool {
        self.id == RuntimeProfile::JudgeSandboxV1
    }

    pub fn validate_health_gate(&self, policy: &HealthGatePolicy) -> Result<(), RuntimeError> {
        self.validate()?;
        policy.validate()?;
        if self.id == RuntimeProfile::JudgeSandboxV1
            && (policy.timeout_ms != JUDGE_SANDBOX_V1_HEALTH_TIMEOUT_MS
                || policy.poll_interval_ms != JUDGE_SANDBOX_V1_HEALTH_POLL_INTERVAL_MS
                || policy.missing_healthcheck != MissingHealthcheckPolicy::Reject)
        {
            return Err(RuntimeError::InvalidHealthPolicy(format!(
                "judge-sandbox-v1 requires timeout_ms={}, poll_interval_ms={}, and missing_healthcheck=reject",
                JUDGE_SANDBOX_V1_HEALTH_TIMEOUT_MS, JUDGE_SANDBOX_V1_HEALTH_POLL_INTERVAL_MS
            )));
        }
        Ok(())
    }
}

/// Node-local expansion of a closed runtime profile. This value is never
/// accepted from Catalog metadata or the control-plane wire payload; the Agent
/// derives it from its local policy and attaches it after decoding a Job.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RuntimeContext {
    pub contract: RuntimeContract,
    pub runtime_policy_sha256: String,
    pub scratch_directory: String,
    pub cache_volume_name: String,
    pub service_context_directory: String,
}

/// Exact, Agent-owned Docker volume contract persisted in the local execution
/// ledger before the Docker mutation starts. The ownership labels make both
/// creation and compensation safe to replay after a lost response: an
/// unrelated pre-existing volume with the same name is rejected and is never
/// removed.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ManagedVolumeSpec {
    pub name: String,
    pub deployment_id: String,
    pub service_id: String,
    pub artifact_digest: String,
    pub runtime_contract: RuntimeContract,
    pub logical_name: String,
    pub lifecycle: String,
}

impl ManagedVolumeSpec {
    pub fn validate(&self) -> Result<(), RuntimeError> {
        self.runtime_contract.validate()?;
        if self.runtime_contract.id != RuntimeProfile::JudgeSandboxV1
            || self.service_id != "judge-worker"
            || self.logical_name != JUDGE_CACHE_VOLUME_LOGICAL_NAME
            || self.lifecycle != RELEASE_VOLUME_LIFECYCLE
            || self.deployment_id.trim().is_empty()
        {
            return Err(RuntimeError::InvalidRuntimeContext(
                "managed cache volume must be the fixed release-scoped judge-sandbox-v1 artifact-cache contract"
                    .to_string(),
            ));
        }
        let expected_prefix = "ojos-judge-cache-";
        let component = self.name.strip_prefix(expected_prefix).ok_or_else(|| {
            RuntimeError::InvalidRuntimeContext(
                "managed cache volume name must start with ojos-judge-cache-".to_string(),
            )
        })?;
        if component.len() != 32
            || !component
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err(RuntimeError::InvalidRuntimeContext(
                "managed cache volume name must end with the Agent-derived 128-bit deployment digest"
                    .to_string(),
            ));
        }
        OciImageReference::parse(&self.artifact_digest)?;
        Ok(())
    }

    pub fn ownership_labels(&self) -> Result<HashMap<String, String>, RuntimeError> {
        self.validate()?;
        Ok(HashMap::from([
            (
                MANAGED_VOLUME_OWNER_LABEL.to_string(),
                MANAGED_VOLUME_OWNER.to_string(),
            ),
            (
                MANAGED_VOLUME_DEPLOYMENT_LABEL.to_string(),
                self.deployment_id.clone(),
            ),
            (
                MANAGED_VOLUME_SERVICE_LABEL.to_string(),
                self.service_id.clone(),
            ),
            (
                MANAGED_VOLUME_ARTIFACT_LABEL.to_string(),
                self.artifact_digest.clone(),
            ),
            (
                MANAGED_VOLUME_PROFILE_LABEL.to_string(),
                self.runtime_contract.profile_sha256.clone(),
            ),
            (
                MANAGED_VOLUME_LOGICAL_NAME_LABEL.to_string(),
                self.logical_name.clone(),
            ),
            (
                MANAGED_VOLUME_LIFECYCLE_LABEL.to_string(),
                self.lifecycle.clone(),
            ),
        ]))
    }
}

/// Read-only capabilities reported by the local Docker Engine. These facts are
/// evidence for an Agent-local policy decision; they are never interpreted as
/// permission to accept arbitrary HostConfig input from a Release.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct DockerRuntimeFacts {
    pub engine: String,
    pub server_version: String,
    pub operating_system: String,
    pub os_type: String,
    pub architecture: String,
    pub cgroup_version: String,
    pub memory_limit: bool,
    pub pids_limit: bool,
    pub rootless: bool,
    pub apparmor: bool,
    pub seccomp: bool,
    pub security_options: Vec<String>,
}

impl RuntimeContext {
    pub fn validate(&self) -> Result<(), RuntimeError> {
        self.contract.validate()?;
        validate_sha256_text("runtime_policy_sha256", &self.runtime_policy_sha256)?;
        if !Path::new(&self.service_context_directory).is_absolute() {
            return Err(RuntimeError::InvalidRuntimeContext(
                "service_context_directory must be an absolute Agent-local path".to_string(),
            ));
        }
        match self.contract.id {
            RuntimeProfile::StandardV1 => {
                if !self.scratch_directory.is_empty() || !self.cache_volume_name.is_empty() {
                    return Err(RuntimeError::InvalidRuntimeContext(
                        "standard-container-v1 context cannot request scratch or cache mounts"
                            .to_string(),
                    ));
                }
            }
            RuntimeProfile::JudgeSandboxV1 => {
                if !Path::new(&self.scratch_directory).is_absolute() {
                    return Err(RuntimeError::InvalidRuntimeContext(
                        "scratch_directory must be an absolute Agent-local path".to_string(),
                    ));
                }
                let component = self
                    .cache_volume_name
                    .strip_prefix("ojos-judge-cache-")
                    .unwrap_or_default();
                if component.len() != 32
                    || !component
                        .bytes()
                        .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
                {
                    return Err(RuntimeError::InvalidRuntimeContext(
                        "cache_volume_name must be ojos-judge-cache- followed by the Agent-derived 128-bit lowercase deployment digest"
                            .to_string(),
                    ));
                }
            }
        }
        Ok(())
    }
}

// Deliberately not Serialize/Deserialize: this value may exist only in Agent
// memory and in the private workload credential file. Keeping it out of serde
// prevents it from being accidentally embedded in a Job result or ledger row.
#[derive(Clone, PartialEq, Eq)]
pub struct WorkloadCredential {
    pub access_token: String,
    pub expires_at_ms: i64,
}

impl std::fmt::Debug for WorkloadCredential {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("WorkloadCredential")
            .field("access_token", &"[REDACTED]")
            .field("expires_at_ms", &self.expires_at_ms)
            .finish()
    }
}

impl WorkloadCredential {
    pub fn validate_at(&self, now_ms: i64) -> Result<(), RuntimeError> {
        if self.access_token.is_empty()
            || self.access_token.len() > 16 * 1024
            || self.access_token.chars().any(char::is_whitespace)
        {
            return Err(RuntimeError::InvalidRuntimeContext(
                "workload access token must be non-empty, at most 16 KiB, and contain no whitespace"
                    .to_string(),
            ));
        }
        if self.expires_at_ms <= now_ms.saturating_add(60_000) {
            return Err(RuntimeError::InvalidRuntimeContext(
                "workload credential must remain valid for at least 60 seconds".to_string(),
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ManagedApiBinding {
    pub binding_id: String,
    pub api_id: String,
    pub timeout_ms: u64,
    #[serde(default)]
    pub context_generation: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(deny_unknown_fields)]
pub struct ManagedEventSubscription {
    pub event_type: String,
    pub consumer_group: String,
}

/// Credential-free event projection sent through the Job protocol. The Redis
/// URL/password never crosses the control-plane boundary; the Agent resolves
/// `connection_id` from its local protected connection configuration.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ManagedEventBinding {
    pub connection_id: String,
    pub stream: String,
    #[serde(default)]
    pub publish_types: Vec<String>,
    #[serde(default)]
    pub subscriptions: Vec<ManagedEventSubscription>,
    pub generation: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ManagedServiceContextSpec {
    #[serde(default)]
    pub generation: u64,
    pub node_id: String,
    pub gateway_origin: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gateway_ca_pem: Option<String>,
    pub bindings: BTreeMap<String, ManagedApiBinding>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub events: Option<ManagedEventBinding>,
}

/// Credential-free control-plane projection used to rebuild an exact Agent
/// context CAS after the last binding was revoked. Tokens are never stored.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ManagedServiceContextProjection {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current: Option<ManagedServiceContextSpec>,
    pub last_nonempty: ManagedServiceContextSpec,
    pub revoked: bool,
}

/// Agent-local, idempotent update of an already-running Deployment's mounted
/// Service Context. `context=None` revokes the local context during a topology
/// compensation/removal; a non-empty context is materialized atomically with a
/// freshly exchanged workload credential.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct BindingContextApplyPayload {
    pub deployment_id: String,
    pub service_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context: Option<ManagedServiceContextSpec>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous_context: Option<ManagedServiceContextSpec>,
}

impl BindingContextApplyPayload {
    pub fn validate(&self) -> Result<(), RuntimeError> {
        for (name, value) in [
            ("deployment_id", self.deployment_id.as_str()),
            ("service_id", self.service_id.as_str()),
        ] {
            if value.trim().is_empty() || value.len() > 256 || value.chars().any(char::is_control) {
                return Err(RuntimeError::InvalidRuntimeContext(format!(
                    "{name} is empty or exceeds protocol bounds"
                )));
            }
        }
        if let Some(context) = &self.context {
            context.validate()?;
        }
        if let Some(context) = &self.previous_context {
            context.validate()?;
        }
        self.previous_context.as_ref().ok_or_else(|| {
            RuntimeError::InvalidRuntimeContext(
                "binding context apply requires the exact previous_context for CAS and compensation"
                    .to_string(),
            )
        })?;
        Ok(())
    }
}

impl ManagedServiceContextSpec {
    pub fn validate(&self) -> Result<(), RuntimeError> {
        if self.generation == 0 {
            return Err(RuntimeError::InvalidRuntimeContext(
                "managed service context generation must be positive".to_string(),
            ));
        }
        if self.node_id.trim().is_empty() {
            return Err(RuntimeError::InvalidRuntimeContext(
                "managed service context requires node_id".to_string(),
            ));
        }
        let origin = self.gateway_origin.as_str();
        let parsed = url::Url::parse(origin).map_err(|error| {
            RuntimeError::InvalidRuntimeContext(format!(
                "managed service gateway_origin is not a valid origin: {error}"
            ))
        })?;
        let has_authority_only = origin == origin.trim()
            && !origin.ends_with('/')
            && parsed.username().is_empty()
            && parsed.password().is_none()
            && parsed.path() == "/"
            && parsed.query().is_none()
            && parsed.fragment().is_none()
            && parsed.host_str().is_some();
        let loopback_http = parsed.scheme() == "http"
            && matches!(parsed.host_str(), Some("127.0.0.1" | "localhost"));
        if !has_authority_only || !(parsed.scheme() == "https" || loopback_http) {
            return Err(RuntimeError::InvalidRuntimeContext(
                "managed service gateway_origin must be an HTTPS scheme+authority without userinfo, path, query, or fragment, or loopback HTTP"
                    .to_string(),
            ));
        }
        for (name, binding) in &self.bindings {
            if name.trim().is_empty()
                || binding.binding_id.trim().is_empty()
                || binding.api_id.trim().is_empty()
                || binding.timeout_ms == 0
                || binding.context_generation != self.generation
                || binding.api_id.contains('/')
                || binding.api_id.chars().any(char::is_whitespace)
            {
                return Err(RuntimeError::InvalidRuntimeContext(format!(
                    "managed API binding {name:?} is incomplete or unsafe"
                )));
            }
        }
        if self
            .gateway_ca_pem
            .as_ref()
            .is_some_and(|pem| pem.is_empty() || pem.len() > 1024 * 1024)
        {
            return Err(RuntimeError::InvalidRuntimeContext(
                "gateway CA PEM must be non-empty and at most 1 MiB".to_string(),
            ));
        }
        if let Some(events) = &self.events {
            if events.generation != self.generation
                || events.connection_id.trim().is_empty()
                || events.stream.trim().is_empty()
                || events.stream.len() > 256
                || events.stream.chars().any(char::is_control)
                || (events.publish_types.is_empty() && events.subscriptions.is_empty())
            {
                return Err(RuntimeError::InvalidRuntimeContext(
                    "managed event binding identity/generation is invalid".to_string(),
                ));
            }
            let mut publish_types = events.publish_types.clone();
            publish_types.sort();
            publish_types.dedup();
            if publish_types != events.publish_types
                || publish_types.iter().any(|event_type| {
                    event_type.trim().is_empty()
                        || event_type.len() > 256
                        || event_type.chars().any(char::is_whitespace)
                })
            {
                return Err(RuntimeError::InvalidRuntimeContext(
                    "managed event publish types must be sorted unique identifiers".to_string(),
                ));
            }
            let mut subscriptions = events.subscriptions.clone();
            subscriptions.sort();
            subscriptions.dedup();
            if subscriptions != events.subscriptions
                || subscriptions.iter().any(|subscription| {
                    subscription.event_type.trim().is_empty()
                        || subscription.consumer_group.trim().is_empty()
                        || subscription.event_type.len() > 256
                        || subscription.consumer_group.len() > 256
                        || subscription.event_type.chars().any(char::is_whitespace)
                        || subscription.consumer_group.chars().any(char::is_whitespace)
                })
            {
                return Err(RuntimeError::InvalidRuntimeContext(
                    "managed event subscriptions must be sorted unique identifiers/groups"
                        .to_string(),
                ));
            }
        }
        Ok(())
    }
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
#[serde(deny_unknown_fields)]
pub struct ContainerSpec {
    pub deployment_id: String,
    pub service_id: String,
    pub generation: u64,
    pub image: OciImageReference,
    #[serde(default)]
    pub runtime_contract: RuntimeContract,
    /// Filled only by the Node Agent after its local policy and runtime facts
    /// have accepted the signed contract. It is deliberately excluded from the
    /// control-plane wire representation.
    #[serde(skip)]
    pub runtime_context: Option<RuntimeContext>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub managed_service_context: Option<ManagedServiceContextSpec>,
    #[serde(default)]
    pub command: Vec<String>,
    #[serde(default)]
    pub environment: Vec<String>,
    #[serde(default)]
    pub labels: HashMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub published_endpoint: Option<PublishedEndpoint>,
}

impl ContainerSpec {
    /// Expands the closed runtime profile into the only Docker named volume
    /// the Agent is allowed to own. Ordinary containers and unmaterialized
    /// wire payloads never request a volume.
    pub fn managed_volume_spec(&self) -> Result<Option<ManagedVolumeSpec>, RuntimeError> {
        if self.runtime_contract.id == RuntimeProfile::StandardV1 {
            return Ok(None);
        }
        let context = validate_judge_sandbox_spec(self)?;
        let spec = ManagedVolumeSpec {
            name: context.cache_volume_name.clone(),
            deployment_id: self.deployment_id.clone(),
            service_id: self.service_id.clone(),
            artifact_digest: self.image.to_string(),
            runtime_contract: self.runtime_contract.clone(),
            logical_name: JUDGE_CACHE_VOLUME_LOGICAL_NAME.to_string(),
            lifecycle: RELEASE_VOLUME_LIFECYCLE.to_string(),
        };
        spec.validate()?;
        Ok(Some(spec))
    }
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
    #[serde(default)]
    pub runtime_contract: RuntimeContract,
    #[serde(default)]
    pub runtime_policy_sha256: String,
    #[serde(default)]
    pub effective_runtime_sha256: String,
    #[serde(default)]
    pub runtime_attested: bool,
    pub desired_state: RuntimeDesiredState,
    pub observed_state: RuntimeObservedState,
    pub health: String,
}

/// A bounded, credential-free observation of one Agent-managed Docker
/// deployment.  It is intentionally independent from `RuntimeInstance`: an
/// observation must remain serializable even when a container has drifted so
/// far that the strict runtime contract can no longer be decoded or attested.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct DeploymentRuntimeObservationV1 {
    pub deployment_id: String,
    pub service_id: String,
    pub container_id: String,
    #[serde(default)]
    pub artifact_digest: String,
    pub runtime_contract: RuntimeContract,
    #[serde(default)]
    pub runtime_policy_sha256: String,
    #[serde(default)]
    pub effective_runtime_sha256: String,
    pub observed_state: RuntimeObservedState,
    pub health: String,
    pub runtime_attested: bool,
    #[serde(default)]
    pub drift_reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ManagedDeploymentInventoryV1 {
    pub inventory_complete: bool,
    #[serde(default)]
    pub inventory_error: String,
    pub deployments: Vec<DeploymentRuntimeObservationV1>,
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
    pub api_id: String,
    #[serde(default)]
    pub binding_id: String,
    #[serde(default)]
    pub consumer_deployment_id: String,
    #[serde(default = "default_credential_generation")]
    pub credential_generation: u64,
    #[serde(default = "default_binding_timeout_ms")]
    pub timeout_ms: u64,
    #[serde(default)]
    pub provider_node_id: String,
    #[serde(default)]
    pub provider_endpoint: String,
    #[serde(default)]
    pub strip_prefix: bool,
    #[serde(default)]
    pub rewrite_prefix: String,
    #[serde(default)]
    pub methods: Vec<String>,
    pub auth_mode: String,
    #[serde(default)]
    pub required_permission: String,
}

const fn default_credential_generation() -> u64 {
    1
}

const fn default_binding_timeout_ms() -> u64 {
    30_000
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
    /// Derive the only health-gate policy accepted for a published runtime
    /// contract. Keeping this mapping beside the closed runtime profiles makes
    /// Store validation, install, dependency, upgrade, and rollback payloads
    /// use exactly the same policy as the Agent-side attestation gate.
    pub fn for_runtime_contract(contract: &RuntimeContract) -> Self {
        match contract.id {
            RuntimeProfile::StandardV1 => Self::default(),
            RuntimeProfile::JudgeSandboxV1 => Self {
                timeout_ms: JUDGE_SANDBOX_V1_HEALTH_TIMEOUT_MS,
                poll_interval_ms: JUDGE_SANDBOX_V1_HEALTH_POLL_INTERVAL_MS,
                missing_healthcheck: MissingHealthcheckPolicy::Reject,
                compensation_timeout_ms: DEFAULT_COMPENSATION_TIMEOUT_MS,
            },
        }
    }

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
    /// Keep the proven old container and its runtime projection until a
    /// control-plane Topology job has atomically switched every ApiBinding to
    /// the healthy replacement. The old container is removed by a subsequent
    /// explicit Uninstall job in the same Operation.
    #[serde(default)]
    pub preserve_old_until_topology_cutover: bool,
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
    async fn create_managed_volume(&self, _spec: &ManagedVolumeSpec) -> Result<(), RuntimeError> {
        Err(RuntimeError::Engine(
            "runtime does not support managed Docker volumes".to_string(),
        ))
    }
    async fn remove_managed_volume(&self, _spec: &ManagedVolumeSpec) -> Result<(), RuntimeError> {
        Err(RuntimeError::Engine(
            "runtime does not support managed Docker volumes".to_string(),
        ))
    }
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

    /// Captures the Engine facts used to decide whether closed runtime
    /// profiles can be materialized on this Node.
    pub async fn runtime_facts(&self) -> Result<DockerRuntimeFacts, RuntimeError> {
        let info = self
            .docker
            .info()
            .await
            .map_err(|error| RuntimeError::EngineUnavailable(error.to_string()))?;
        Ok(docker_runtime_facts(&info))
    }

    /// Enumerates every container carrying the immutable OJOS deployment
    /// label, including stopped containers, and attests each one without
    /// creating a Job or Operation. The returned inventory is deterministic
    /// and bounded so a compromised Engine cannot make the Agent publish an
    /// unbounded report.
    pub async fn managed_deployment_inventory(
        &self,
        max_deployments: usize,
    ) -> Result<ManagedDeploymentInventoryV1, RuntimeError> {
        if max_deployments == 0 {
            return Err(RuntimeError::Engine(
                "managed deployment inventory limit must be positive".to_string(),
            ));
        }
        let filters =
            HashMap::from([("label".to_string(), vec!["ojos.deployment_id".to_string()])]);
        let options = ListContainersOptionsBuilder::default()
            .all(true)
            .filters(&filters)
            .build();
        let mut summaries = self
            .docker
            .list_containers(Some(options))
            .await
            .map_err(|error| RuntimeError::Engine(error.to_string()))?;
        summaries.sort_by(|left, right| left.id.cmp(&right.id));

        let truncated = summaries.len() > max_deployments;
        summaries.truncate(max_deployments);
        let mut inventory_complete = !truncated;
        let mut inventory_errors = Vec::new();
        if truncated {
            inventory_errors.push(format!(
                "managed container inventory exceeds the bounded limit of {max_deployments}"
            ));
        }
        let mut observations = BTreeMap::<String, DeploymentRuntimeObservationV1>::new();
        for summary in summaries {
            let container_id = summary.id.clone().unwrap_or_default();
            let labels = summary.labels.as_ref();
            let deployment_id = labels
                .and_then(|labels| labels.get("ojos.deployment_id"))
                .cloned()
                .unwrap_or_default();
            if deployment_id.trim().is_empty() || container_id.trim().is_empty() {
                inventory_complete = false;
                inventory_errors.push(
                    "a managed Docker container has an empty deployment label or container ID"
                        .to_string(),
                );
                continue;
            }

            let observation = match self.inspect_container(&container_id).await {
                Ok(instance) => {
                    inspected_runtime_observation(&deployment_id, &container_id, instance)
                }
                Err(error) => DeploymentRuntimeObservationV1 {
                    deployment_id: deployment_id.clone(),
                    service_id: labels
                        .and_then(|labels| labels.get("ojos.service_id"))
                        .cloned()
                        .filter(|value| !value.trim().is_empty())
                        .unwrap_or_else(|| "<missing>".to_string()),
                    container_id: container_id.clone(),
                    artifact_digest: labels
                        .and_then(|labels| labels.get("ojos.artifact_digest"))
                        .cloned()
                        .unwrap_or_default(),
                    runtime_contract: fallback_runtime_contract(labels),
                    runtime_policy_sha256: labels
                        .and_then(|labels| labels.get("ojos.runtime_policy_sha256"))
                        .cloned()
                        .unwrap_or_default(),
                    effective_runtime_sha256: labels
                        .and_then(|labels| labels.get("ojos.runtime_effective_sha256"))
                        .cloned()
                        .unwrap_or_default(),
                    observed_state: summary_observed_state(summary.state.as_ref()),
                    health: summary
                        .health
                        .and_then(|health| health.status)
                        .map(|status| status.to_string().to_ascii_uppercase())
                        .unwrap_or_else(|| "UNKNOWN".to_string()),
                    runtime_attested: false,
                    drift_reason: bounded_drift_reason(&error.to_string()),
                },
            };

            if let Some(existing) = observations.get_mut(&deployment_id) {
                existing.runtime_attested = false;
                existing.observed_state = RuntimeObservedState::Unknown;
                existing.health = "UNHEALTHY".to_string();
                existing.drift_reason = bounded_drift_reason(&format!(
                    "duplicate managed containers for deployment {deployment_id}: {}, {}",
                    existing.container_id, observation.container_id
                ));
            } else {
                observations.insert(deployment_id, observation);
            }
        }

        Ok(ManagedDeploymentInventoryV1 {
            inventory_complete,
            inventory_error: bounded_drift_reason(&inventory_errors.join("; ")),
            deployments: observations.into_values().collect(),
        })
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

fn docker_runtime_facts(info: &bollard::models::SystemInfo) -> DockerRuntimeFacts {
    let mut security_options = info.security_options.clone().unwrap_or_default();
    security_options.sort();
    security_options.dedup();
    let has_option = |name: &str| {
        security_options.iter().any(|option| {
            option == name
                || option
                    .strip_prefix("name=")
                    .is_some_and(|value| value == name || value.starts_with(&format!("{name},")))
        })
    };
    DockerRuntimeFacts {
        engine: "docker".to_string(),
        server_version: info.server_version.clone().unwrap_or_default(),
        operating_system: info.operating_system.clone().unwrap_or_default(),
        os_type: info.os_type.clone().unwrap_or_default(),
        architecture: info.architecture.clone().unwrap_or_default(),
        cgroup_version: info
            .cgroup_version
            .map(|version| version.to_string())
            .unwrap_or_default(),
        memory_limit: info.memory_limit.unwrap_or(false),
        pids_limit: info.pids_limit.unwrap_or(false),
        rootless: has_option("rootless"),
        apparmor: has_option("apparmor"),
        seccomp: has_option("seccomp"),
        security_options,
    }
}

fn fallback_runtime_contract(labels: Option<&HashMap<String, String>>) -> RuntimeContract {
    match labels
        .and_then(|labels| labels.get("ojos.runtime_profile"))
        .map(String::as_str)
    {
        Some(JUDGE_SANDBOX_V1_PROFILE_ID) => RuntimeContract {
            id: RuntimeProfile::JudgeSandboxV1,
            profile_sha256: JUDGE_SANDBOX_V1_PROFILE_SHA256.to_string(),
        },
        _ => RuntimeContract {
            id: RuntimeProfile::StandardV1,
            profile_sha256: STANDARD_RUNTIME_PROFILE_SHA256.to_string(),
        },
    }
}

fn inspected_runtime_observation(
    expected_deployment_id: &str,
    fallback_container_id: &str,
    mut instance: RuntimeInstance,
) -> DeploymentRuntimeObservationV1 {
    let mut drift = Vec::new();
    if instance.deployment_id != expected_deployment_id {
        drift.push("deployment identity label changed during Docker inspection".to_string());
        instance.deployment_id = expected_deployment_id.to_string();
    }
    if instance.service_id.trim().is_empty() {
        drift.push("managed container is missing the service identity label".to_string());
        instance.service_id = "<missing>".to_string();
    }
    if instance.container_id.trim().is_empty() {
        drift.push("Docker inspection returned an empty container ID".to_string());
        instance.container_id = fallback_container_id.to_string();
    }
    if OciImageReference::parse(&instance.artifact_digest).is_err() {
        drift.push("managed container is missing a canonical OCI artifact digest".to_string());
    }
    if !instance.runtime_attested {
        drift.push("Docker runtime attestation did not succeed".to_string());
    }
    let runtime_attested = drift.is_empty();
    DeploymentRuntimeObservationV1 {
        deployment_id: instance.deployment_id,
        service_id: instance.service_id,
        container_id: instance.container_id,
        artifact_digest: instance.artifact_digest,
        runtime_contract: instance.runtime_contract,
        runtime_policy_sha256: instance.runtime_policy_sha256,
        effective_runtime_sha256: instance.effective_runtime_sha256,
        observed_state: instance.observed_state,
        health: instance.health,
        runtime_attested,
        drift_reason: bounded_drift_reason(&drift.join("; ")),
    }
}

fn summary_observed_state(state: Option<&ContainerSummaryStateEnum>) -> RuntimeObservedState {
    match state.map(AsRef::as_ref) {
        Some("running") => RuntimeObservedState::Running,
        Some("created") => RuntimeObservedState::Created,
        Some("exited") | Some("dead") => RuntimeObservedState::Exited,
        Some("paused") | Some("restarting") | Some("removing") | Some("stopping") => {
            RuntimeObservedState::Unknown
        }
        Some(_) => RuntimeObservedState::Stopped,
        None => RuntimeObservedState::Unknown,
    }
}

fn bounded_drift_reason(value: &str) -> String {
    const MAX_BYTES: usize = 512;
    let mut bounded = String::new();
    for character in value.chars() {
        let printable = if character.is_control() {
            ' '
        } else {
            character
        };
        if bounded.len() + printable.len_utf8() > MAX_BYTES {
            break;
        }
        bounded.push(printable);
    }
    let bounded = bounded.trim().to_string();
    if !value.is_empty() && bounded.is_empty() {
        "runtime attestation failed".to_string()
    } else {
        bounded
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

fn attest_managed_volume(
    volume: &bollard::models::Volume,
    spec: &ManagedVolumeSpec,
) -> Result<(), RuntimeError> {
    let expected_labels = spec.ownership_labels()?;
    if volume.name != spec.name
        || volume.driver != "local"
        || volume.labels != expected_labels
        || volume
            .scope
            .is_some_and(|scope| scope.as_ref() != "local" && scope.as_ref() != "")
    {
        return Err(RuntimeError::InvalidRuntimeContext(format!(
            "Docker volume {} does not match the exact Agent ownership contract; refusing adoption or deletion",
            spec.name
        )));
    }
    Ok(())
}

#[async_trait]
impl ContainerRuntime for DockerEngineRuntime {
    async fn create_managed_volume(&self, spec: &ManagedVolumeSpec) -> Result<(), RuntimeError> {
        let labels = spec.ownership_labels()?;
        self.docker
            .create_volume(VolumeCreateRequest {
                name: Some(spec.name.clone()),
                driver: Some("local".to_string()),
                labels: Some(labels),
                ..Default::default()
            })
            .await
            .map_err(|error| RuntimeError::Engine(error.to_string()))?;
        let inspected = self
            .docker
            .inspect_volume(&spec.name)
            .await
            .map_err(|error| RuntimeError::Engine(error.to_string()))?;
        attest_managed_volume(&inspected, spec)
    }

    async fn remove_managed_volume(&self, spec: &ManagedVolumeSpec) -> Result<(), RuntimeError> {
        spec.validate()?;
        let inspected = match self.docker.inspect_volume(&spec.name).await {
            Ok(volume) => volume,
            Err(bollard::errors::Error::DockerResponseServerError {
                status_code: 404, ..
            }) => return Ok(()),
            Err(error) => return Err(RuntimeError::Engine(error.to_string())),
        };
        attest_managed_volume(&inspected, spec)?;
        let options = RemoveVolumeOptionsBuilder::default().force(false).build();
        self.docker
            .remove_volume(&spec.name, Some(options))
            .await
            .or_else(|error| match error {
                // The exact owned volume was already absent after a lost
                // response. Replaying compensation is therefore complete.
                bollard::errors::Error::DockerResponseServerError {
                    status_code: 404, ..
                } => Ok(()),
                other => Err(other),
            })
            .map_err(|error| RuntimeError::Engine(error.to_string()))
    }

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
        let effective_runtime_sha256 = effective_runtime_sha256(spec)?;
        let runtime_policy_sha256 = spec
            .runtime_context
            .as_ref()
            .map(|context| context.runtime_policy_sha256.clone())
            .unwrap_or_default();
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
            runtime_contract: spec.runtime_contract.clone(),
            runtime_policy_sha256,
            effective_runtime_sha256,
            runtime_attested: true,
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
        let runtime_profile = labels
            .and_then(|labels| labels.get("ojos.runtime_profile"))
            .map(String::as_str)
            .unwrap_or(STANDARD_RUNTIME_PROFILE_ID);
        let runtime_profile = match runtime_profile {
            STANDARD_RUNTIME_PROFILE_ID => RuntimeProfile::StandardV1,
            JUDGE_SANDBOX_V1_PROFILE_ID => RuntimeProfile::JudgeSandboxV1,
            other => {
                return Err(RuntimeError::InvalidRuntimeContract(format!(
                    "container advertises unknown runtime profile {other}"
                )));
            }
        };
        let runtime_contract = RuntimeContract {
            id: runtime_profile,
            profile_sha256: labels
                .and_then(|labels| labels.get("ojos.runtime_profile_sha256"))
                .cloned()
                .unwrap_or_else(|| runtime_profile.expected_sha256().to_string()),
        };
        runtime_contract.validate()?;
        let runtime_policy_sha256 = labels
            .and_then(|labels| labels.get("ojos.runtime_policy_sha256"))
            .cloned()
            .unwrap_or_default();
        let effective_runtime_sha256 = labels
            .and_then(|labels| labels.get("ojos.runtime_effective_sha256"))
            .cloned()
            .unwrap_or_default();
        if !runtime_policy_sha256.is_empty() {
            validate_sha256_text("runtime_policy_sha256", &runtime_policy_sha256)?;
        }
        if !effective_runtime_sha256.is_empty() {
            validate_sha256_text("effective_runtime_sha256", &effective_runtime_sha256)?;
        }
        if runtime_contract.id == RuntimeProfile::JudgeSandboxV1 {
            if runtime_policy_sha256.is_empty() || effective_runtime_sha256.is_empty() {
                return Err(RuntimeError::InvalidRuntimeContext(
                    "judge-sandbox-v1 inspection is missing policy/effective runtime attestations"
                        .to_string(),
                ));
            }
            attest_judge_sandbox_inspection(
                &inspected,
                &runtime_contract,
                &runtime_policy_sha256,
                &effective_runtime_sha256,
            )?;
        } else if !runtime_policy_sha256.is_empty() {
            if effective_runtime_sha256.is_empty() {
                return Err(RuntimeError::InvalidRuntimeContext(
                    "managed standard-container-v1 inspection is missing its effective runtime attestation"
                        .to_string(),
                ));
            }
            attest_standard_managed_context_inspection(
                &inspected,
                &runtime_contract,
                &runtime_policy_sha256,
                &effective_runtime_sha256,
            )?;
        }
        if !artifact_digest.is_empty() {
            let expected = OciImageReference::parse(&artifact_digest)?;
            self.ensure_digest(&expected).await?;
            let expected_image = self
                .docker
                .inspect_image(&expected.to_string())
                .await
                .map_err(|error| RuntimeError::Engine(error.to_string()))?;
            let expected_image_id = expected_image.id.unwrap_or_default();
            let actual_image_id = inspected.image.clone().unwrap_or_default();
            if expected_image_id.is_empty() || actual_image_id != expected_image_id {
                return Err(RuntimeError::DigestMismatch {
                    requested: expected.to_string(),
                    actual: vec![actual_image_id],
                });
            }
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
        let runtime_attested = runtime_contract.id == RuntimeProfile::StandardV1
            || !effective_runtime_sha256.is_empty();
        Ok(RuntimeInstance {
            deployment_id,
            service_id,
            release_version,
            container_id: inspected.id.unwrap_or_else(|| container_id.to_string()),
            artifact_digest,
            runtime_contract,
            runtime_policy_sha256,
            effective_runtime_sha256: effective_runtime_sha256.clone(),
            runtime_attested,
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

fn validate_sha256_text(name: &str, value: &str) -> Result<(), RuntimeError> {
    let valid = value.len() == 71
        && value.starts_with("sha256:")
        && value[7..]
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte));
    if valid {
        Ok(())
    } else {
        Err(RuntimeError::InvalidRuntimeContext(format!(
            "{name} must be sha256:<64 lowercase hex>"
        )))
    }
}

fn effective_runtime_sha256(spec: &ContainerSpec) -> Result<String, RuntimeError> {
    spec.runtime_contract.validate()?;
    let bytes = if let Some(context) = spec.runtime_context.as_ref() {
        context.validate()?;
        if context.contract != spec.runtime_contract {
            return Err(RuntimeError::InvalidRuntimeContext(
                "runtime context contract differs from ContainerSpec contract".to_string(),
            ));
        }
        serde_json::to_vec(context).map_err(|error| {
            RuntimeError::InvalidRuntimeContext(format!(
                "cannot encode effective runtime context: {error}"
            ))
        })?
    } else if spec.runtime_contract.id == RuntimeProfile::StandardV1 {
        spec.runtime_contract.profile_sha256.as_bytes().to_vec()
    } else {
        return Err(RuntimeError::InvalidRuntimeContext(
            "judge-sandbox-v1 requires an Agent-materialized runtime context".to_string(),
        ));
    };
    Ok(format!("sha256:{:x}", Sha256::digest(bytes)))
}

fn validate_managed_runtime_context(spec: &ContainerSpec) -> Result<&RuntimeContext, RuntimeError> {
    let managed = spec.managed_service_context.as_ref().ok_or_else(|| {
        RuntimeError::InvalidRuntimeContext(
            "managed runtime context requires managed_service_context".to_string(),
        )
    })?;
    managed.validate()?;
    let context = spec.runtime_context.as_ref().ok_or_else(|| {
        RuntimeError::InvalidRuntimeContext(
            "managed_service_context requires an Agent-materialized runtime context".to_string(),
        )
    })?;
    context.validate()?;
    if context.contract != spec.runtime_contract {
        return Err(RuntimeError::InvalidRuntimeContext(
            "runtime context contract differs from ContainerSpec contract".to_string(),
        ));
    }
    Ok(context)
}

fn validate_judge_sandbox_spec(spec: &ContainerSpec) -> Result<&RuntimeContext, RuntimeError> {
    if spec.service_id != "judge-worker" {
        return Err(RuntimeError::InvalidRuntimeContract(
            "judge-sandbox-v1 is restricted to service_id=judge-worker".to_string(),
        ));
    }
    if !spec.command.is_empty() {
        return Err(RuntimeError::InvalidRuntimeContract(
            "judge-sandbox-v1 uses the signed image entrypoint and forbids command overrides"
                .to_string(),
        ));
    }
    if spec.published_endpoint.is_some() {
        return Err(RuntimeError::InvalidRuntimeContract(
            "judge-sandbox-v1 is an internal worker and cannot publish a host port".to_string(),
        ));
    }
    let context = validate_managed_runtime_context(spec)?;
    for value in &spec.environment {
        let (key, configured) = value.split_once('=').unwrap_or((value.as_str(), ""));
        if key == "OJOS_ALLOW_CGROUP_FALLBACK" && !configured.eq_ignore_ascii_case("false") {
            return Err(RuntimeError::InvalidRuntimeContract(
                "judge-sandbox-v1 forbids OJOS_ALLOW_CGROUP_FALLBACK".to_string(),
            ));
        }
        if key == "OJOS_NSJAIL_NO_PIVOTROOT" && !configured.eq_ignore_ascii_case("false") {
            return Err(RuntimeError::InvalidRuntimeContract(
                "judge-sandbox-v1 forbids OJOS_NSJAIL_NO_PIVOTROOT".to_string(),
            ));
        }
        if key == "OJOS_MANAGED_WORKLOAD" && !configured.eq_ignore_ascii_case("true") {
            return Err(RuntimeError::InvalidRuntimeContract(
                "judge-sandbox-v1 forbids disabling OJOS_MANAGED_WORKLOAD".to_string(),
            ));
        }
    }
    Ok(context)
}

fn managed_service_context_mount(context: &RuntimeContext) -> Mount {
    Mount {
        target: Some(MANAGED_SERVICE_CONTEXT_TARGET.to_string()),
        source: Some(context.service_context_directory.clone()),
        typ: Some(MountType::BIND),
        read_only: Some(true),
        bind_options: Some(MountBindOptions {
            propagation: Some(MountBindOptionsPropagationEnum::RPRIVATE),
            non_recursive: Some(false),
            create_mountpoint: Some(false),
            ..Default::default()
        }),
        ..Default::default()
    }
}

fn judge_sandbox_mounts(context: &RuntimeContext) -> Vec<Mount> {
    vec![
        Mount {
            target: Some("/var/lib/ojos-worker/work".to_string()),
            source: Some(context.scratch_directory.clone()),
            typ: Some(MountType::BIND),
            read_only: Some(false),
            bind_options: Some(MountBindOptions {
                propagation: Some(MountBindOptionsPropagationEnum::RPRIVATE),
                non_recursive: Some(false),
                create_mountpoint: Some(false),
                ..Default::default()
            }),
            ..Default::default()
        },
        Mount {
            target: Some("/var/lib/ojos-worker/cache".to_string()),
            source: Some(context.cache_volume_name.clone()),
            typ: Some(MountType::VOLUME),
            read_only: Some(false),
            volume_options: Some(MountVolumeOptions {
                no_copy: Some(true),
                ..Default::default()
            }),
            ..Default::default()
        },
        Mount {
            target: Some("/sys/fs/cgroup".to_string()),
            source: Some("/sys/fs/cgroup".to_string()),
            typ: Some(MountType::BIND),
            read_only: Some(false),
            bind_options: Some(MountBindOptions {
                propagation: Some(MountBindOptionsPropagationEnum::RPRIVATE),
                non_recursive: Some(false),
                create_mountpoint: Some(false),
                ..Default::default()
            }),
            ..Default::default()
        },
        Mount {
            target: Some("/tmp".to_string()),
            typ: Some(MountType::TMPFS),
            read_only: Some(false),
            tmpfs_options: Some(MountTmpfsOptions {
                size_bytes: Some(JUDGE_SANDBOX_V1_TMPFS_BYTES),
                mode: Some(0o1777),
                options: None,
            }),
            ..Default::default()
        },
        managed_service_context_mount(context),
    ]
}

fn attest_judge_sandbox_inspection(
    inspected: &bollard::models::ContainerInspectResponse,
    contract: &RuntimeContract,
    runtime_policy_sha256: &str,
    claimed_effective_sha256: &str,
) -> Result<(), RuntimeError> {
    attest_managed_context_environment(inspected)?;
    let host = inspected.host_config.as_ref().ok_or_else(|| {
        RuntimeError::InvalidRuntimeContext(
            "judge-sandbox-v1 container inspection has no HostConfig".to_string(),
        )
    })?;
    let expected_cap_add = vec![
        "SYS_ADMIN".to_string(),
        "SYS_CHROOT".to_string(),
        "NET_ADMIN".to_string(),
    ];
    if host.privileged != Some(true)
        || host.cap_add.as_ref() != Some(&expected_cap_add)
        || host.cap_drop.as_ref().is_some_and(|caps| !caps.is_empty())
        || host.cgroupns_mode != Some(HostConfigCgroupnsModeEnum::HOST)
        || host.memory != Some(JUDGE_SANDBOX_V1_MEMORY_BYTES)
        || host.memory_swap != Some(JUDGE_SANDBOX_V1_MEMORY_BYTES)
        || host.pids_limit != Some(JUDGE_SANDBOX_V1_PIDS_LIMIT)
        || host.readonly_rootfs == Some(true)
        || host.network_mode.as_deref() != Some("bridge")
        || host.init != Some(true)
        || host
            .port_bindings
            .as_ref()
            .is_some_and(|bindings| !bindings.is_empty())
    {
        return Err(RuntimeError::InvalidRuntimeContext(
            "judge-sandbox-v1 Docker HostConfig drifted from its fixed security/resource policy"
                .to_string(),
        ));
    }
    if inspected
        .config
        .as_ref()
        .and_then(|config| config.user.as_deref())
        != Some("0:0")
    {
        return Err(RuntimeError::InvalidRuntimeContext(
            "judge-sandbox-v1 must run its supervisor as container user 0:0".to_string(),
        ));
    }
    let security = host.security_opt.as_deref().unwrap_or_default();
    if !judge_sandbox_security_options_are_exact(security) {
        return Err(RuntimeError::InvalidRuntimeContext(
            "judge-sandbox-v1 security options must contain apparmor=unconfined exactly once, may contain Docker's implicit label=disable once, and cannot contain any other option"
                .to_string(),
        ));
    }
    let mounts = host.mounts.as_deref().unwrap_or_default();
    if mounts.len() != 5 {
        return Err(RuntimeError::InvalidRuntimeContext(format!(
            "judge-sandbox-v1 requires exactly five typed mounts, found {}",
            mounts.len()
        )));
    }
    let mount = |target: &str| {
        mounts
            .iter()
            .find(|mount| mount.target.as_deref() == Some(target))
            .ok_or_else(|| {
                RuntimeError::InvalidRuntimeContext(format!(
                    "judge-sandbox-v1 is missing mount target {target}"
                ))
            })
    };
    let scratch = mount("/var/lib/ojos-worker/work")?;
    let cache = mount("/var/lib/ojos-worker/cache")?;
    let cgroup = mount("/sys/fs/cgroup")?;
    let tmp = mount("/tmp")?;
    let service_context = mount(MANAGED_SERVICE_CONTEXT_TARGET)?;
    if scratch.typ != Some(MountType::BIND)
        || scratch.read_only == Some(true)
        || !judge_sandbox_has_exact_bind_options(scratch)
        || cache.typ != Some(MountType::VOLUME)
        || cache.read_only == Some(true)
        || cache.bind_options.is_some()
        || cache.volume_options.as_ref().is_none_or(|options| {
            options.no_copy != Some(true)
                || options.labels.is_some()
                || options.driver_config.is_some()
                || options.subpath.is_some()
        })
        || cgroup.typ != Some(MountType::BIND)
        || cgroup.read_only == Some(true)
        || !judge_sandbox_has_exact_bind_options(cgroup)
        || tmp.typ != Some(MountType::TMPFS)
        || tmp.read_only == Some(true)
        || tmp.source.is_some()
        || tmp.bind_options.is_some()
        || tmp.volume_options.is_some()
        || tmp.tmpfs_options.as_ref().is_none_or(|options| {
            options.size_bytes != Some(JUDGE_SANDBOX_V1_TMPFS_BYTES)
                || options.mode != Some(0o1777)
                || options
                    .options
                    .as_ref()
                    .is_some_and(|values| !values.is_empty())
        })
        || service_context.typ != Some(MountType::BIND)
        || service_context.read_only != Some(true)
        || !judge_sandbox_has_exact_bind_options(service_context)
        || cgroup.source.as_deref() != Some("/sys/fs/cgroup")
    {
        return Err(RuntimeError::InvalidRuntimeContext(
            "judge-sandbox-v1 mount types/access do not match the fixed v1 contract".to_string(),
        ));
    }
    let context = RuntimeContext {
        contract: contract.clone(),
        runtime_policy_sha256: runtime_policy_sha256.to_string(),
        scratch_directory: scratch.source.clone().unwrap_or_default(),
        cache_volume_name: cache.source.clone().unwrap_or_default(),
        service_context_directory: service_context.source.clone().unwrap_or_default(),
    };
    context.validate()?;
    let actual = format!(
        "sha256:{:x}",
        Sha256::digest(serde_json::to_vec(&context).map_err(|error| {
            RuntimeError::InvalidRuntimeContext(format!(
                "cannot encode inspected runtime context: {error}"
            ))
        })?)
    );
    if claimed_effective_sha256 != actual {
        return Err(RuntimeError::InvalidRuntimeContext(format!(
            "judge-sandbox-v1 effective runtime digest drift: claimed {claimed_effective_sha256}, inspected {actual}"
        )));
    }
    Ok(())
}

fn attest_standard_managed_context_inspection(
    inspected: &bollard::models::ContainerInspectResponse,
    contract: &RuntimeContract,
    runtime_policy_sha256: &str,
    claimed_effective_sha256: &str,
) -> Result<(), RuntimeError> {
    attest_managed_context_environment(inspected)?;
    let host = inspected.host_config.as_ref().ok_or_else(|| {
        RuntimeError::InvalidRuntimeContext(
            "managed standard container inspection has no HostConfig".to_string(),
        )
    })?;
    if host.privileged == Some(true)
        || host.cap_add.as_ref().is_some_and(|caps| !caps.is_empty())
        || host.cgroupns_mode == Some(HostConfigCgroupnsModeEnum::HOST)
        || host
            .security_opt
            .as_ref()
            .is_some_and(|options| !options.is_empty())
    {
        return Err(RuntimeError::InvalidRuntimeContext(
            "standard-container-v1 acquired runtime privileges outside its fixed contract"
                .to_string(),
        ));
    }
    let mounts = host.mounts.as_deref().unwrap_or_default();
    if mounts.len() != 1 {
        return Err(RuntimeError::InvalidRuntimeContext(format!(
            "managed standard-container-v1 requires exactly one service context mount, found {}",
            mounts.len()
        )));
    }
    let service_context = &mounts[0];
    if service_context.target.as_deref() != Some(MANAGED_SERVICE_CONTEXT_TARGET)
        || service_context.typ != Some(MountType::BIND)
        || service_context.read_only != Some(true)
    {
        return Err(RuntimeError::InvalidRuntimeContext(
            "managed standard-container-v1 service context mount drifted".to_string(),
        ));
    }
    let context = RuntimeContext {
        contract: contract.clone(),
        runtime_policy_sha256: runtime_policy_sha256.to_string(),
        scratch_directory: String::new(),
        cache_volume_name: String::new(),
        service_context_directory: service_context.source.clone().unwrap_or_default(),
    };
    context.validate()?;
    let actual = format!(
        "sha256:{:x}",
        Sha256::digest(serde_json::to_vec(&context).map_err(|error| {
            RuntimeError::InvalidRuntimeContext(format!(
                "cannot encode inspected runtime context: {error}"
            ))
        })?)
    );
    if claimed_effective_sha256 != actual {
        return Err(RuntimeError::InvalidRuntimeContext(format!(
            "managed standard-container-v1 effective runtime digest drift: claimed {claimed_effective_sha256}, inspected {actual}"
        )));
    }
    Ok(())
}

fn attest_managed_context_environment(
    inspected: &bollard::models::ContainerInspectResponse,
) -> Result<(), RuntimeError> {
    let environment = inspected
        .config
        .as_ref()
        .and_then(|config| config.env.as_deref())
        .unwrap_or_default();
    for expected in [
        "OJOS_MANAGED_WORKLOAD=true",
        "OJOS_SERVICE_CONTEXT_FILE=/run/ojos/service/context.json",
    ] {
        if environment
            .iter()
            .filter(|value| value.as_str() == expected)
            .count()
            != 1
        {
            return Err(RuntimeError::InvalidRuntimeContext(format!(
                "managed container environment must contain exactly one {expected}"
            )));
        }
    }
    Ok(())
}

fn force_managed_context_environment(environment: &mut Vec<String>) {
    environment.retain(|value| !value.starts_with("OJOS_MANAGED_WORKLOAD="));
    environment.push("OJOS_MANAGED_WORKLOAD=true".to_string());
    environment.retain(|value| !value.starts_with("OJOS_SERVICE_CONTEXT_FILE="));
    environment.push(format!(
        "OJOS_SERVICE_CONTEXT_FILE={MANAGED_SERVICE_CONTEXT_FILE}"
    ));
}

fn container_create_body(spec: &ContainerSpec) -> Result<ContainerCreateBody, RuntimeError> {
    spec.runtime_contract.validate()?;
    let effective_runtime_sha256 = effective_runtime_sha256(spec)?;
    let mut labels = spec.labels.clone();
    labels.insert("ojos.deployment_id".to_string(), spec.deployment_id.clone());
    labels.insert("ojos.service_id".to_string(), spec.service_id.clone());
    labels.insert("ojos.generation".to_string(), spec.generation.to_string());
    labels.insert("ojos.artifact_digest".to_string(), spec.image.to_string());
    labels.insert(
        "ojos.runtime_profile".to_string(),
        spec.runtime_contract.id.to_string(),
    );
    labels.insert(
        "ojos.runtime_profile_sha256".to_string(),
        spec.runtime_contract.profile_sha256.clone(),
    );
    labels.insert(
        "ojos.runtime_effective_sha256".to_string(),
        effective_runtime_sha256,
    );
    if let Some(context) = spec.runtime_context.as_ref() {
        labels.insert(
            "ojos.runtime_policy_sha256".to_string(),
            context.runtime_policy_sha256.clone(),
        );
    }
    let (exposed_ports, port_bindings) = if let Some(endpoint) = &spec.published_endpoint {
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
        (Some(vec![docker_port]), Some(port_bindings))
    } else {
        (None, None)
    };
    let mut host_config = port_bindings.map(|port_bindings| HostConfig {
        port_bindings: Some(port_bindings),
        ..Default::default()
    });
    let mut environment = spec.environment.clone();
    let user = match spec.runtime_contract.id {
        RuntimeProfile::StandardV1 => {
            match (
                spec.managed_service_context.as_ref(),
                spec.runtime_context.as_ref(),
            ) {
                (None, None) => {}
                (Some(_), Some(_)) => {
                    let context = validate_managed_runtime_context(spec)?;
                    force_managed_context_environment(&mut environment);
                    let config = host_config.get_or_insert_with(HostConfig::default);
                    config.mounts = Some(vec![managed_service_context_mount(context)]);
                }
                (Some(_), None) => {
                    return Err(RuntimeError::InvalidRuntimeContext(
                        "managed_service_context requires Agent materialization before Docker create"
                            .to_string(),
                    ));
                }
                (None, Some(_)) => {
                    return Err(RuntimeError::InvalidRuntimeContext(
                        "a standard runtime context is valid only with managed_service_context"
                            .to_string(),
                    ));
                }
            }
            None
        }
        RuntimeProfile::JudgeSandboxV1 => {
            let context = validate_judge_sandbox_spec(spec)?;
            environment.retain(|value| !value.starts_with("OJOS_CGROUP_V2_ROOT="));
            environment.push("OJOS_CGROUP_V2_ROOT=/sys/fs/cgroup".to_string());
            environment.retain(|value| !value.starts_with("OJOS_ALLOW_CGROUP_FALLBACK="));
            environment.push("OJOS_ALLOW_CGROUP_FALLBACK=false".to_string());
            environment.retain(|value| !value.starts_with("OJOS_NSJAIL_NO_PIVOTROOT="));
            environment.push("OJOS_NSJAIL_NO_PIVOTROOT=false".to_string());
            force_managed_context_environment(&mut environment);
            let config = host_config.get_or_insert_with(HostConfig::default);
            config.memory = Some(JUDGE_SANDBOX_V1_MEMORY_BYTES);
            config.memory_swap = Some(JUDGE_SANDBOX_V1_MEMORY_BYTES);
            config.pids_limit = Some(JUDGE_SANDBOX_V1_PIDS_LIMIT);
            config.cgroupns_mode = Some(HostConfigCgroupnsModeEnum::HOST);
            config.cap_add = Some(vec![
                "SYS_ADMIN".to_string(),
                "SYS_CHROOT".to_string(),
                "NET_ADMIN".to_string(),
            ]);
            config.privileged = Some(true);
            config.readonly_rootfs = Some(false);
            config.security_opt = Some(vec![JUDGE_SANDBOX_V1_APPARMOR_SECURITY_OPT.to_string()]);
            config.mounts = Some(judge_sandbox_mounts(context));
            config.network_mode = Some("bridge".to_string());
            config.init = Some(true);
            Some("0:0".to_string())
        }
    };
    Ok(ContainerCreateBody {
        image: Some(spec.image.to_string()),
        cmd: (!spec.command.is_empty()).then(|| spec.command.clone()),
        env: (!environment.is_empty()).then_some(environment),
        user,
        labels: Some(labels),
        exposed_ports,
        host_config,
        ..Default::default()
    })
}

fn judge_sandbox_security_options_are_exact(options: &[String]) -> bool {
    let mut apparmor = 0_usize;
    let mut privileged_label = 0_usize;
    for option in options {
        match option.as_str() {
            JUDGE_SANDBOX_V1_APPARMOR_SECURITY_OPT => apparmor += 1,
            JUDGE_SANDBOX_V1_PRIVILEGED_LABEL_SECURITY_OPT => privileged_label += 1,
            _ => return false,
        }
    }
    apparmor == 1 && privileged_label <= 1
}

fn judge_sandbox_has_exact_bind_options(mount: &Mount) -> bool {
    mount.bind_options.as_ref().is_some_and(|options| {
        options.propagation == Some(MountBindOptionsPropagationEnum::RPRIVATE)
            && options.non_recursive != Some(true)
            && options.create_mountpoint != Some(true)
            && options.read_only_non_recursive != Some(true)
            && options.read_only_force_recursive != Some(true)
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
            runtime_contract: RuntimeContract::standard_v1(),
            runtime_context: None,
            managed_service_context: None,
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

    fn judge_test_context() -> RuntimeContext {
        let root = if cfg!(windows) {
            "C:\\ojos"
        } else {
            "/var/lib/ojos"
        };
        RuntimeContext {
            contract: RuntimeContract::judge_sandbox_v1(),
            runtime_policy_sha256: format!("sha256:{}", "b".repeat(64)),
            scratch_directory: format!("{root}/contexts/deployment-1/work"),
            cache_volume_name: "ojos-judge-cache-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_string(),
            service_context_directory: format!("{root}/contexts/deployment-1/service"),
        }
    }

    fn judge_test_spec() -> ContainerSpec {
        ContainerSpec {
            deployment_id: "deployment-1".to_string(),
            service_id: "judge-worker".to_string(),
            generation: 1,
            image: OciImageReference::parse(&format!("ghcr.io/acme/judge-worker@sha256:{DIGEST}"))
                .unwrap(),
            runtime_contract: RuntimeContract::judge_sandbox_v1(),
            runtime_context: Some(judge_test_context()),
            managed_service_context: Some(ManagedServiceContextSpec {
                generation: 3,
                node_id: "node-1".to_string(),
                gateway_origin: "https://gateway.internal".to_string(),
                gateway_ca_pem: Some(
                    "-----BEGIN CERTIFICATE-----\nfixture\n-----END CERTIFICATE-----\n".to_string(),
                ),
                bindings: BTreeMap::from([(
                    "storage_get".to_string(),
                    ManagedApiBinding {
                        binding_id: "binding-1".to_string(),
                        api_id: "storage.object.get".to_string(),
                        timeout_ms: 300_000,
                        context_generation: 3,
                    },
                )]),
                events: None,
            }),
            command: Vec::new(),
            environment: vec!["OJOS_ALLOW_CGROUP_FALLBACK=false".to_string()],
            labels: HashMap::new(),
            published_endpoint: None,
        }
    }

    #[test]
    fn judge_sandbox_profile_digest_is_frozen_to_the_canonical_contract() {
        assert_eq!(
            format!(
                "sha256:{:x}",
                Sha256::digest(STANDARD_RUNTIME_PROFILE_CANONICAL_JSON.as_bytes())
            ),
            STANDARD_RUNTIME_PROFILE_SHA256
        );
        assert_eq!(
            format!(
                "sha256:{:x}",
                Sha256::digest(JUDGE_SANDBOX_V1_CANONICAL_JSON.as_bytes())
            ),
            JUDGE_SANDBOX_V1_PROFILE_SHA256
        );
        RuntimeContract::judge_sandbox_v1().validate().unwrap();
        let mut changed = RuntimeContract::judge_sandbox_v1();
        changed.profile_sha256 = format!("sha256:{}", "0".repeat(64));
        assert!(matches!(
            changed.validate(),
            Err(RuntimeError::InvalidRuntimeContract(_))
        ));
    }

    #[test]
    fn judge_sandbox_docker_host_config_is_exact_and_has_no_arbitrary_host_mount() {
        let spec = judge_test_spec();
        let body = container_create_body(&spec).unwrap();
        assert_eq!(body.user.as_deref(), Some("0:0"));
        assert!(
            body.env
                .as_ref()
                .unwrap()
                .contains(&"OJOS_CGROUP_V2_ROOT=/sys/fs/cgroup".to_string())
        );
        let host = body.host_config.unwrap();
        assert_eq!(host.privileged, Some(true));
        assert!(host.cap_drop.as_ref().is_none_or(Vec::is_empty));
        assert_eq!(
            host.cap_add,
            Some(vec![
                "SYS_ADMIN".to_string(),
                "SYS_CHROOT".to_string(),
                "NET_ADMIN".to_string()
            ])
        );
        assert_eq!(host.cgroupns_mode, Some(HostConfigCgroupnsModeEnum::HOST));
        assert_eq!(host.memory, Some(JUDGE_SANDBOX_V1_MEMORY_BYTES));
        assert_eq!(host.pids_limit, Some(JUDGE_SANDBOX_V1_PIDS_LIMIT));
        assert_eq!(host.readonly_rootfs, Some(false));
        assert_eq!(
            host.security_opt,
            Some(vec!["apparmor=unconfined".to_string()])
        );
        let mounts = host.mounts.unwrap();
        assert_eq!(mounts.len(), 5);
        let cgroup = mounts
            .iter()
            .find(|mount| mount.target.as_deref() == Some("/sys/fs/cgroup"))
            .unwrap();
        assert_eq!(cgroup.source.as_deref(), Some("/sys/fs/cgroup"));
        assert_eq!(cgroup.read_only, Some(false));
        assert!(
            mounts
                .iter()
                .all(|mount| mount.target.as_deref() != Some("/"))
        );
        let service_context = mounts
            .iter()
            .find(|mount| mount.target.as_deref() == Some(MANAGED_SERVICE_CONTEXT_TARGET))
            .unwrap();
        assert_eq!(service_context.typ, Some(MountType::BIND));
        assert_eq!(service_context.read_only, Some(true));
        let cache = mounts
            .iter()
            .find(|mount| mount.target.as_deref() == Some("/var/lib/ojos-worker/cache"))
            .unwrap();
        assert_eq!(cache.typ, Some(MountType::VOLUME));
        assert_eq!(
            cache.source.as_deref(),
            Some("ojos-judge-cache-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa")
        );
        assert_eq!(cache.read_only, Some(false));
        assert_eq!(
            cache
                .volume_options
                .as_ref()
                .and_then(|options| options.no_copy),
            Some(true)
        );
        let tmp = mounts
            .iter()
            .find(|mount| mount.target.as_deref() == Some("/tmp"))
            .unwrap();
        assert_eq!(tmp.typ, Some(MountType::TMPFS));
        assert_eq!(tmp.read_only, Some(false));
        assert!(tmp.source.is_none());
        assert!(tmp.bind_options.is_none());
        assert!(tmp.volume_options.is_none());
        let tmpfs = tmp.tmpfs_options.as_ref().unwrap();
        assert_eq!(tmpfs.size_bytes, Some(JUDGE_SANDBOX_V1_TMPFS_BYTES));
        assert_eq!(tmpfs.mode, Some(0o1777));
        assert!(tmpfs.options.is_none());

        let encoded = serde_json::to_value(tmp).unwrap();
        assert_eq!(
            encoded,
            serde_json::json!({
                "Target": "/tmp",
                "Type": "tmpfs",
                "ReadOnly": false,
                "TmpfsOptions": {
                    "SizeBytes": JUDGE_SANDBOX_V1_TMPFS_BYTES,
                    "Mode": 0o1777
                }
            })
        );
    }

    #[test]
    fn judge_sandbox_security_option_attestation_accepts_only_engine_normalization() {
        for valid in [
            vec!["apparmor=unconfined".to_string()],
            vec![
                "apparmor=unconfined".to_string(),
                "label=disable".to_string(),
            ],
            vec![
                "label=disable".to_string(),
                "apparmor=unconfined".to_string(),
            ],
        ] {
            assert!(judge_sandbox_security_options_are_exact(&valid));
        }

        for invalid in [
            Vec::<String>::new(),
            vec!["label=disable".to_string()],
            vec!["apparmor:unconfined".to_string()],
            vec![
                "apparmor=unconfined".to_string(),
                "seccomp=unconfined".to_string(),
            ],
            vec![
                "apparmor=unconfined".to_string(),
                "apparmor=unconfined".to_string(),
            ],
            vec![
                "apparmor=unconfined".to_string(),
                "label=disable".to_string(),
                "label=disable".to_string(),
            ],
        ] {
            assert!(!judge_sandbox_security_options_are_exact(&invalid));
        }
    }

    #[test]
    fn judge_cache_volume_has_exact_release_ownership_labels() {
        let spec = judge_test_spec();
        let volume = spec.managed_volume_spec().unwrap().unwrap();
        assert_eq!(
            volume.name,
            "ojos-judge-cache-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
        );
        assert_eq!(volume.logical_name, JUDGE_CACHE_VOLUME_LOGICAL_NAME);
        assert_eq!(volume.lifecycle, RELEASE_VOLUME_LIFECYCLE);
        assert_eq!(volume.artifact_digest, spec.image.to_string());
        assert_eq!(
            volume.ownership_labels().unwrap(),
            HashMap::from([
                (
                    MANAGED_VOLUME_OWNER_LABEL.to_string(),
                    MANAGED_VOLUME_OWNER.to_string(),
                ),
                (
                    MANAGED_VOLUME_DEPLOYMENT_LABEL.to_string(),
                    "deployment-1".to_string(),
                ),
                (
                    MANAGED_VOLUME_SERVICE_LABEL.to_string(),
                    "judge-worker".to_string(),
                ),
                (
                    MANAGED_VOLUME_ARTIFACT_LABEL.to_string(),
                    spec.image.to_string(),
                ),
                (
                    MANAGED_VOLUME_PROFILE_LABEL.to_string(),
                    JUDGE_SANDBOX_V1_PROFILE_SHA256.to_string(),
                ),
                (
                    MANAGED_VOLUME_LOGICAL_NAME_LABEL.to_string(),
                    JUDGE_CACHE_VOLUME_LOGICAL_NAME.to_string(),
                ),
                (
                    MANAGED_VOLUME_LIFECYCLE_LABEL.to_string(),
                    RELEASE_VOLUME_LIFECYCLE.to_string(),
                ),
            ])
        );
    }

    #[test]
    fn managed_volume_attestation_refuses_same_name_with_foreign_labels() {
        let spec = judge_test_spec().managed_volume_spec().unwrap().unwrap();
        let mut volume = bollard::models::Volume {
            name: spec.name.clone(),
            driver: "local".to_string(),
            mountpoint: String::new(),
            created_at: None,
            status: None,
            labels: spec.ownership_labels().unwrap(),
            scope: Some(bollard::models::VolumeScopeEnum::LOCAL),
            cluster_volume: None,
            options: HashMap::new(),
            usage_data: None,
        };
        attest_managed_volume(&volume, &spec).unwrap();
        volume.labels.insert(
            MANAGED_VOLUME_DEPLOYMENT_LABEL.to_string(),
            "another-deployment".to_string(),
        );
        assert!(matches!(
            attest_managed_volume(&volume, &spec),
            Err(RuntimeError::InvalidRuntimeContext(_))
        ));
    }

    #[test]
    fn judge_sandbox_attestation_rejects_host_config_drift() {
        let spec = judge_test_spec();
        let body = container_create_body(&spec).unwrap();
        let effective = effective_runtime_sha256(&spec).unwrap();
        let context = spec.runtime_context.as_ref().unwrap();
        let mut inspected = bollard::models::ContainerInspectResponse {
            config: Some(bollard::models::ContainerConfig {
                user: body.user,
                env: body.env,
                labels: body.labels,
                ..Default::default()
            }),
            host_config: body.host_config,
            ..Default::default()
        };
        attest_judge_sandbox_inspection(
            &inspected,
            &spec.runtime_contract,
            &context.runtime_policy_sha256,
            &effective,
        )
        .unwrap();

        inspected
            .host_config
            .as_mut()
            .unwrap()
            .security_opt
            .as_mut()
            .unwrap()
            .push("label=disable".to_string());
        attest_judge_sandbox_inspection(
            &inspected,
            &spec.runtime_contract,
            &context.runtime_policy_sha256,
            &effective,
        )
        .unwrap();

        inspected.host_config.as_mut().unwrap().privileged = Some(false);
        assert!(matches!(
            attest_judge_sandbox_inspection(
                &inspected,
                &spec.runtime_contract,
                &context.runtime_policy_sha256,
                &effective,
            ),
            Err(RuntimeError::InvalidRuntimeContext(_))
        ));
    }

    #[test]
    fn judge_sandbox_attestation_normalizes_only_writable_mount_read_only_none() {
        let spec = judge_test_spec();
        let body = container_create_body(&spec).unwrap();
        let effective = effective_runtime_sha256(&spec).unwrap();
        let context = spec.runtime_context.as_ref().unwrap();
        let mut inspected = bollard::models::ContainerInspectResponse {
            config: Some(bollard::models::ContainerConfig {
                user: body.user,
                env: body.env,
                labels: body.labels,
                ..Default::default()
            }),
            host_config: body.host_config,
            ..Default::default()
        };
        let writable_targets = [
            "/var/lib/ojos-worker/work",
            "/var/lib/ojos-worker/cache",
            "/sys/fs/cgroup",
            "/tmp",
        ];
        for target in writable_targets {
            inspected
                .host_config
                .as_mut()
                .unwrap()
                .mounts
                .as_mut()
                .unwrap()
                .iter_mut()
                .find(|mount| mount.target.as_deref() == Some(target))
                .unwrap()
                .read_only = None;
        }
        attest_judge_sandbox_inspection(
            &inspected,
            &spec.runtime_contract,
            &context.runtime_policy_sha256,
            &effective,
        )
        .unwrap();

        for target in writable_targets {
            let mut drifted = inspected.clone();
            drifted
                .host_config
                .as_mut()
                .unwrap()
                .mounts
                .as_mut()
                .unwrap()
                .iter_mut()
                .find(|mount| mount.target.as_deref() == Some(target))
                .unwrap()
                .read_only = Some(true);
            assert!(matches!(
                attest_judge_sandbox_inspection(
                    &drifted,
                    &spec.runtime_contract,
                    &context.runtime_policy_sha256,
                    &effective,
                ),
                Err(RuntimeError::InvalidRuntimeContext(_))
            ));
        }

        inspected
            .host_config
            .as_mut()
            .unwrap()
            .mounts
            .as_mut()
            .unwrap()
            .iter_mut()
            .find(|mount| mount.target.as_deref() == Some(MANAGED_SERVICE_CONTEXT_TARGET))
            .unwrap()
            .read_only = None;
        assert!(matches!(
            attest_judge_sandbox_inspection(
                &inspected,
                &spec.runtime_contract,
                &context.runtime_policy_sha256,
                &effective,
            ),
            Err(RuntimeError::InvalidRuntimeContext(_))
        ));
    }

    #[test]
    fn judge_sandbox_attestation_requires_exact_rprivate_bind_options() {
        let spec = judge_test_spec();
        let body = container_create_body(&spec).unwrap();
        let effective = effective_runtime_sha256(&spec).unwrap();
        let context = spec.runtime_context.as_ref().unwrap();
        let mut inspected = bollard::models::ContainerInspectResponse {
            config: Some(bollard::models::ContainerConfig {
                user: body.user,
                env: body.env,
                labels: body.labels,
                ..Default::default()
            }),
            host_config: body.host_config,
            ..Default::default()
        };
        let bind_targets = [
            "/var/lib/ojos-worker/work",
            "/sys/fs/cgroup",
            MANAGED_SERVICE_CONTEXT_TARGET,
        ];
        for target in bind_targets {
            let options = inspected
                .host_config
                .as_mut()
                .unwrap()
                .mounts
                .as_mut()
                .unwrap()
                .iter_mut()
                .find(|mount| mount.target.as_deref() == Some(target))
                .unwrap()
                .bind_options
                .as_mut()
                .unwrap();
            options.non_recursive = None;
            options.create_mountpoint = None;
            options.read_only_non_recursive = None;
            options.read_only_force_recursive = None;
        }
        attest_judge_sandbox_inspection(
            &inspected,
            &spec.runtime_contract,
            &context.runtime_policy_sha256,
            &effective,
        )
        .unwrap();

        let option_mutations: [fn(&mut MountBindOptions); 6] = [
            |options| options.propagation = None,
            |options| options.propagation = Some(MountBindOptionsPropagationEnum::RSHARED),
            |options| options.non_recursive = Some(true),
            |options| options.create_mountpoint = Some(true),
            |options| options.read_only_non_recursive = Some(true),
            |options| options.read_only_force_recursive = Some(true),
        ];
        for target in bind_targets {
            let mut missing = inspected.clone();
            missing
                .host_config
                .as_mut()
                .unwrap()
                .mounts
                .as_mut()
                .unwrap()
                .iter_mut()
                .find(|mount| mount.target.as_deref() == Some(target))
                .unwrap()
                .bind_options = None;
            assert!(
                attest_judge_sandbox_inspection(
                    &missing,
                    &spec.runtime_contract,
                    &context.runtime_policy_sha256,
                    &effective,
                )
                .is_err()
            );

            for mutate in option_mutations {
                let mut drifted = inspected.clone();
                let options = drifted
                    .host_config
                    .as_mut()
                    .unwrap()
                    .mounts
                    .as_mut()
                    .unwrap()
                    .iter_mut()
                    .find(|mount| mount.target.as_deref() == Some(target))
                    .unwrap()
                    .bind_options
                    .as_mut()
                    .unwrap();
                mutate(options);
                assert!(
                    attest_judge_sandbox_inspection(
                        &drifted,
                        &spec.runtime_contract,
                        &context.runtime_policy_sha256,
                        &effective,
                    )
                    .is_err()
                );
            }
        }
    }

    #[test]
    fn judge_sandbox_attestation_rejects_cache_volume_option_drift() {
        let spec = judge_test_spec();
        let body = container_create_body(&spec).unwrap();
        let effective = effective_runtime_sha256(&spec).unwrap();
        let context = spec.runtime_context.as_ref().unwrap();
        let mut inspected = bollard::models::ContainerInspectResponse {
            config: Some(bollard::models::ContainerConfig {
                user: body.user,
                env: body.env,
                labels: body.labels,
                ..Default::default()
            }),
            host_config: body.host_config,
            ..Default::default()
        };
        let cache = inspected
            .host_config
            .as_mut()
            .unwrap()
            .mounts
            .as_mut()
            .unwrap()
            .iter_mut()
            .find(|mount| mount.target.as_deref() == Some("/var/lib/ojos-worker/cache"))
            .unwrap();
        cache.volume_options.as_mut().unwrap().no_copy = Some(false);
        assert!(matches!(
            attest_judge_sandbox_inspection(
                &inspected,
                &spec.runtime_contract,
                &context.runtime_policy_sha256,
                &effective,
            ),
            Err(RuntimeError::InvalidRuntimeContext(_))
        ));
    }

    #[test]
    fn judge_sandbox_attestation_rejects_tmpfs_contract_drift() {
        let spec = judge_test_spec();
        let body = container_create_body(&spec).unwrap();
        let effective = effective_runtime_sha256(&spec).unwrap();
        let context = spec.runtime_context.as_ref().unwrap();
        let inspected = bollard::models::ContainerInspectResponse {
            config: Some(bollard::models::ContainerConfig {
                user: body.user,
                env: body.env,
                labels: body.labels,
                ..Default::default()
            }),
            host_config: body.host_config,
            ..Default::default()
        };

        for mutate in [
            |mount: &mut Mount| mount.source = Some("forbidden".to_string()),
            |mount: &mut Mount| mount.bind_options = Some(MountBindOptions::default()),
            |mount: &mut Mount| mount.volume_options = Some(MountVolumeOptions::default()),
            |mount: &mut Mount| mount.tmpfs_options.as_mut().unwrap().size_bytes = Some(1),
            |mount: &mut Mount| mount.tmpfs_options.as_mut().unwrap().mode = Some(0o700),
            |mount: &mut Mount| {
                mount.tmpfs_options.as_mut().unwrap().options =
                    Some(vec![vec!["nosuid".to_string()]])
            },
            |mount: &mut Mount| mount.tmpfs_options = None,
        ] {
            let mut drifted = inspected.clone();
            let tmp = drifted
                .host_config
                .as_mut()
                .unwrap()
                .mounts
                .as_mut()
                .unwrap()
                .iter_mut()
                .find(|mount| mount.target.as_deref() == Some("/tmp"))
                .unwrap();
            mutate(tmp);
            assert!(matches!(
                attest_judge_sandbox_inspection(
                    &drifted,
                    &spec.runtime_contract,
                    &context.runtime_policy_sha256,
                    &effective,
                ),
                Err(RuntimeError::InvalidRuntimeContext(_))
            ));
        }
    }

    #[test]
    fn judge_sandbox_rejects_command_override_and_wrong_service() {
        let mut spec = judge_test_spec();
        spec.command = vec!["/bin/sh".to_string()];
        assert!(matches!(
            container_create_body(&spec),
            Err(RuntimeError::InvalidRuntimeContract(_))
        ));
        let mut spec = judge_test_spec();
        spec.service_id = "other".to_string();
        assert!(matches!(
            container_create_body(&spec),
            Err(RuntimeError::InvalidRuntimeContract(_))
        ));
    }

    #[test]
    fn standard_managed_context_is_read_only_without_sandbox_privileges() {
        let mut spec = judge_test_spec();
        spec.service_id = "problem-service".to_string();
        spec.runtime_contract = RuntimeContract::standard_v1();
        spec.runtime_context = Some(RuntimeContext {
            contract: RuntimeContract::standard_v1(),
            runtime_policy_sha256: format!("sha256:{}", "c".repeat(64)),
            scratch_directory: String::new(),
            cache_volume_name: String::new(),
            service_context_directory: if cfg!(windows) {
                "C:\\ojos\\contexts\\deployment-1\\service".to_string()
            } else {
                "/var/lib/ojos/contexts/deployment-1/service".to_string()
            },
        });

        let body = container_create_body(&spec).unwrap();
        assert!(body.user.is_none());
        let environment = body.env.unwrap();
        assert!(environment.contains(&"OJOS_MANAGED_WORKLOAD=true".to_string()));
        assert!(
            environment
                .contains(&"OJOS_SERVICE_CONTEXT_FILE=/run/ojos/service/context.json".to_string())
        );
        let host = body.host_config.unwrap();
        assert_ne!(host.privileged, Some(true));
        assert!(host.cap_add.as_ref().is_none_or(Vec::is_empty));
        assert_ne!(host.cgroupns_mode, Some(HostConfigCgroupnsModeEnum::HOST));
        assert!(host.security_opt.as_ref().is_none_or(Vec::is_empty));
        assert_eq!(host.mounts.as_ref().map(Vec::len), Some(1));
        let context_mount = &host.mounts.unwrap()[0];
        assert_eq!(
            context_mount.target.as_deref(),
            Some(MANAGED_SERVICE_CONTEXT_TARGET)
        );
        assert_eq!(context_mount.typ, Some(MountType::BIND));
        assert_eq!(context_mount.read_only, Some(true));
    }

    #[test]
    fn managed_service_context_rejects_loopback_prefix_spoofing() {
        let mut context = judge_test_spec().managed_service_context.unwrap();
        for origin in [
            "http://127.0.0.1.example.test",
            "http://localhost.example.test",
            "http://127.0.0.1:not-a-port",
            "https://user@gateway.internal",
            "https://gateway.internal/",
            "https://gateway.internal/path",
            "https://gateway.internal?query=1",
            "https://gateway.internal#fragment",
            " https://gateway.internal",
        ] {
            context.gateway_origin = origin.to_string();
            assert!(
                matches!(
                    context.validate(),
                    Err(RuntimeError::InvalidRuntimeContext(_))
                ),
                "{origin}"
            );
        }
        for origin in [
            "http://127.0.0.1",
            "http://127.0.0.1:38123",
            "http://localhost:38123",
            "https://gateway.internal",
        ] {
            context.gateway_origin = origin.to_string();
            context.validate().unwrap();
        }
    }

    #[test]
    fn managed_service_context_requires_positive_generation_and_allows_empty_bindings() {
        let mut context = judge_test_spec().managed_service_context.unwrap();
        context.generation = 0;
        assert!(context.validate().is_err());
        context.generation = 4;
        assert!(context.validate().is_err());
        context
            .bindings
            .get_mut("storage_get")
            .unwrap()
            .context_generation = 4;
        context.validate().unwrap();
        context.bindings.clear();
        context.validate().unwrap();
    }

    #[test]
    fn managed_event_binding_requires_one_context_generation_and_canonical_sets() {
        let mut context = judge_test_spec().managed_service_context.unwrap();
        context.events = Some(ManagedEventBinding {
            connection_id: "shared-events".to_string(),
            stream: MANAGED_EVENT_STREAM_V1.to_string(),
            publish_types: vec![
                "io.example.deleted.v1".to_string(),
                "io.example.snapshot.v1".to_string(),
            ],
            subscriptions: vec![ManagedEventSubscription {
                event_type: "io.example.snapshot.v1".to_string(),
                consumer_group: "fixture-consumer".to_string(),
            }],
            generation: 3,
        });
        context.validate().unwrap();

        let mut generation_split = context.clone();
        generation_split.events.as_mut().unwrap().generation = 2;
        assert!(generation_split.validate().is_err());

        let mut service_split_stream = context.clone();
        service_split_stream.events.as_mut().unwrap().stream =
            "ojos:events:fixture-service".to_string();
        // The wire type permits future streams, but Store v1 is the authority
        // that deterministically selects the one shared v1 stream.
        service_split_stream.validate().unwrap();

        let mut noncanonical = context.clone();
        noncanonical
            .events
            .as_mut()
            .unwrap()
            .publish_types
            .reverse();
        assert!(noncanonical.validate().is_err());

        let mut duplicate = context;
        let first = duplicate.events.as_ref().unwrap().subscriptions[0].clone();
        duplicate.events.as_mut().unwrap().subscriptions.push(first);
        assert!(duplicate.validate().is_err());
    }

    #[test]
    fn managed_inventory_turns_incomplete_labels_into_reportable_drift() {
        let observation = inspected_runtime_observation(
            "deployment-expected",
            "container-expected",
            RuntimeInstance {
                deployment_id: "deployment-expected".to_string(),
                service_id: String::new(),
                release_version: String::new(),
                container_id: "container-expected".to_string(),
                artifact_digest: String::new(),
                runtime_contract: RuntimeContract::standard_v1(),
                runtime_policy_sha256: String::new(),
                effective_runtime_sha256: String::new(),
                runtime_attested: true,
                desired_state: RuntimeDesiredState::Running,
                observed_state: RuntimeObservedState::Running,
                health: "HEALTHY".to_string(),
            },
        );
        assert_eq!(observation.service_id, "<missing>");
        assert!(!observation.runtime_attested);
        assert!(observation.drift_reason.contains("service identity"));
        assert!(observation.drift_reason.contains("OCI artifact"));

        let printable = bounded_drift_reason("bad\nattestation\tstate");
        assert!(!printable.chars().any(char::is_control));
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

    #[test]
    fn docker_runtime_facts_normalize_security_capabilities() {
        let info = bollard::models::SystemInfo {
            server_version: Some("28.0.1".to_string()),
            operating_system: Some("Linux".to_string()),
            os_type: Some("linux".to_string()),
            architecture: Some("x86_64".to_string()),
            cgroup_version: Some(bollard::models::SystemInfoCgroupVersionEnum::_2),
            memory_limit: Some(true),
            pids_limit: Some(true),
            security_options: Some(vec![
                "name=seccomp,profile=builtin".to_string(),
                "name=apparmor".to_string(),
                "name=rootless".to_string(),
                "name=apparmor".to_string(),
            ]),
            ..Default::default()
        };
        let facts = docker_runtime_facts(&info);
        assert_eq!(facts.engine, "docker");
        assert_eq!(facts.cgroup_version, "2");
        assert!(facts.memory_limit);
        assert!(facts.pids_limit);
        assert!(facts.rootless);
        assert!(facts.apparmor);
        assert!(facts.seccomp);
        assert_eq!(facts.security_options.len(), 3);
    }

    fn runtime_instance(observed_state: RuntimeObservedState, health: &str) -> RuntimeInstance {
        RuntimeInstance {
            deployment_id: "deployment-1".to_string(),
            service_id: "service-1".to_string(),
            release_version: "1.0.0".to_string(),
            container_id: "container-1".to_string(),
            artifact_digest: format!("ghcr.io/acme/service@sha256:{DIGEST}"),
            runtime_contract: RuntimeContract::standard_v1(),
            runtime_policy_sha256: String::new(),
            effective_runtime_sha256: String::new(),
            runtime_attested: true,
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
    fn published_runtime_contracts_derive_their_canonical_health_policy() {
        let standard = RuntimeContract::standard_v1();
        let standard_policy = HealthGatePolicy::for_runtime_contract(&standard);
        assert_eq!(standard_policy, HealthGatePolicy::default());
        standard.validate_health_gate(&standard_policy).unwrap();

        let judge = RuntimeContract::judge_sandbox_v1();
        let judge_policy = HealthGatePolicy::for_runtime_contract(&judge);
        assert_eq!(judge_policy.timeout_ms, JUDGE_SANDBOX_V1_HEALTH_TIMEOUT_MS);
        assert_eq!(
            judge_policy.poll_interval_ms,
            JUDGE_SANDBOX_V1_HEALTH_POLL_INTERVAL_MS
        );
        assert_eq!(
            judge_policy.missing_healthcheck,
            MissingHealthcheckPolicy::Reject
        );
        judge.validate_health_gate(&judge_policy).unwrap();
        assert!(
            judge
                .validate_health_gate(&HealthGatePolicy::default())
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
