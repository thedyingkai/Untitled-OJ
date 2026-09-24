//! Deterministic container contracts; no environment reads or I/O.

use super::validation::{validate_safe_resource_name, validate_sha256_text};
use super::{
    JUDGE_CACHE_VOLUME_LOGICAL_NAME, MANAGED_RESOURCE_SECRET_ROOT, ManagedServiceContextSpec,
    ManagedVolumeSpec, RELEASE_VOLUME_LIFECYCLE, RetainedVolumeAttachmentV1,
};
use crate::{RuntimeContract, RuntimeError, RuntimeProfile};
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    fmt::{Display, Formatter},
    path::Path,
    sync::OnceLock,
};

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

    pub fn docker_port(&self) -> String {
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
    /// Agent-derived resource output files. Like `runtime_context`, this is an
    /// in-memory execution detail and can never be requested by control-plane
    /// JSON. The Agent may attach the same value to a migration or service
    /// runtime `ContainerSpec`.
    #[serde(skip)]
    pub resource_secret_file_mounts: Vec<ResourceSecretFileMount>,
    /// Signed Store projection of one stable platform-owned RETAIN volume.
    /// The Agent derives the Docker name and rejects arbitrary host paths.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retained_volume: Option<RetainedVolumeAttachmentV1>,
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

/// One Agent-owned secret output file exposed to a standard container at a
/// deterministic, read-only path. `host_source_path` is deliberately absent
/// from every serialized `ContainerSpec` representation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResourceSecretFileMount {
    pub resource_name: String,
    pub host_source_path: String,
}

impl ResourceSecretFileMount {
    pub fn container_destination(&self) -> Result<String, RuntimeError> {
        validate_safe_resource_name(&self.resource_name)?;
        Ok(format!(
            "{MANAGED_RESOURCE_SECRET_ROOT}/{}/output",
            self.resource_name
        ))
    }

    pub fn validate(&self) -> Result<(), RuntimeError> {
        self.container_destination()?;
        let source = Path::new(&self.host_source_path);
        if self.host_source_path.is_empty()
            || self.host_source_path.len() > 4_096
            || !source.is_absolute()
            || self.host_source_path.ends_with('/')
            || self.host_source_path.ends_with('\\')
            || source.components().any(|component| {
                matches!(
                    component,
                    std::path::Component::ParentDir | std::path::Component::CurDir
                )
            })
        {
            return Err(RuntimeError::InvalidRuntimeContext(
                "resource secret host_source_path must be an absolute normalized Agent-local path"
                    .to_string(),
            ));
        }
        Ok(())
    }
}

impl ContainerSpec {
    /// Expands the closed runtime profile into the only Docker named volume
    /// the Agent is allowed to own. Ordinary containers and unmaterialized
    /// wire payloads never request a volume.
    pub fn managed_volume_spec(&self) -> Result<Option<ManagedVolumeSpec>, RuntimeError> {
        if self.runtime_contract.id == RuntimeProfile::StandardV1 {
            let Some(attachment) = self.retained_volume.as_ref() else {
                return Ok(None);
            };
            attachment.validate_for_service(&self.service_id)?;
            let spec = ManagedVolumeSpec {
                name: attachment.docker_name(&self.service_id)?,
                deployment_id: self.deployment_id.clone(),
                service_id: self.service_id.clone(),
                artifact_digest: self.image.to_string(),
                runtime_contract: self.runtime_contract.clone(),
                logical_name: attachment.logical_name.clone(),
                lifecycle: attachment.lifecycle.clone(),
                owner_instance_id: attachment.owner_instance_id.clone(),
                target: attachment.target.clone(),
            };
            spec.validate()?;
            return Ok(Some(spec));
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
            owner_instance_id: String::new(),
            target: "/var/lib/ojos-worker/cache".to_string(),
        };
        spec.validate()?;
        Ok(Some(spec))
    }
}

pub fn validate_managed_runtime_context(
    spec: &ContainerSpec,
) -> Result<&RuntimeContext, RuntimeError> {
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

pub fn validate_judge_sandbox_spec(spec: &ContainerSpec) -> Result<&RuntimeContext, RuntimeError> {
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
