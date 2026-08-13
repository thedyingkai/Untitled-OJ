use regex::Regex;
use semver::{Version, VersionReq};
use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ApiBindingState {
    Pending,
    Resolved,
    Active,
    Unbound,
    Revoked,
    Error,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ApiBindingDesiredState {
    Active,
    Revoked,
}

impl ApiBindingDesiredState {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Active => "ACTIVE",
            Self::Revoked => "REVOKED",
        }
    }
}

impl PartialEq<&str> for ApiBindingDesiredState {
    fn eq(&self, other: &&str) -> bool {
        self.as_str() == *other
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ApiBindingObservedState {
    Pending,
    Resolved,
    Active,
    Revoked,
    Error,
}

impl ApiBindingObservedState {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "PENDING",
            Self::Resolved => "RESOLVED",
            Self::Active => "ACTIVE",
            Self::Revoked => "REVOKED",
            Self::Error => "ERROR",
        }
    }
}

impl PartialEq<&str> for ApiBindingObservedState {
    fn eq(&self, other: &&str) -> bool {
        self.as_str() == *other
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ApiBindingHealth {
    Unknown,
    Healthy,
    Unhealthy,
    Degraded,
}

impl ApiBindingHealth {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Unknown => "UNKNOWN",
            Self::Healthy => "HEALTHY",
            Self::Unhealthy => "UNHEALTHY",
            Self::Degraded => "DEGRADED",
        }
    }
}

impl PartialEq<&str> for ApiBindingHealth {
    fn eq(&self, other: &&str) -> bool {
        self.as_str() == *other
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ApiBinding {
    pub binding_id: String,
    pub requirement_name: String,
    pub api_id: String,
    #[serde(default)]
    pub api_version: String,
    pub consumer_deployment_id: String,
    pub consumer_service_id: String,
    pub consumer_node_id: String,
    #[serde(default)]
    pub consumer_endpoint: String,
    #[serde(default)]
    pub provider_deployment_id: String,
    #[serde(default)]
    pub provider_service_id: String,
    #[serde(default)]
    pub provider_node_id: String,
    #[serde(default)]
    pub provider_endpoint: String,
    /// Provider-native API path from the signed Service Contract.
    #[serde(default)]
    pub provider_path: String,
    #[serde(default)]
    pub virtual_endpoint: String,
    #[serde(default)]
    pub protocol: String,
    #[serde(default)]
    pub methods: Vec<String>,
    #[serde(default)]
    pub auth_mode: String,
    #[serde(default)]
    pub provider_auth_mode: String,
    #[serde(default)]
    pub permission: String,
    #[serde(default)]
    pub timeout_ms: Option<u64>,
    #[serde(default)]
    pub topology_id: String,
    #[serde(default)]
    pub topology_revision_id: String,
    #[serde(default)]
    pub link_source_endpoint: String,
    #[serde(default)]
    pub link_target_endpoint: String,
    #[serde(default)]
    pub credential_ref: String,
    #[serde(default = "default_generation")]
    pub credential_generation: u64,
    #[serde(default = "default_generation")]
    pub context_generation: u64,
    #[serde(default = "default_desired_state")]
    pub desired_state: ApiBindingDesiredState,
    #[serde(default = "default_observed_state")]
    pub observed_state: ApiBindingObservedState,
    #[serde(default = "default_health")]
    pub health: ApiBindingHealth,
    #[serde(default)]
    pub drift: Vec<String>,
    #[serde(default)]
    pub last_operation_id: String,
    pub state: ApiBindingState,
    #[serde(default)]
    pub optional: bool,
    #[serde(default)]
    pub reason: String,
    pub created_at: String,
    pub updated_at: String,
}

const fn default_generation() -> u64 {
    1
}

const fn default_desired_state() -> ApiBindingDesiredState {
    ApiBindingDesiredState::Active
}

const fn default_observed_state() -> ApiBindingObservedState {
    ApiBindingObservedState::Pending
}

const fn default_health() -> ApiBindingHealth {
    ApiBindingHealth::Unknown
}

impl ApiBinding {
    /// Returns the only lifecycle state compatible with the typed desired and
    /// observed facts. Health is deliberately orthogonal: a transiently
    /// unhealthy active binding remains ACTIVE and is handled by routing and
    /// evidence gates instead of silently changing lifecycle ownership.
    pub const fn derived_state(&self) -> ApiBindingState {
        match (self.desired_state, self.observed_state) {
            (ApiBindingDesiredState::Active, ApiBindingObservedState::Pending) => {
                ApiBindingState::Pending
            }
            (ApiBindingDesiredState::Active, ApiBindingObservedState::Resolved) => {
                ApiBindingState::Resolved
            }
            (ApiBindingDesiredState::Active, ApiBindingObservedState::Active) => {
                ApiBindingState::Active
            }
            (ApiBindingDesiredState::Active, ApiBindingObservedState::Revoked) => {
                if self.optional {
                    ApiBindingState::Unbound
                } else {
                    ApiBindingState::Error
                }
            }
            (ApiBindingDesiredState::Active, ApiBindingObservedState::Error)
            | (ApiBindingDesiredState::Revoked, ApiBindingObservedState::Error) => {
                ApiBindingState::Error
            }
            (ApiBindingDesiredState::Revoked, ApiBindingObservedState::Revoked) => {
                ApiBindingState::Revoked
            }
            (ApiBindingDesiredState::Revoked, _) => ApiBindingState::Pending,
        }
    }

    pub fn validate(&self) -> Result<(), ApiBindingValidationError> {
        let stable = Regex::new(r"^[A-Za-z0-9][A-Za-z0-9_.:-]*$").expect("valid regex");
        for (name, value) in [
            ("binding_id", self.binding_id.as_str()),
            ("requirement_name", self.requirement_name.as_str()),
            ("api_id", self.api_id.as_str()),
            (
                "consumer_deployment_id",
                self.consumer_deployment_id.as_str(),
            ),
            ("consumer_service_id", self.consumer_service_id.as_str()),
            ("consumer_node_id", self.consumer_node_id.as_str()),
            ("created_at", self.created_at.as_str()),
            ("updated_at", self.updated_at.as_str()),
        ] {
            if value.trim().is_empty() {
                return Err(ApiBindingValidationError::Invalid(format!(
                    "API binding {name} must not be empty"
                )));
            }
        }
        for (name, value) in [
            ("binding_id", self.binding_id.as_str()),
            ("requirement_name", self.requirement_name.as_str()),
            ("api_id", self.api_id.as_str()),
        ] {
            if !stable.is_match(value) {
                return Err(ApiBindingValidationError::Invalid(format!(
                    "API binding {name} is invalid"
                )));
            }
        }
        if self.credential_generation == 0 || self.context_generation == 0 {
            return Err(ApiBindingValidationError::Invalid(
                "API binding credential_generation and context_generation must be positive"
                    .to_string(),
            ));
        }
        let derived_state = self.derived_state();
        if self.state != derived_state {
            return Err(ApiBindingValidationError::Invalid(format!(
                "API binding compatibility state {:?} disagrees with derived state {:?}",
                self.state, derived_state
            )));
        }
        if matches!(
            self.state,
            ApiBindingState::Resolved | ApiBindingState::Active
        ) {
            for (name, value) in [
                (
                    "provider_deployment_id",
                    self.provider_deployment_id.as_str(),
                ),
                ("provider_service_id", self.provider_service_id.as_str()),
                ("provider_node_id", self.provider_node_id.as_str()),
                ("provider_endpoint", self.provider_endpoint.as_str()),
                ("protocol", self.protocol.as_str()),
                ("auth_mode", self.auth_mode.as_str()),
            ] {
                if value.trim().is_empty() {
                    return Err(ApiBindingValidationError::Invalid(format!(
                        "resolved API binding {name} must not be empty"
                    )));
                }
            }
            if matches!(self.protocol.as_str(), "http" | "https")
                && (!self.provider_path.starts_with('/') || self.methods.is_empty())
            {
                return Err(ApiBindingValidationError::Invalid(
                    "resolved HTTP API binding requires provider_path and methods".to_string(),
                ));
            }
        }
        if self.state == ApiBindingState::Unbound && !self.optional {
            return Err(ApiBindingValidationError::Invalid(
                "only optional API bindings may be UNBOUND".to_string(),
            ));
        }
        if !self.topology_revision_id.is_empty() && self.topology_id.is_empty() {
            return Err(ApiBindingValidationError::Invalid(
                "topology_revision_id requires topology_id".to_string(),
            ));
        }
        if self.link_source_endpoint.is_empty() != self.link_target_endpoint.is_empty() {
            return Err(ApiBindingValidationError::Invalid(
                "link source and target endpoints must be set together".to_string(),
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ApiProviderCandidate {
    pub deployment_id: String,
    pub service_id: String,
    pub node_id: String,
    pub endpoint: String,
    pub path: String,
    pub api_id: String,
    pub api_version: String,
    pub protocol: String,
    #[serde(default)]
    pub methods: Vec<String>,
    pub auth_mode: String,
    pub permission: String,
    #[serde(default)]
    pub healthy: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ApiBindingResolutionRequest {
    pub requirement_name: String,
    pub api_id: String,
    #[serde(default)]
    pub version_requirement: String,
    pub consumer_node_id: String,
    #[serde(default)]
    pub provider_deployment_id: String,
    #[serde(default)]
    pub optional: bool,
    #[serde(default = "default_selection")]
    pub selection: String,
}

fn default_selection() -> String {
    "nearest-healthy".to_string()
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum ApiBindingResolutionError {
    #[error("invalid API binding request: {0}")]
    Invalid(String),
    #[error("required API binding {requirement_name} ({api_id}) has no healthy provider")]
    MissingProvider {
        requirement_name: String,
        api_id: String,
    },
    #[error("API binding {requirement_name} is ambiguous; select provider_deployment_id")]
    Ambiguous { requirement_name: String },
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum ApiBindingValidationError {
    #[error("{0}")]
    Invalid(String),
}

pub fn resolve_api_binding_candidate(
    request: &ApiBindingResolutionRequest,
    candidates: &[ApiProviderCandidate],
) -> Result<Option<ApiProviderCandidate>, ApiBindingResolutionError> {
    validate_resolution_request(request)?;
    let mut matches = candidates
        .iter()
        .filter(|candidate| {
            candidate.healthy
                && candidate.api_id == request.api_id
                && api_version_matches(&request.version_requirement, &candidate.api_version)
                && (request.provider_deployment_id.is_empty()
                    || candidate.deployment_id == request.provider_deployment_id)
                && (request.selection != "same-node"
                    || candidate.node_id == request.consumer_node_id)
        })
        .cloned()
        .collect::<Vec<_>>();
    matches.sort_by(|left, right| {
        let left_remote = left.node_id != request.consumer_node_id;
        let right_remote = right.node_id != request.consumer_node_id;
        left_remote
            .cmp(&right_remote)
            .then_with(|| left.service_id.cmp(&right.service_id))
            .then_with(|| left.deployment_id.cmp(&right.deployment_id))
            .then_with(|| left.endpoint.cmp(&right.endpoint))
    });
    if request.provider_deployment_id.is_empty() && matches.len() > 1 {
        return Err(ApiBindingResolutionError::Ambiguous {
            requirement_name: request.requirement_name.clone(),
        });
    }
    match matches.into_iter().next() {
        Some(candidate) => Ok(Some(candidate)),
        None if request.optional => Ok(None),
        None => Err(ApiBindingResolutionError::MissingProvider {
            requirement_name: request.requirement_name.clone(),
            api_id: request.api_id.clone(),
        }),
    }
}

fn validate_resolution_request(
    request: &ApiBindingResolutionRequest,
) -> Result<(), ApiBindingResolutionError> {
    if request.requirement_name.trim().is_empty()
        || request.api_id.trim().is_empty()
        || request.consumer_node_id.trim().is_empty()
    {
        return Err(ApiBindingResolutionError::Invalid(
            "requirement_name, api_id, and consumer_node_id are required".to_string(),
        ));
    }
    if !matches!(
        request.selection.as_str(),
        "nearest-healthy" | "same-node" | "explicit"
    ) {
        return Err(ApiBindingResolutionError::Invalid(format!(
            "unsupported selection policy {}",
            request.selection
        )));
    }
    Ok(())
}

/// API contracts in the existing repository use both `v1` and semver. Exact
/// legacy tokens remain valid while semver requirements support proper range
/// selection for Contract v2.
pub fn api_version_matches(requirement: &str, candidate: &str) -> bool {
    let requirement = requirement.trim();
    let candidate = candidate.trim();
    if requirement.is_empty() || requirement == "*" {
        return true;
    }
    if requirement.eq_ignore_ascii_case(candidate) {
        return true;
    }
    let Some(candidate) = parse_api_semver(candidate) else {
        return false;
    };
    let normalized_requirement = if let Some(version) = parse_api_semver(requirement) {
        format!("={version}")
    } else {
        requirement.to_string()
    };
    VersionReq::parse(&normalized_requirement)
        .is_ok_and(|requirement| requirement.matches(&candidate))
}

fn parse_api_semver(value: &str) -> Option<Version> {
    let value = value.trim().strip_prefix('v').unwrap_or(value.trim());
    if let Ok(version) = Version::parse(value) {
        return Some(version);
    }
    let dots = value.bytes().filter(|byte| *byte == b'.').count();
    match dots {
        0 => Version::parse(&format!("{value}.0.0")).ok(),
        1 => Version::parse(&format!("{value}.0")).ok(),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn binding() -> ApiBinding {
        ApiBinding {
            binding_id: "binding-1".to_string(),
            requirement_name: "storage_get".to_string(),
            api_id: "storage.object.get".to_string(),
            api_version: "1.0.0".to_string(),
            consumer_deployment_id: "consumer-1".to_string(),
            consumer_service_id: "consumer".to_string(),
            consumer_node_id: "node-a".to_string(),
            consumer_endpoint: "127.0.0.1:9000:consumer".to_string(),
            provider_deployment_id: "storage-1".to_string(),
            provider_service_id: "storage".to_string(),
            provider_node_id: "node-b".to_string(),
            provider_endpoint: "127.0.0.1:9001:storage".to_string(),
            provider_path: "/objects".to_string(),
            virtual_endpoint: "/internal/apis/storage.object.get".to_string(),
            protocol: "http".to_string(),
            methods: vec!["GET".to_string()],
            auth_mode: "workload".to_string(),
            provider_auth_mode: "workload".to_string(),
            permission: "storage.object.read".to_string(),
            timeout_ms: Some(5_000),
            topology_id: "main".to_string(),
            topology_revision_id: "revision-1".to_string(),
            link_source_endpoint: "source".to_string(),
            link_target_endpoint: "target".to_string(),
            credential_ref: String::new(),
            credential_generation: 1,
            context_generation: 1,
            desired_state: ApiBindingDesiredState::Active,
            observed_state: ApiBindingObservedState::Resolved,
            health: ApiBindingHealth::Unknown,
            drift: Vec::new(),
            last_operation_id: String::new(),
            state: ApiBindingState::Resolved,
            optional: false,
            reason: String::new(),
            created_at: "unix-ms:1".to_string(),
            updated_at: "unix-ms:1".to_string(),
        }
    }

    #[test]
    fn compatibility_state_is_derived_and_wire_values_remain_stable() {
        let mut value = binding();
        assert_eq!(value.derived_state(), ApiBindingState::Resolved);
        let wire = serde_json::to_value(&value).unwrap();
        assert_eq!(wire["desired_state"], "ACTIVE");
        assert_eq!(wire["observed_state"], "RESOLVED");
        assert_eq!(wire["health"], "UNKNOWN");

        value.state = ApiBindingState::Active;
        assert!(value.validate().is_err());
        value.observed_state = ApiBindingObservedState::Active;
        value.health = ApiBindingHealth::Degraded;
        assert_eq!(value.derived_state(), ApiBindingState::Active);
        value.validate().unwrap();
    }

    fn candidate(id: &str, node: &str, version: &str) -> ApiProviderCandidate {
        ApiProviderCandidate {
            deployment_id: id.to_string(),
            service_id: "storage".to_string(),
            node_id: node.to_string(),
            endpoint: format!("10.0.0.1:8080:{id}"),
            path: "/objects".to_string(),
            api_id: "storage.object.get".to_string(),
            api_version: version.to_string(),
            protocol: "http".to_string(),
            methods: vec!["GET".to_string()],
            auth_mode: "service".to_string(),
            permission: "storage.object.read".to_string(),
            healthy: true,
        }
    }

    #[test]
    fn resolver_rejects_multiple_healthy_candidates_without_explicit_selection() {
        let request = ApiBindingResolutionRequest {
            requirement_name: "STORAGE_GET".to_string(),
            api_id: "storage.object.get".to_string(),
            version_requirement: ">=1, <2".to_string(),
            consumer_node_id: "node-b".to_string(),
            provider_deployment_id: String::new(),
            optional: false,
            selection: "nearest-healthy".to_string(),
        };
        let candidates = vec![
            candidate("remote", "node-a", "v1"),
            candidate("local", "node-b", "1.2.0"),
        ];
        assert!(matches!(
            resolve_api_binding_candidate(&request, &candidates),
            Err(ApiBindingResolutionError::Ambiguous { requirement_name })
                if requirement_name == "STORAGE_GET"
        ));

        let explicit = ApiBindingResolutionRequest {
            provider_deployment_id: "local".to_string(),
            ..request
        };
        assert_eq!(
            resolve_api_binding_candidate(&explicit, &candidates)
                .unwrap()
                .unwrap()
                .deployment_id,
            "local"
        );
    }

    #[test]
    fn optional_requirement_can_remain_unbound() {
        let request = ApiBindingResolutionRequest {
            requirement_name: "OPTIONAL".to_string(),
            api_id: "missing.api".to_string(),
            version_requirement: "*".to_string(),
            consumer_node_id: "node-b".to_string(),
            provider_deployment_id: String::new(),
            optional: true,
            selection: "nearest-healthy".to_string(),
        };
        assert_eq!(resolve_api_binding_candidate(&request, &[]).unwrap(), None);
    }
}
