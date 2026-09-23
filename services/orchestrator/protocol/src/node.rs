//! Versioned runtime observations. Reports never contain workload credentials.

use crate::RuntimeContract;
use serde::{Deserialize, Serialize};

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

/// The versioned capability report accepted by the control plane.
/// It deliberately advertises only closed runtime contracts already accepted
/// by both local policy and observed Docker facts.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct NodeRuntimeFactsV1 {
    pub schema_version: u32,
    #[serde(default)]
    pub report_id: String,
    /// Agent-clock lower bound for this inventory snapshot. It is captured
    /// before Docker enumeration starts, so a lifecycle completion carrying a
    /// watermark at or after this value is causally newer and cannot be
    /// overwritten by this report when its container is absent.
    pub observed_at_ms: i64,
    pub agent_version: String,
    pub runtime_policy_sha256: String,
    pub allowed_contracts: Vec<RuntimeContract>,
    #[serde(default)]
    pub judge_sandbox_allowed_images: Vec<String>,
    /// Agent-local Redis connection identifiers safe to publish. URLs and
    /// credentials remain only in the protected Agent configuration.
    #[serde(default)]
    pub redis_connection_ids: Vec<String>,
    pub docker: DockerRuntimeFacts,
    #[serde(default)]
    pub inventory_complete: bool,
    #[serde(default)]
    pub inventory_error: String,
    #[serde(default)]
    pub deployment_observations: Vec<DeploymentRuntimeObservationV1>,
    #[serde(default)]
    pub credential_statuses: Vec<CredentialRefreshStatus>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CredentialRefreshStatus {
    pub deployment_id: String,
    pub expires_at_ms: i64,
    pub last_success_at_ms: i64,
    pub last_error: String,
}
