//! Deterministic volumes contracts; no environment reads or I/O.

use super::OciImageReference;
use crate::{RuntimeContract, RuntimeError, RuntimeProfile};
use regex::Regex;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashMap;

pub const JUDGE_CACHE_VOLUME_LOGICAL_NAME: &str = "artifact-cache";

pub const RELEASE_VOLUME_LIFECYCLE: &str = "release";

pub const RETAIN_VOLUME_LIFECYCLE: &str = "retain";

pub const MANAGED_VOLUME_OWNER_LABEL: &str = "ojos.managed_by";

pub const MANAGED_VOLUME_OWNER: &str = "orchestrator-agent";

pub const MANAGED_VOLUME_DEPLOYMENT_LABEL: &str = "ojos.deployment_id";

pub const MANAGED_VOLUME_SERVICE_LABEL: &str = "ojos.service_id";

pub const MANAGED_VOLUME_ARTIFACT_LABEL: &str = "ojos.artifact_digest";

pub const MANAGED_VOLUME_PROFILE_LABEL: &str = "ojos.runtime_profile_sha256";

pub const MANAGED_VOLUME_LOGICAL_NAME_LABEL: &str = "ojos.volume_logical_name";

pub const MANAGED_VOLUME_LIFECYCLE_LABEL: &str = "ojos.volume_lifecycle";

pub const MANAGED_VOLUME_OWNER_INSTANCE_LABEL: &str = "ojos.owner_instance_id";

pub const MANAGED_VOLUME_TARGET_LABEL: &str = "ojos.volume_target";

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
    #[serde(default)]
    pub owner_instance_id: String,
    #[serde(default)]
    pub target: String,
}

impl ManagedVolumeSpec {
    pub fn validate(&self) -> Result<(), RuntimeError> {
        self.runtime_contract.validate()?;
        let expected_prefix = match self.runtime_contract.id {
            RuntimeProfile::JudgeSandboxV1 => {
                if self.service_id != "judge-worker"
                    || self.logical_name != JUDGE_CACHE_VOLUME_LOGICAL_NAME
                    || self.lifecycle != RELEASE_VOLUME_LIFECYCLE
                    || self.deployment_id.trim().is_empty()
                    || !self.owner_instance_id.is_empty()
                    || (!self.target.is_empty() && self.target != "/var/lib/ojos-worker/cache")
                {
                    return Err(RuntimeError::InvalidRuntimeContext(
                        "managed cache volume must be the fixed release-scoped judge-sandbox-v1 artifact-cache contract"
                            .to_string(),
                    ));
                }
                OciImageReference::parse(&self.artifact_digest)?;
                "ojos-judge-cache-"
            }
            RuntimeProfile::StandardV1 => {
                validate_retained_volume_identity(
                    &self.owner_instance_id,
                    &self.service_id,
                    &self.logical_name,
                    &self.target,
                )?;
                if self.lifecycle != RETAIN_VOLUME_LIFECYCLE
                    || self.deployment_id.trim().is_empty()
                    || self.artifact_digest.is_empty()
                {
                    return Err(RuntimeError::InvalidRuntimeContext(
                        "standard managed volume must be an Agent-derived stable RETAIN attachment"
                            .to_string(),
                    ));
                }
                OciImageReference::parse(&self.artifact_digest)?;
                "ojos-retain-"
            }
        };
        let component = self.name.strip_prefix(expected_prefix).ok_or_else(|| {
            RuntimeError::InvalidRuntimeContext(format!(
                "managed volume name must start with {expected_prefix}"
            ))
        })?;
        if component.len() != 32
            || !component
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err(RuntimeError::InvalidRuntimeContext(
                "managed volume name must end with the Agent-derived 128-bit identity digest"
                    .to_string(),
            ));
        }
        Ok(())
    }

    pub fn ownership_labels(&self) -> Result<HashMap<String, String>, RuntimeError> {
        self.validate()?;
        let mut labels = HashMap::from([
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
        ]);
        if self.runtime_contract.id == RuntimeProfile::StandardV1 {
            labels.remove(MANAGED_VOLUME_DEPLOYMENT_LABEL);
            labels.remove(MANAGED_VOLUME_ARTIFACT_LABEL);
            labels.insert(
                MANAGED_VOLUME_OWNER_INSTANCE_LABEL.to_string(),
                self.owner_instance_id.clone(),
            );
            labels.insert(MANAGED_VOLUME_TARGET_LABEL.to_string(), self.target.clone());
        }
        Ok(labels)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RetainedVolumeAttachmentV1 {
    pub owner_instance_id: String,
    pub logical_name: String,
    pub target: String,
    pub access: String,
    pub lifecycle: String,
}

impl RetainedVolumeAttachmentV1 {
    pub fn validate_for_service(&self, service_id: &str) -> Result<(), RuntimeError> {
        validate_retained_volume_identity(
            &self.owner_instance_id,
            service_id,
            &self.logical_name,
            &self.target,
        )?;
        if self.access != "rw" || self.lifecycle != RETAIN_VOLUME_LIFECYCLE {
            return Err(RuntimeError::InvalidRuntimeContext(
                "retained volume attachment requires access=rw and lifecycle=retain".to_string(),
            ));
        }
        Ok(())
    }

    pub fn docker_name(&self, service_id: &str) -> Result<String, RuntimeError> {
        self.validate_for_service(service_id)?;
        let digest = Sha256::digest(
            format!(
                "{}\0{}\0{}",
                self.owner_instance_id, service_id, self.logical_name
            )
            .as_bytes(),
        );
        Ok(format!("ojos-retain-{}", &format!("{digest:x}")[..32]))
    }
}

fn validate_retained_volume_identity(
    owner_instance_id: &str,
    service_id: &str,
    logical_name: &str,
    target: &str,
) -> Result<(), RuntimeError> {
    let stable = Regex::new(r"^[A-Za-z0-9][A-Za-z0-9._:-]{0,255}$").expect("valid regex");
    let logical = Regex::new(r"^[a-z][a-z0-9]*(?:[._-][a-z0-9]+)*$").expect("valid regex");
    let reserved = target == "/"
        || ["/run/ojos", "/proc", "/sys", "/dev"]
            .iter()
            .any(|prefix| target == *prefix || target.starts_with(&format!("{prefix}/")));
    if !stable.is_match(owner_instance_id)
        || !stable.is_match(service_id)
        || !logical.is_match(logical_name)
        || !target.starts_with('/')
        || (target.len() > 1 && target.ends_with('/'))
        || target.contains("//")
        || target.contains('?')
        || target.contains('#')
        || reserved
    {
        return Err(RuntimeError::InvalidRuntimeContext(
            "retained volume owner, service, logical name, or target is invalid".to_string(),
        ));
    }
    Ok(())
}
