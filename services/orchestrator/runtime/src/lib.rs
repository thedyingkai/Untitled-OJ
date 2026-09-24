//! Runtime drivers used by node agents.
//!
//! The production container implementation talks to the Docker Engine API
//! directly.  No request field is interpolated into a shell command.

use async_trait::async_trait;
use bollard::Docker;
use bollard::auth::DockerCredentials;
use bollard::models::{
    ContainerCreateBody, ContainerSummaryStateEnum, HostConfig, HostConfigCgroupnsModeEnum,
    HostConfigLogConfig, Mount, MountBindOptions, MountBindOptionsPropagationEnum,
    MountTmpfsOptions, MountType, MountVolumeOptions, PortBinding, VolumeCreateRequest,
};
use bollard::query_parameters::{
    CreateContainerOptionsBuilder, CreateImageOptionsBuilder, ImportImageOptionsBuilder,
    ListContainersOptionsBuilder, RemoveContainerOptionsBuilder, RemoveVolumeOptionsBuilder,
    RestartContainerOptionsBuilder, StopContainerOptionsBuilder, WaitContainerOptionsBuilder,
};
use futures_util::TryStreamExt;
use regex::Regex;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashMap};
use std::fmt::Formatter;
use std::path::Path;
use std::sync::OnceLock;
// Compatibility exports retain the existing runtime source API.
use orchestrator_protocol::execution::validation::{
    validate_safe_resource_name, validate_sha256_text,
};
pub use orchestrator_protocol::*;

pub const JUDGE_SANDBOX_V1_MEMORY_BYTES: i64 = 2 * 1024 * 1024 * 1024;
pub const JUDGE_SANDBOX_V1_PIDS_LIMIT: i64 = 512;
pub const JUDGE_SANDBOX_V1_TMPFS_BYTES: i64 = 256 * 1024 * 1024;
pub const STANDARD_V3_PIDS_LIMIT: i64 = 512;
pub const STANDARD_V3_MEMORY_BYTES: i64 = 2 * 1024 * 1024 * 1024;
pub const STANDARD_V3_TMPFS_BYTES: i64 = 64 * 1024 * 1024;
pub const STANDARD_V3_USER: &str = "65532:65532";
pub const STANDARD_WORKLOAD_UID: u32 = 65_532;
pub const STANDARD_WORKLOAD_GID: u32 = 65_532;

/// Agent-local ownership policy for files bind-mounted into a workload.
///
/// `CurrentProcess` is intentionally limited to tests and explicitly selected
/// local development. Production Agents use [`Self::standard_v3`] so the
/// owner of a `0700` service-context directory and its `0600` files is the
/// same identity Docker applies to signed standard-v3 workloads. Keeping this
/// typed prevents a future container-user change from silently diverging from
/// the materialization policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkloadFileOwnership {
    CurrentProcess,
    Unix { uid: u32, gid: u32 },
}

impl WorkloadFileOwnership {
    pub const fn current_process() -> Self {
        Self::CurrentProcess
    }

    pub const fn standard_v3() -> Self {
        Self::Unix {
            uid: STANDARD_WORKLOAD_UID,
            gid: STANDARD_WORKLOAD_GID,
        }
    }

    pub const fn unix_ids(self) -> Option<(u32, u32)> {
        match self {
            Self::CurrentProcess => None,
            Self::Unix { uid, gid } => Some((uid, gid)),
        }
    }
}
pub const JUDGE_SANDBOX_V1_APPARMOR_PROFILE: &str = "unconfined";
const JUDGE_SANDBOX_V1_APPARMOR_SECURITY_OPT: &str = "apparmor=unconfined";
// Docker 29.5.2 adds this option while normalizing a privileged create. Older
// Engines may omit it from inspect, so attestation accepts it at most once but
// never sends it as caller-controlled policy.
const JUDGE_SANDBOX_V1_PRIVILEGED_LABEL_SECURITY_OPT: &str = "label=disable";

const WORKLOAD_VERIFIER_ENV_SHA256_LABEL: &str = "ojos.workload_verifier_env_sha256";

const RUNTIME_RETAINED_VOLUME_SHA256_LABEL: &str = "ojos.runtime_retained_volume_sha256";
const RUNTIME_RETAINED_VOLUME_ACCESS_LABEL: &str = "ojos.runtime_retained_volume_access";
const RUNTIME_RESOURCE_SECRET_MOUNTS_SHA256_LABEL: &str =
    "ojos.runtime_resource_secret_mounts_sha256";

const STANDARD_V3_NO_NEW_PRIVILEGES: &str = "no-new-privileges=true";
const STANDARD_V3_LOG_MAX_SIZE: &str = "10m";
const STANDARD_V3_LOG_MAX_FILES: &str = "3";
const MAX_REGISTRY_CREDENTIALS_BYTES: u64 = 64 * 1024;
const MAX_REGISTRY_CREDENTIALS: usize = 32;

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
            if labels
                .and_then(|labels| labels.get(MIGRATION_RUNTIME_ROLE_LABEL))
                .is_some_and(|role| role == MIGRATION_RUNTIME_ROLE)
            {
                continue;
            }
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

    /// Lists only containers explicitly marked as OJOS migration runtimes and
    /// projects their closed identity contract. Invalid or partial labels are
    /// reported as observations and are never silently skipped.
    pub async fn migration_container_inventory(
        &self,
        max_containers: usize,
    ) -> Result<MigrationContainerInventoryV1, RuntimeError> {
        if max_containers == 0 {
            return Err(RuntimeError::Engine(
                "migration container inventory limit must be positive".to_string(),
            ));
        }
        let filters = HashMap::from([(
            "label".to_string(),
            vec![format!(
                "{MIGRATION_RUNTIME_ROLE_LABEL}={MIGRATION_RUNTIME_ROLE}"
            )],
        )]);
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
        let truncated = summaries.len() > max_containers;
        summaries.truncate(max_containers);
        let mut observations = Vec::with_capacity(summaries.len());
        for summary in summaries {
            let container_id = summary.id.unwrap_or_default();
            let labels = summary.labels.unwrap_or_default();
            let identity = migration_identity_from_labels(&labels);
            let (identity, validation_error) = match identity {
                Ok(identity)
                    if validate_migration_label_token(
                        "migration container_id",
                        &container_id,
                        128,
                    )
                    .is_ok() =>
                {
                    (Some(identity), String::new())
                }
                Ok(_) => (
                    None,
                    "migration container has an invalid Docker container ID".to_string(),
                ),
                Err(error) => (None, bounded_drift_reason(&error.to_string())),
            };
            observations.push(MigrationContainerObservationV1 {
                container_id,
                observed_state: migration_summary_state(summary.state.as_ref()),
                identity,
                validation_error,
            });
        }
        Ok(MigrationContainerInventoryV1 {
            inventory_complete: !truncated,
            inventory_error: if truncated {
                format!(
                    "migration container inventory exceeds the bounded limit of {max_containers}"
                )
            } else {
                String::new()
            },
            containers: observations,
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

fn migration_identity_from_labels(
    labels: &HashMap<String, String>,
) -> Result<MigrationContainerIdentityV1, RuntimeError> {
    let required = |name: &str| {
        labels
            .get(name)
            .filter(|value| !value.trim().is_empty())
            .cloned()
            .ok_or_else(|| {
                RuntimeError::InvalidRuntimeContext(format!(
                    "migration container is missing required label {name}"
                ))
            })
    };
    if required(MIGRATION_RUNTIME_ROLE_LABEL)? != MIGRATION_RUNTIME_ROLE {
        return Err(RuntimeError::InvalidRuntimeContext(
            "container runtime role is not migration".to_string(),
        ));
    }
    if required(MIGRATION_MANAGED_BY_LABEL)? != MIGRATION_MANAGED_BY {
        return Err(RuntimeError::InvalidRuntimeContext(
            "migration container is not owned by orchestrator-agent".to_string(),
        ));
    }
    let identity = MigrationContainerIdentityV1 {
        job_id: required(MIGRATION_JOB_ID_LABEL)?,
        service_name: required(MIGRATION_SERVICE_LABEL)?,
        version: required(MIGRATION_VERSION_LABEL)?,
        checksum: required(MIGRATION_CHECKSUM_LABEL)?,
        image: required("ojos.artifact_digest")?,
        resource_claims_sha256: required(MIGRATION_RESOURCE_CLAIMS_LABEL)?,
        identity_sha256: required(MIGRATION_IDENTITY_LABEL)?,
    };
    identity.validate()?;
    Ok(identity)
}

fn migration_summary_state(state: Option<&ContainerSummaryStateEnum>) -> MigrationContainerStateV1 {
    match state.map(AsRef::as_ref) {
        Some("created") => MigrationContainerStateV1::Created,
        Some("running") => MigrationContainerStateV1::Running,
        Some("paused") => MigrationContainerStateV1::Paused,
        Some("restarting") => MigrationContainerStateV1::Restarting,
        Some("exited") | Some("dead") => MigrationContainerStateV1::Exited,
        Some("removing") => MigrationContainerStateV1::Stopped,
        Some(_) | None => MigrationContainerStateV1::Unknown,
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
        let resource_secret_mounts_sha256 = labels
            .and_then(|labels| labels.get(RUNTIME_RESOURCE_SECRET_MOUNTS_SHA256_LABEL))
            .map(String::as_str);
        let require_v3_security = labels
            .and_then(|labels| labels.get(SERVICE_CONTRACT_GENERATION_LABEL))
            .is_some_and(|generation| generation == "3");
        let (retained_volume, retained_volume_sha256) =
            retained_volume_attachment_from_labels(labels, &service_id)?;
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
                &StandardManagedContextAttestation {
                    contract: &runtime_contract,
                    runtime_policy_sha256: &runtime_policy_sha256,
                    claimed_effective_sha256: &effective_runtime_sha256,
                    claimed_resource_secret_mounts_sha256: resource_secret_mounts_sha256,
                    retained_volume: retained_volume.as_ref(),
                    claimed_retained_volume_sha256: retained_volume_sha256.as_deref(),
                    service_id: &service_id,
                    require_v3_security,
                },
            )?;
        } else {
            attest_standard_resource_secret_mounts(
                &inspected,
                resource_secret_mounts_sha256,
                retained_volume.as_ref(),
                retained_volume_sha256.as_deref(),
                &service_id,
                require_v3_security,
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

fn effective_runtime_sha256(spec: &ContainerSpec) -> Result<String, RuntimeError> {
    spec.runtime_contract.validate()?;
    let context_bytes = if let Some(context) = spec.runtime_context.as_ref() {
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
    effective_runtime_sha256_from_bytes(&context_bytes, retained_volume_sha256(spec)?.as_deref())
}

fn effective_runtime_sha256_from_bytes(
    context_bytes: &[u8],
    retained_volume_sha256: Option<&str>,
) -> Result<String, RuntimeError> {
    let Some(retained_volume_sha256) = retained_volume_sha256 else {
        return Ok(format!("sha256:{:x}", Sha256::digest(context_bytes)));
    };
    validate_sha256_text("retained_volume_sha256", retained_volume_sha256)?;
    let mut hasher = Sha256::new();
    hasher.update((context_bytes.len() as u64).to_be_bytes());
    hasher.update(context_bytes);
    hasher.update((retained_volume_sha256.len() as u64).to_be_bytes());
    hasher.update(retained_volume_sha256.as_bytes());
    Ok(format!("sha256:{:x}", hasher.finalize()))
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

fn resource_secret_file_mount(resource: &ResourceSecretFileMount) -> Result<Mount, RuntimeError> {
    resource.validate()?;
    Ok(Mount {
        target: Some(resource.container_destination()?),
        source: Some(resource.host_source_path.clone()),
        typ: Some(MountType::BIND),
        read_only: Some(true),
        bind_options: Some(MountBindOptions {
            propagation: Some(MountBindOptionsPropagationEnum::RPRIVATE),
            non_recursive: Some(true),
            create_mountpoint: Some(false),
            ..Default::default()
        }),
        ..Default::default()
    })
}

fn standard_resource_secret_file_mounts(spec: &ContainerSpec) -> Result<Vec<Mount>, RuntimeError> {
    if spec.resource_secret_file_mounts.is_empty() {
        return Ok(Vec::new());
    }
    if spec.runtime_contract.id != RuntimeProfile::StandardV1 {
        return Err(RuntimeError::InvalidRuntimeContext(
            "resource secret file mounts are restricted to standard-container-v1".to_string(),
        ));
    }
    let mut previous_name: Option<&str> = None;
    let mut mounts = Vec::with_capacity(spec.resource_secret_file_mounts.len());
    for resource in &spec.resource_secret_file_mounts {
        resource.validate()?;
        if previous_name.is_some_and(|previous| previous >= resource.resource_name.as_str()) {
            return Err(RuntimeError::InvalidRuntimeContext(
                "resource secret file mounts must have unique resource names in canonical order"
                    .to_string(),
            ));
        }
        previous_name = Some(resource.resource_name.as_str());
        mounts.push(resource_secret_file_mount(resource)?);
    }
    Ok(mounts)
}

fn retained_volume_mount(spec: &ContainerSpec) -> Result<Option<Mount>, RuntimeError> {
    let Some(attachment) = spec.retained_volume.as_ref() else {
        return Ok(None);
    };
    if spec.runtime_contract.id != RuntimeProfile::StandardV1 {
        return Err(RuntimeError::InvalidRuntimeContext(
            "retained volumes are restricted to standard-container-v1".to_string(),
        ));
    }
    attachment.validate_for_service(&spec.service_id)?;
    Ok(Some(Mount {
        target: Some(attachment.target.clone()),
        source: Some(attachment.docker_name(&spec.service_id)?),
        typ: Some(MountType::VOLUME),
        read_only: Some(false),
        // First attach copies the image-owned target directory into the empty
        // volume, preserving the signed image's non-root uid/gid. Re-attaches
        // do not copy because the volume is no longer empty.
        volume_options: Some(MountVolumeOptions {
            no_copy: Some(false),
            ..Default::default()
        }),
        ..Default::default()
    }))
}

fn retained_volume_sha256(spec: &ContainerSpec) -> Result<Option<String>, RuntimeError> {
    let Some(attachment) = spec.retained_volume.as_ref() else {
        return Ok(None);
    };
    Ok(Some(retained_volume_attachment_sha256(
        attachment,
        &spec.service_id,
    )?))
}

fn retained_volume_attachment_sha256(
    attachment: &RetainedVolumeAttachmentV1,
    service_id: &str,
) -> Result<String, RuntimeError> {
    attachment.validate_for_service(service_id)?;
    let bytes = serde_json::to_vec(attachment).map_err(|error| {
        RuntimeError::InvalidRuntimeContext(format!(
            "cannot encode retained volume attachment: {error}"
        ))
    })?;
    Ok(format!("sha256:{:x}", Sha256::digest(bytes)))
}

fn retained_volume_mount_has_exact_contract(
    mount: &Mount,
    attachment: &RetainedVolumeAttachmentV1,
    service_id: &str,
) -> Result<bool, RuntimeError> {
    Ok(mount.target.as_deref() == Some(attachment.target.as_str())
        && mount.source.as_deref() == Some(attachment.docker_name(service_id)?.as_str())
        && mount.typ == Some(MountType::VOLUME)
        && mount.read_only == Some(false)
        && mount.consistency.is_none()
        && mount.bind_options.is_none()
        && mount.image_options.is_none()
        && mount.tmpfs_options.is_none()
        && mount.volume_options.as_ref().is_some_and(|options| {
            options.no_copy == Some(false)
                && options.labels.is_none()
                && options.driver_config.is_none()
                && options.subpath.is_none()
        }))
}

fn standard_mounts_without_retained_volume<'a>(
    mounts: &'a [Mount],
    attachment: Option<&RetainedVolumeAttachmentV1>,
    service_id: &str,
    claimed_sha256: Option<&str>,
) -> Result<&'a [Mount], RuntimeError> {
    let Some(attachment) = attachment else {
        if claimed_sha256.is_some() {
            return Err(RuntimeError::InvalidRuntimeContext(
                "standard container advertises a retained-volume digest without its typed identity"
                    .to_string(),
            ));
        }
        return Ok(mounts);
    };
    let expected_sha256 = retained_volume_attachment_sha256(attachment, service_id)?;
    if claimed_sha256 != Some(expected_sha256.as_str()) {
        return Err(RuntimeError::InvalidRuntimeContext(
            "standard retained-volume identity digest drifted".to_string(),
        ));
    }
    let (retained, ordinary) = mounts.split_last().ok_or_else(|| {
        RuntimeError::InvalidRuntimeContext(
            "standard retained-volume attachment has no Docker mount".to_string(),
        )
    })?;
    if !retained_volume_mount_has_exact_contract(retained, attachment, service_id)? {
        return Err(RuntimeError::InvalidRuntimeContext(
            "standard retained-volume Docker mount drifted".to_string(),
        ));
    }
    Ok(ordinary)
}

fn retained_volume_attachment_from_labels(
    labels: Option<&HashMap<String, String>>,
    service_id: &str,
) -> Result<(Option<RetainedVolumeAttachmentV1>, Option<String>), RuntimeError> {
    let read = |name: &str| labels.and_then(|labels| labels.get(name)).cloned();
    let values = [
        read(MANAGED_VOLUME_OWNER_INSTANCE_LABEL),
        read(MANAGED_VOLUME_LOGICAL_NAME_LABEL),
        read(MANAGED_VOLUME_TARGET_LABEL),
        read(RUNTIME_RETAINED_VOLUME_ACCESS_LABEL),
        read(MANAGED_VOLUME_LIFECYCLE_LABEL),
        read(RUNTIME_RETAINED_VOLUME_SHA256_LABEL),
    ];
    if values.iter().all(Option::is_none) {
        return Ok((None, None));
    }
    if values.iter().any(Option::is_none) {
        return Err(RuntimeError::InvalidRuntimeContext(
            "standard retained-volume container identity labels are incomplete".to_string(),
        ));
    }
    let attachment = RetainedVolumeAttachmentV1 {
        owner_instance_id: values[0].clone().unwrap_or_default(),
        logical_name: values[1].clone().unwrap_or_default(),
        target: values[2].clone().unwrap_or_default(),
        access: values[3].clone().unwrap_or_default(),
        lifecycle: values[4].clone().unwrap_or_default(),
    };
    attachment.validate_for_service(service_id)?;
    let claimed = values[5].clone().unwrap_or_default();
    validate_sha256_text("retained_volume_sha256", &claimed)?;
    if retained_volume_attachment_sha256(&attachment, service_id)? != claimed {
        return Err(RuntimeError::InvalidRuntimeContext(
            "standard retained-volume container identity digest drifted".to_string(),
        ));
    }
    Ok((Some(attachment), Some(claimed)))
}

fn resource_secret_mounts_sha256(
    resource_secret_file_mounts: &[ResourceSecretFileMount],
) -> Result<Option<String>, RuntimeError> {
    if resource_secret_file_mounts.is_empty() {
        return Ok(None);
    }
    let mut hasher = Sha256::new();
    for resource in resource_secret_file_mounts {
        resource.validate()?;
        for value in [
            resource.resource_name.as_bytes(),
            resource.host_source_path.as_bytes(),
        ] {
            hasher.update((value.len() as u64).to_be_bytes());
            hasher.update(value);
        }
    }
    Ok(Some(format!("sha256:{:x}", hasher.finalize())))
}

fn resource_secret_mount_has_exact_contract(mount: &Mount) -> bool {
    mount.typ == Some(MountType::BIND)
        && mount.read_only == Some(true)
        && mount.consistency.is_none()
        && mount.volume_options.is_none()
        && mount.image_options.is_none()
        && mount.tmpfs_options.is_none()
        && mount.bind_options.as_ref().is_some_and(|options| {
            options.propagation == Some(MountBindOptionsPropagationEnum::RPRIVATE)
                && options.non_recursive == Some(true)
                // Docker omits false-valued bind options from inspect output.
                // Treat absent and explicit false as the same safe state while
                // continuing to reject every option that weakens this mount.
                && options.create_mountpoint != Some(true)
                && options.read_only_non_recursive != Some(true)
                && options.read_only_force_recursive != Some(true)
        })
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

struct StandardManagedContextAttestation<'a> {
    contract: &'a RuntimeContract,
    runtime_policy_sha256: &'a str,
    claimed_effective_sha256: &'a str,
    claimed_resource_secret_mounts_sha256: Option<&'a str>,
    retained_volume: Option<&'a RetainedVolumeAttachmentV1>,
    claimed_retained_volume_sha256: Option<&'a str>,
    service_id: &'a str,
    require_v3_security: bool,
}

fn attest_standard_managed_context_inspection(
    inspected: &bollard::models::ContainerInspectResponse,
    expected: &StandardManagedContextAttestation<'_>,
) -> Result<(), RuntimeError> {
    attest_managed_context_environment(inspected)?;
    let host = inspected.host_config.as_ref().ok_or_else(|| {
        RuntimeError::InvalidRuntimeContext(
            "managed standard container inspection has no HostConfig".to_string(),
        )
    })?;
    if expected.require_v3_security {
        attest_standard_v3_security(inspected, host)?;
    }
    if host.privileged == Some(true)
        || host.cap_add.as_ref().is_some_and(|caps| !caps.is_empty())
        || host.cgroupns_mode == Some(HostConfigCgroupnsModeEnum::HOST)
    {
        return Err(RuntimeError::InvalidRuntimeContext(
            "standard-container-v1 acquired runtime privileges outside its fixed contract"
                .to_string(),
        ));
    }
    let mounts = host.mounts.as_deref().unwrap_or_default();
    let mounts = standard_mounts_without_retained_volume(
        mounts,
        expected.retained_volume,
        expected.service_id,
        expected.claimed_retained_volume_sha256,
    )?;
    if mounts.is_empty() {
        return Err(RuntimeError::InvalidRuntimeContext(format!(
            "managed standard-container-v1 requires its service context mount, found {} mounts",
            mounts.len()
        )));
    }
    let service_context = &mounts[0];
    if service_context.target.as_deref() != Some(MANAGED_SERVICE_CONTEXT_TARGET)
        || service_context.typ != Some(MountType::BIND)
        || service_context.read_only != Some(true)
        || !judge_sandbox_has_exact_bind_options(service_context)
        || service_context.consistency.is_some()
        || service_context.volume_options.is_some()
        || service_context.image_options.is_some()
        || service_context.tmpfs_options.is_some()
    {
        return Err(RuntimeError::InvalidRuntimeContext(
            "managed standard-container-v1 service context mount drifted".to_string(),
        ));
    }
    let mut previous_resource_name: Option<&str> = None;
    let mut inspected_resource_secret_mounts = Vec::with_capacity(mounts.len().saturating_sub(1));
    for resource in &mounts[1..] {
        let target = resource
            .target
            .as_deref()
            .and_then(|target| target.strip_prefix(MANAGED_RESOURCE_SECRET_ROOT))
            .and_then(|target| target.strip_prefix('/'))
            .and_then(|target| target.strip_suffix("/output"))
            .filter(|resource_name| !resource_name.contains('/'))
            .ok_or_else(|| {
                RuntimeError::InvalidRuntimeContext(
                    "managed standard-container-v1 contains an untyped mount target".to_string(),
                )
            })?;
        validate_safe_resource_name(target)?;
        if previous_resource_name.is_some_and(|previous| previous >= target)
            || resource.source.as_deref().is_none_or(|source| {
                source.is_empty()
                    || !Path::new(source).is_absolute()
                    || source.ends_with('/')
                    || source.ends_with('\\')
                    || Path::new(source).components().any(|component| {
                        matches!(
                            component,
                            std::path::Component::ParentDir | std::path::Component::CurDir
                        )
                    })
            })
            || !resource_secret_mount_has_exact_contract(resource)
        {
            return Err(RuntimeError::InvalidRuntimeContext(
                "managed standard-container-v1 resource secret mount drifted".to_string(),
            ));
        }
        previous_resource_name = Some(target);
        inspected_resource_secret_mounts.push(ResourceSecretFileMount {
            resource_name: target.to_string(),
            host_source_path: resource.source.clone().unwrap_or_default(),
        });
    }
    let actual_resource_secret_mounts_sha256 =
        resource_secret_mounts_sha256(&inspected_resource_secret_mounts)?;
    if expected.claimed_resource_secret_mounts_sha256
        != actual_resource_secret_mounts_sha256.as_deref()
    {
        return Err(RuntimeError::InvalidRuntimeContext(
            "managed standard-container-v1 resource secret mount attestation drifted".to_string(),
        ));
    }
    let context = RuntimeContext {
        contract: expected.contract.clone(),
        runtime_policy_sha256: expected.runtime_policy_sha256.to_string(),
        scratch_directory: String::new(),
        cache_volume_name: String::new(),
        service_context_directory: service_context.source.clone().unwrap_or_default(),
    };
    context.validate()?;
    let context_bytes = serde_json::to_vec(&context).map_err(|error| {
        RuntimeError::InvalidRuntimeContext(format!(
            "cannot encode inspected runtime context: {error}"
        ))
    })?;
    let actual = effective_runtime_sha256_from_bytes(
        &context_bytes,
        expected.claimed_retained_volume_sha256,
    )?;
    if expected.claimed_effective_sha256 != actual {
        return Err(RuntimeError::InvalidRuntimeContext(format!(
            "managed standard-container-v1 effective runtime digest drift: claimed {}, inspected {actual}",
            expected.claimed_effective_sha256
        )));
    }
    Ok(())
}

fn attest_standard_resource_secret_mounts(
    inspected: &bollard::models::ContainerInspectResponse,
    claimed_resource_secret_mounts_sha256: Option<&str>,
    retained_volume: Option<&RetainedVolumeAttachmentV1>,
    claimed_retained_volume_sha256: Option<&str>,
    service_id: &str,
    require_v3_security: bool,
) -> Result<(), RuntimeError> {
    let host = inspected.host_config.as_ref().ok_or_else(|| {
        RuntimeError::InvalidRuntimeContext(
            "standard container inspection has no HostConfig".to_string(),
        )
    })?;
    if require_v3_security {
        attest_standard_v3_security(inspected, host)?;
    }
    let mounts = host.mounts.as_deref().unwrap_or_default();
    let mounts = standard_mounts_without_retained_volume(
        mounts,
        retained_volume,
        service_id,
        claimed_retained_volume_sha256,
    )?;
    let mut previous_resource_name: Option<&str> = None;
    let mut inspected_resource_secret_mounts = Vec::with_capacity(mounts.len());
    for resource in mounts {
        let target = resource
            .target
            .as_deref()
            .and_then(|target| target.strip_prefix(MANAGED_RESOURCE_SECRET_ROOT))
            .and_then(|target| target.strip_prefix('/'))
            .and_then(|target| target.strip_suffix("/output"))
            .filter(|resource_name| !resource_name.contains('/'))
            .ok_or_else(|| {
                RuntimeError::InvalidRuntimeContext(
                    "unmanaged standard-container-v1 contains an untyped mount target".to_string(),
                )
            })?;
        validate_safe_resource_name(target)?;
        if previous_resource_name.is_some_and(|previous| previous >= target)
            || resource.source.as_deref().is_none_or(|source| {
                source.is_empty()
                    || !Path::new(source).is_absolute()
                    || source.ends_with('/')
                    || source.ends_with('\\')
                    || Path::new(source).components().any(|component| {
                        matches!(
                            component,
                            std::path::Component::ParentDir | std::path::Component::CurDir
                        )
                    })
            })
            || !resource_secret_mount_has_exact_contract(resource)
        {
            return Err(RuntimeError::InvalidRuntimeContext(
                "standard-container-v1 resource secret mount drifted".to_string(),
            ));
        }
        previous_resource_name = Some(target);
        inspected_resource_secret_mounts.push(ResourceSecretFileMount {
            resource_name: target.to_string(),
            host_source_path: resource.source.clone().unwrap_or_default(),
        });
    }
    let actual_resource_secret_mounts_sha256 =
        resource_secret_mounts_sha256(&inspected_resource_secret_mounts)?;
    if claimed_resource_secret_mounts_sha256 != actual_resource_secret_mounts_sha256.as_deref() {
        return Err(RuntimeError::InvalidRuntimeContext(
            "standard-container-v1 resource secret mount attestation drifted".to_string(),
        ));
    }
    Ok(())
}

fn attest_standard_v3_security(
    inspected: &bollard::models::ContainerInspectResponse,
    host: &HostConfig,
) -> Result<(), RuntimeError> {
    let user = inspected
        .config
        .as_ref()
        .and_then(|config| config.user.as_deref())
        .unwrap_or_default();
    let wrong_user = user != STANDARD_V3_USER;
    if host.readonly_rootfs != Some(true)
        || host.cap_drop.as_deref().is_none_or(|caps| caps != ["ALL"])
        || host
            .security_opt
            .as_deref()
            .is_none_or(|options| options != [STANDARD_V3_NO_NEW_PRIVILEGES])
        || host.pids_limit != Some(STANDARD_V3_PIDS_LIMIT)
        || host.memory != Some(STANDARD_V3_MEMORY_BYTES)
        || host.memory_swap != Some(STANDARD_V3_MEMORY_BYTES)
        || host.init != Some(true)
        || wrong_user
        || host.privileged == Some(true)
        || host.cap_add.as_ref().is_some_and(|caps| !caps.is_empty())
        || host.cgroupns_mode == Some(HostConfigCgroupnsModeEnum::HOST)
        || host.network_mode.as_deref() == Some("host")
        || host.pid_mode.as_deref() == Some("host")
        || host.ipc_mode.as_deref() == Some("host")
        || host.userns_mode.as_deref() == Some("host")
        || host.binds.as_ref().is_some_and(|binds| !binds.is_empty())
        || host
            .devices
            .as_ref()
            .is_some_and(|devices| !devices.is_empty())
        || host.log_config.as_ref().is_none_or(|logging| {
            logging.typ.as_deref() != Some("local")
                || logging.config.as_ref().is_none_or(|config| {
                    config.len() != 2
                        || config.get("max-size").map(String::as_str)
                            != Some(STANDARD_V3_LOG_MAX_SIZE)
                        || config.get("max-file").map(String::as_str)
                            != Some(STANDARD_V3_LOG_MAX_FILES)
                })
        })
        || host.tmpfs.as_ref().is_none_or(|tmpfs| {
            tmpfs.len() != 1
                || tmpfs.get("/tmp").map(String::as_str)
                    != Some("rw,noexec,nosuid,nodev,size=67108864,mode=1777")
        })
    {
        return Err(RuntimeError::InvalidRuntimeContext(
            "standard-container-v1 signed workload Docker security baseline drifted".to_string(),
        ));
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
    let managed_names = [
        "OJOS_WORKLOAD_PUBLIC_KEY_FILE",
        "OJOS_WORKLOAD_KEY_ID",
        "OJOS_WORKLOAD_ISSUER",
        "OJOS_WORKLOAD_AUDIENCE",
    ];
    let exact_values = managed_names
        .iter()
        .map(|name| {
            environment
                .iter()
                .filter_map(|entry| entry.strip_prefix(&format!("{name}=")))
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    let claimed_verifier_environment_sha256 = inspected
        .config
        .as_ref()
        .and_then(|config| config.labels.as_ref())
        .and_then(|labels| labels.get(WORKLOAD_VERIFIER_ENV_SHA256_LABEL))
        .map(String::as_str);
    match claimed_verifier_environment_sha256 {
        Some(claimed) => {
            validate_sha256_text(WORKLOAD_VERIFIER_ENV_SHA256_LABEL, claimed)?;
            if exact_values.iter().any(|values| values.len() != 1)
                || exact_values[0][0] != MANAGED_WORKLOAD_PUBLIC_KEY_FILE
            {
                return Err(RuntimeError::InvalidRuntimeContext(
                    "managed workload verifier environment is incomplete or duplicated".to_string(),
                ));
            }
            let actual = format!(
                "sha256:{:x}",
                Sha256::digest(
                    serde_json::to_vec(&[
                        exact_values[1][0],
                        exact_values[2][0],
                        exact_values[3][0],
                    ])
                    .map_err(|error| RuntimeError::InvalidRuntimeContext(
                        format!("cannot attest workload verifier environment: {error}")
                    ))?
                )
            );
            if actual != claimed {
                return Err(RuntimeError::InvalidRuntimeContext(
                    "managed workload verifier environment attestation drifted".to_string(),
                ));
            }
        }
        None if exact_values.iter().any(|values| !values.is_empty()) => {
            return Err(RuntimeError::InvalidRuntimeContext(
                "managed workload without a verifier retained workload verifier environment"
                    .to_string(),
            ));
        }
        None => {}
    }
    Ok(())
}

fn force_managed_context_environment(
    environment: &mut Vec<String>,
    managed: &ManagedServiceContextSpec,
) -> Result<Option<String>, RuntimeError> {
    const MANAGED_NAMES: &[&str] = &[
        "OJOS_MANAGED_WORKLOAD",
        "OJOS_SERVICE_CONTEXT_FILE",
        "OJOS_WORKLOAD_PUBLIC_KEY_FILE",
        "OJOS_WORKLOAD_KEY_ID",
        "OJOS_WORKLOAD_ISSUER",
        "OJOS_WORKLOAD_AUDIENCE",
    ];
    environment.retain(|value| {
        !MANAGED_NAMES
            .iter()
            .any(|name| value.starts_with(&format!("{name}=")))
    });
    environment.extend([
        "OJOS_MANAGED_WORKLOAD=true".to_string(),
        format!("OJOS_SERVICE_CONTEXT_FILE={MANAGED_SERVICE_CONTEXT_FILE}"),
    ]);
    let Some(verifier) = managed.workload_verifier.as_ref() else {
        return Ok(None);
    };
    let digest = verifier.environment_sha256()?;
    environment.extend([
        format!("OJOS_WORKLOAD_PUBLIC_KEY_FILE={MANAGED_WORKLOAD_PUBLIC_KEY_FILE}"),
        format!("OJOS_WORKLOAD_KEY_ID={}", verifier.key_id),
        format!("OJOS_WORKLOAD_ISSUER={}", verifier.issuer),
        format!("OJOS_WORKLOAD_AUDIENCE={}", verifier.audience),
    ]);
    Ok(Some(digest))
}

fn container_create_body(spec: &ContainerSpec) -> Result<ContainerCreateBody, RuntimeError> {
    spec.runtime_contract.validate()?;
    let resource_secret_mounts = standard_resource_secret_file_mounts(spec)?;
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
    if let Some(attachment) = spec.retained_volume.as_ref() {
        attachment.validate_for_service(&spec.service_id)?;
        labels.insert(
            MANAGED_VOLUME_OWNER_INSTANCE_LABEL.to_string(),
            attachment.owner_instance_id.clone(),
        );
        labels.insert(
            MANAGED_VOLUME_LOGICAL_NAME_LABEL.to_string(),
            attachment.logical_name.clone(),
        );
        labels.insert(
            MANAGED_VOLUME_TARGET_LABEL.to_string(),
            attachment.target.clone(),
        );
        labels.insert(
            RUNTIME_RETAINED_VOLUME_ACCESS_LABEL.to_string(),
            attachment.access.clone(),
        );
        labels.insert(
            MANAGED_VOLUME_LIFECYCLE_LABEL.to_string(),
            attachment.lifecycle.clone(),
        );
        labels.insert(
            RUNTIME_RETAINED_VOLUME_SHA256_LABEL.to_string(),
            retained_volume_attachment_sha256(attachment, &spec.service_id)?,
        );
    }
    if let Some(resource_secret_mounts_sha256) =
        resource_secret_mounts_sha256(&spec.resource_secret_file_mounts)?
    {
        labels.insert(
            RUNTIME_RESOURCE_SECRET_MOUNTS_SHA256_LABEL.to_string(),
            resource_secret_mounts_sha256,
        );
    }
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
    let retained_volume_mount = retained_volume_mount(spec)?;
    let user = match spec.runtime_contract.id {
        RuntimeProfile::StandardV1 => {
            let signed_service_contract = spec
                .labels
                .get("ojos.catalog_signature_verified")
                .is_some_and(|value| value == "true")
                && spec
                    .labels
                    .get(SERVICE_CONTRACT_GENERATION_LABEL)
                    .is_some_and(|generation| generation == "3");
            match (
                spec.managed_service_context.as_ref(),
                spec.runtime_context.as_ref(),
            ) {
                (None, None) => {}
                (Some(managed), Some(_)) => {
                    let context = validate_managed_runtime_context(spec)?;
                    if let Some(digest) =
                        force_managed_context_environment(&mut environment, managed)?
                    {
                        labels.insert(WORKLOAD_VERIFIER_ENV_SHA256_LABEL.to_string(), digest);
                    } else {
                        labels.remove(WORKLOAD_VERIFIER_ENV_SHA256_LABEL);
                    }
                    let config = host_config.get_or_insert_with(HostConfig::default);
                    let mut mounts = vec![managed_service_context_mount(context)];
                    mounts.extend(resource_secret_mounts.iter().cloned());
                    mounts.extend(retained_volume_mount.iter().cloned());
                    config.mounts = Some(mounts);
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
            if spec.managed_service_context.is_none()
                && (!resource_secret_mounts.is_empty() || retained_volume_mount.is_some())
            {
                let config = host_config.get_or_insert_with(HostConfig::default);
                let mut mounts = resource_secret_mounts;
                mounts.extend(retained_volume_mount);
                config.mounts = Some(mounts);
            }
            if signed_service_contract {
                let config = host_config.get_or_insert_with(HostConfig::default);
                config.readonly_rootfs = Some(true);
                config.cap_drop = Some(vec!["ALL".to_string()]);
                config.security_opt = Some(vec![STANDARD_V3_NO_NEW_PRIVILEGES.to_string()]);
                config.pids_limit = Some(STANDARD_V3_PIDS_LIMIT);
                config.memory = Some(STANDARD_V3_MEMORY_BYTES);
                config.memory_swap = Some(STANDARD_V3_MEMORY_BYTES);
                config.init = Some(true);
                config.log_config = Some(HostConfigLogConfig {
                    typ: Some("local".to_string()),
                    config: Some(HashMap::from([
                        ("max-size".to_string(), STANDARD_V3_LOG_MAX_SIZE.to_string()),
                        (
                            "max-file".to_string(),
                            STANDARD_V3_LOG_MAX_FILES.to_string(),
                        ),
                    ])),
                });
                config.tmpfs = Some(HashMap::from([(
                    "/tmp".to_string(),
                    "rw,noexec,nosuid,nodev,size=67108864,mode=1777".to_string(),
                )]));
            }
            signed_service_contract.then(|| STANDARD_V3_USER.to_string())
        }
        RuntimeProfile::JudgeSandboxV1 => {
            let context = validate_judge_sandbox_spec(spec)?;
            environment.retain(|value| !value.starts_with("OJOS_CGROUP_V2_ROOT="));
            environment.push("OJOS_CGROUP_V2_ROOT=/sys/fs/cgroup".to_string());
            environment.retain(|value| !value.starts_with("OJOS_ALLOW_CGROUP_FALLBACK="));
            environment.push("OJOS_ALLOW_CGROUP_FALLBACK=false".to_string());
            environment.retain(|value| !value.starts_with("OJOS_NSJAIL_NO_PIVOTROOT="));
            environment.push("OJOS_NSJAIL_NO_PIVOTROOT=false".to_string());
            let managed = spec.managed_service_context.as_ref().ok_or_else(|| {
                RuntimeError::InvalidRuntimeContext(
                    "judge-sandbox-v1 requires managed_service_context".to_string(),
                )
            })?;
            if let Some(digest) = force_managed_context_environment(&mut environment, managed)? {
                labels.insert(WORKLOAD_VERIFIER_ENV_SHA256_LABEL.to_string(), digest);
            } else {
                labels.remove(WORKLOAD_VERIFIER_ENV_SHA256_LABEL);
            }
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
