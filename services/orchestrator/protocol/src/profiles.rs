//! Closed profile identities and deterministic health-policy validation.

use crate::RuntimeError;
use serde::{Deserialize, Serialize};
use std::fmt::{Display, Formatter};

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
pub const JUDGE_SANDBOX_V1_HEALTH_TIMEOUT_MS: u64 = 120_000;
pub const JUDGE_SANDBOX_V1_HEALTH_POLL_INTERVAL_MS: u64 = 2_000;
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

const fn default_health_timeout_ms() -> u64 {
    DEFAULT_HEALTH_TIMEOUT_MS
}

const fn default_health_poll_interval_ms() -> u64 {
    DEFAULT_HEALTH_POLL_INTERVAL_MS
}

const fn default_compensation_timeout_ms() -> u64 {
    DEFAULT_COMPENSATION_TIMEOUT_MS
}
