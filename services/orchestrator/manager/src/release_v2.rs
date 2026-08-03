//! Store release installation contracts and lifecycle invariants.

use crate::catalog_v2::{OciImageReference, Sha256Digest};
use semver::Version;
use serde::{Deserialize, Serialize};
use std::fmt;
use thiserror::Error;

fn default_start() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StoreInstallRequestV2 {
    pub service_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<Version>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_node_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub endpoint: Option<String>,
    #[serde(default)]
    pub mode: InstallModeV2,
    #[serde(default = "default_start")]
    pub start: bool,
    #[serde(default)]
    pub migration_policy: MigrationPolicyV2,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gateway_node_id: Option<String>,
}

impl Default for StoreInstallRequestV2 {
    fn default() -> Self {
        Self {
            service_id: String::new(),
            version: None,
            target_node_id: None,
            endpoint: None,
            mode: InstallModeV2::Managed,
            start: true,
            migration_policy: MigrationPolicyV2::Apply,
            gateway_node_id: None,
        }
    }
}

impl StoreInstallRequestV2 {
    pub fn managed(service_id: impl Into<String>, target_node_id: impl Into<String>) -> Self {
        Self {
            service_id: service_id.into(),
            target_node_id: Some(target_node_id.into()),
            ..Self::default()
        }
    }

    pub fn external(service_id: impl Into<String>, endpoint: impl Into<String>) -> Self {
        Self {
            service_id: service_id.into(),
            endpoint: Some(endpoint.into()),
            mode: InstallModeV2::External,
            ..Self::default()
        }
    }

    pub fn validate(&self) -> Result<(), ReleaseV2Error> {
        validate_identifier("service_id", &self.service_id)?;
        validate_optional_identifier("target_node_id", self.target_node_id.as_deref())?;
        validate_optional_identifier("gateway_node_id", self.gateway_node_id.as_deref())?;
        match self.mode {
            InstallModeV2::Managed => {
                if self.target_node_id.is_none() {
                    return Err(ReleaseV2Error::TargetNodeRequired);
                }
            }
            InstallModeV2::External => {
                if self.target_node_id.is_some() {
                    return Err(ReleaseV2Error::ExternalTargetNodeForbidden);
                }
                let endpoint = self
                    .endpoint
                    .as_deref()
                    .ok_or(ReleaseV2Error::ExternalEndpointRequired)?;
                validate_endpoint(endpoint)?;
            }
        }
        if let Some(endpoint) = self.endpoint.as_deref() {
            validate_endpoint(endpoint)?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
pub enum InstallModeV2 {
    #[default]
    #[serde(rename = "Managed", alias = "managed")]
    Managed,
    #[serde(rename = "External", alias = "external")]
    External,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
pub enum MigrationPolicyV2 {
    #[default]
    #[serde(rename = "Apply", alias = "apply")]
    Apply,
    #[serde(rename = "DryRun", alias = "dry-run", alias = "dry_run")]
    DryRun,
    #[serde(rename = "Skip", alias = "skip")]
    Skip,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "PascalCase")]
pub enum ReleaseStateV2 {
    #[default]
    Imported,
    Deploying,
    Running,
    Stopped,
    Failed,
}

impl ReleaseStateV2 {
    pub fn can_transition_to(self, next: Self) -> bool {
        if self == next {
            return true;
        }
        matches!(
            (self, next),
            (Self::Imported, Self::Deploying | Self::Failed)
                | (
                    Self::Deploying,
                    Self::Running | Self::Stopped | Self::Failed
                )
                | (
                    Self::Running,
                    Self::Deploying | Self::Stopped | Self::Failed
                )
                | (
                    Self::Stopped,
                    Self::Deploying | Self::Running | Self::Failed
                )
                | (Self::Failed, Self::Deploying)
        )
    }

    pub fn transition_to(&mut self, next: Self) -> Result<(), ReleaseV2Error> {
        if !self.can_transition_to(next) {
            return Err(ReleaseV2Error::InvalidStateTransition {
                from: *self,
                to: next,
            });
        }
        *self = next;
        Ok(())
    }
}

impl fmt::Display for ReleaseStateV2 {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Imported => "Imported",
            Self::Deploying => "Deploying",
            Self::Running => "Running",
            Self::Stopped => "Stopped",
            Self::Failed => "Failed",
        })
    }
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "PascalCase")]
pub enum RuntimeDesiredStateV2 {
    #[default]
    Running,
    Stopped,
    Removed,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "PascalCase")]
pub enum RuntimeHealthV2 {
    #[default]
    Unknown,
    Starting,
    Healthy,
    Unhealthy,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RuntimeInstanceV2 {
    pub deployment_id: String,
    pub service_id: String,
    pub version: Version,
    pub target_node_id: String,
    pub expected_image: OciImageReference,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub container_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observed_repo_digest: Option<Sha256Digest>,
    #[serde(default)]
    pub desired_state: RuntimeDesiredStateV2,
    #[serde(default)]
    pub state: ReleaseStateV2,
    #[serde(default)]
    pub health: RuntimeHealthV2,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
}

impl RuntimeInstanceV2 {
    pub fn validate(&self) -> Result<(), ReleaseV2Error> {
        validate_identifier("deployment_id", &self.deployment_id)?;
        validate_identifier("service_id", &self.service_id)?;
        validate_identifier("target_node_id", &self.target_node_id)?;
        if let Some(container_id) = self.container_id.as_deref()
            && (container_id.len() < 12
                || container_id.len() > 64
                || !container_id
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)))
        {
            return Err(ReleaseV2Error::InvalidContainerId(container_id.to_string()));
        }
        if self.state == ReleaseStateV2::Running && self.container_id.is_none() {
            return Err(ReleaseV2Error::RunningContainerRequired);
        }
        if let Some(observed) = self.observed_repo_digest.as_ref()
            && observed != self.expected_image.digest()
        {
            return Err(ReleaseV2Error::ObservedDigestMismatch {
                expected: self.expected_image.digest().clone(),
                observed: observed.clone(),
            });
        }
        if self
            .last_error
            .as_ref()
            .is_some_and(|value| value.len() > 4096)
        {
            return Err(ReleaseV2Error::LastErrorTooLong);
        }
        Ok(())
    }

    pub fn observed_image_is_expected(&self) -> bool {
        self.observed_repo_digest
            .as_ref()
            .is_some_and(|digest| digest == self.expected_image.digest())
    }
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum ReleaseV2Error {
    #[error("invalid {field}: {reason}")]
    InvalidField { field: &'static str, reason: String },
    #[error("managed installs require target_node_id")]
    TargetNodeRequired,
    #[error("external installs use endpoint and must not set target_node_id")]
    ExternalTargetNodeForbidden,
    #[error("external installs require endpoint")]
    ExternalEndpointRequired,
    #[error("invalid external endpoint {0}")]
    InvalidEndpoint(String),
    #[error("invalid release state transition {from} -> {to}")]
    InvalidStateTransition {
        from: ReleaseStateV2,
        to: ReleaseStateV2,
    },
    #[error("invalid Docker container id {0}")]
    InvalidContainerId(String),
    #[error("a Running runtime instance must have a container id")]
    RunningContainerRequired,
    #[error("runtime image digest mismatch: expected {expected}, observed {observed}")]
    ObservedDigestMismatch {
        expected: Sha256Digest,
        observed: Sha256Digest,
    },
    #[error("last_error exceeds 4096 bytes")]
    LastErrorTooLong,
}

fn validate_optional_identifier(
    field: &'static str,
    value: Option<&str>,
) -> Result<(), ReleaseV2Error> {
    if let Some(value) = value {
        validate_identifier(field, value)?;
    }
    Ok(())
}

fn validate_identifier(field: &'static str, value: &str) -> Result<(), ReleaseV2Error> {
    if value.is_empty()
        || value.len() > 128
        || !value
            .bytes()
            .next()
            .is_some_and(|byte| byte.is_ascii_alphanumeric())
        || !value
            .bytes()
            .last()
            .is_some_and(|byte| byte.is_ascii_alphanumeric())
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b':'))
    {
        return Err(ReleaseV2Error::InvalidField {
            field,
            reason:
                "must be a non-empty identifier using ASCII letters, digits, '.', '_', '-' or ':'"
                    .to_string(),
        });
    }
    Ok(())
}

fn validate_endpoint(endpoint: &str) -> Result<(), ReleaseV2Error> {
    let common_valid = endpoint.len() <= 2048
        && endpoint.trim() == endpoint
        && !endpoint.chars().any(char::is_control)
        && !endpoint.chars().any(char::is_whitespace);
    let uri_valid = endpoint
        .split_once("://")
        .is_some_and(|(scheme, authority)| {
            !scheme.is_empty()
                && !authority.is_empty()
                && scheme.bytes().enumerate().all(|(index, byte)| {
                    byte.is_ascii_lowercase()
                        || (!index.eq(&0)
                            && (byte.is_ascii_digit() || matches!(byte, b'+' | b'-' | b'.')))
                })
        });
    let orchestrator_endpoint_valid = orchestrator_legacy::validate_endpoint_id(endpoint).is_ok();
    if common_valid && (uri_valid || orchestrator_endpoint_valid) {
        Ok(())
    } else {
        Err(ReleaseV2Error::InvalidEndpoint(endpoint.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SHA: &str = "sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    #[test]
    fn serde_and_rust_defaults_install_and_start_managed_releases() {
        let rust = StoreInstallRequestV2::default();
        assert_eq!(rust.mode, InstallModeV2::Managed);
        assert!(rust.start);
        assert_eq!(rust.migration_policy, MigrationPolicyV2::Apply);

        let decoded: StoreInstallRequestV2 =
            serde_json::from_str(r#"{"service_id":"api","target_node_id":"node-1"}"#).unwrap();
        assert_eq!(decoded.mode, InstallModeV2::Managed);
        assert!(decoded.start);
        assert_eq!(decoded.migration_policy, MigrationPolicyV2::Apply);
        decoded.validate().unwrap();
    }

    #[test]
    fn managed_and_external_install_requirements_are_unambiguous() {
        assert!(
            StoreInstallRequestV2::managed("api", "node-1")
                .validate()
                .is_ok()
        );
        assert!(
            StoreInstallRequestV2::external("postgresql", "postgres://db:5432/oj")
                .validate()
                .is_ok()
        );

        let managed_without_node = StoreInstallRequestV2 {
            service_id: "api".to_string(),
            ..StoreInstallRequestV2::default()
        };
        assert_eq!(
            managed_without_node.validate(),
            Err(ReleaseV2Error::TargetNodeRequired)
        );

        let external_without_endpoint = StoreInstallRequestV2 {
            service_id: "postgresql".to_string(),
            mode: InstallModeV2::External,
            ..StoreInstallRequestV2::default()
        };
        assert_eq!(
            external_without_endpoint.validate(),
            Err(ReleaseV2Error::ExternalEndpointRequired)
        );
    }

    #[test]
    fn lifecycle_rejects_impossible_state_jumps() {
        let mut state = ReleaseStateV2::Imported;
        assert_eq!(
            state.transition_to(ReleaseStateV2::Running),
            Err(ReleaseV2Error::InvalidStateTransition {
                from: ReleaseStateV2::Imported,
                to: ReleaseStateV2::Running,
            })
        );
        state.transition_to(ReleaseStateV2::Deploying).unwrap();
        state.transition_to(ReleaseStateV2::Running).unwrap();
        state.transition_to(ReleaseStateV2::Stopped).unwrap();
        assert_eq!(state, ReleaseStateV2::Stopped);
    }

    #[test]
    fn runtime_instance_binds_observed_digest_to_the_requested_image() {
        let instance = RuntimeInstanceV2 {
            deployment_id: "deployment-1".to_string(),
            service_id: "api".to_string(),
            version: Version::parse("1.0.0").unwrap(),
            target_node_id: "node-1".to_string(),
            expected_image: format!("registry.example/ojos/api@{SHA}").parse().unwrap(),
            container_id: Some("0123456789abcdef".to_string()),
            observed_repo_digest: Some(SHA.parse().unwrap()),
            desired_state: RuntimeDesiredStateV2::Running,
            state: ReleaseStateV2::Running,
            health: RuntimeHealthV2::Healthy,
            last_error: None,
        };
        instance.validate().unwrap();
        assert!(instance.observed_image_is_expected());

        let mut mismatched = instance.clone();
        mismatched.observed_repo_digest = Some(
            "sha256:ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff"
                .parse()
                .unwrap(),
        );
        assert!(matches!(
            mismatched.validate(),
            Err(ReleaseV2Error::ObservedDigestMismatch { .. })
        ));
    }

    #[test]
    fn runtime_running_state_requires_a_real_container() {
        let instance = RuntimeInstanceV2 {
            deployment_id: "deployment-1".to_string(),
            service_id: "api".to_string(),
            version: Version::parse("1.0.0").unwrap(),
            target_node_id: "node-1".to_string(),
            expected_image: format!("registry.example/ojos/api@{SHA}").parse().unwrap(),
            container_id: None,
            observed_repo_digest: None,
            desired_state: RuntimeDesiredStateV2::Running,
            state: ReleaseStateV2::Running,
            health: RuntimeHealthV2::Starting,
            last_error: None,
        };
        assert_eq!(
            instance.validate(),
            Err(ReleaseV2Error::RunningContainerRequired)
        );
    }
}
