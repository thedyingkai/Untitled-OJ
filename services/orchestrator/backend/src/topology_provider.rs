use orchestrator_legacy::{
    ApiBinding, ApiBindingState, TopologyEndpointStatus, TopologyLinkStatus, TopologySpec,
    parse_endpoint_id, validate_endpoint_id,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fmt;
use std::io::Read;
use std::time::Duration;
use thiserror::Error;
use ureq::Agent;

const PROVIDER_API_VERSION: &str = "v1";
// A saga can perform at most four sequential provider calls (Gateway apply,
// Auth apply, Auth compensation, Gateway compensation). Keeping each call at
// or below five seconds leaves room inside the control-plane's 30 second lease.
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(3);
const MAX_TIMEOUT: Duration = Duration::from_secs(5);
const DEFAULT_MAX_REQUEST_BYTES: usize = 8 * 1024 * 1024;
const DEFAULT_MAX_RESPONSE_BYTES: usize = 64 * 1024;
const MAX_CONFIGURED_REQUEST_BYTES: usize = 64 * 1024 * 1024;
const MAX_CONFIGURED_RESPONSE_BYTES: usize = 1024 * 1024;

/// Explicit configuration for one controlled management endpoint.
///
/// `origin` is deliberately limited to an HTTP(S) origin. Paths, queries,
/// fragments, and embedded credentials are rejected so that callers cannot
/// redirect topology writes away from the provider's fixed v1 resource path.
#[derive(Clone)]
pub(crate) struct HttpManagementProviderConfig {
    origin: String,
    bearer_token: Option<String>,
}

impl HttpManagementProviderConfig {
    pub(crate) fn new(origin: impl Into<String>) -> Result<Self, TopologyProviderBuildError> {
        Ok(Self {
            origin: normalize_origin(&origin.into())?,
            bearer_token: None,
        })
    }

    pub(crate) fn with_bearer_token(
        mut self,
        bearer_token: impl Into<String>,
    ) -> Result<Self, TopologyProviderBuildError> {
        let bearer_token = bearer_token.into();
        if bearer_token.is_empty()
            || bearer_token.len() > 4096
            || !bearer_token.bytes().all(|byte| byte.is_ascii_graphic())
        {
            return Err(TopologyProviderBuildError::InvalidBearerToken);
        }
        self.bearer_token = Some(bearer_token);
        Ok(self)
    }
}

impl fmt::Debug for HttpManagementProviderConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("HttpManagementProviderConfig")
            .field("origin", &self.origin)
            .field(
                "bearer_token",
                &self.bearer_token.as_ref().map(|_| "[redacted]"),
            )
            .finish()
    }
}

/// Both providers must be supplied explicitly. There is no environment lookup
/// or no-op fallback in this module.
#[derive(Clone, Debug)]
pub(crate) struct TopologyProviderConfig {
    gateway: Option<HttpManagementProviderConfig>,
    auth: Option<HttpManagementProviderConfig>,
    timeout: Duration,
    max_request_bytes: usize,
    max_response_bytes: usize,
}

impl TopologyProviderConfig {
    pub(crate) fn new(
        gateway: Option<HttpManagementProviderConfig>,
        auth: Option<HttpManagementProviderConfig>,
    ) -> Self {
        Self {
            gateway,
            auth,
            timeout: DEFAULT_TIMEOUT,
            max_request_bytes: DEFAULT_MAX_REQUEST_BYTES,
            max_response_bytes: DEFAULT_MAX_RESPONSE_BYTES,
        }
    }

    pub(crate) fn with_timeout(
        mut self,
        timeout: Duration,
    ) -> Result<Self, TopologyProviderBuildError> {
        if timeout.is_zero() || timeout > MAX_TIMEOUT {
            return Err(TopologyProviderBuildError::InvalidTimeout);
        }
        self.timeout = timeout;
        Ok(self)
    }

    pub(crate) fn with_size_limits(
        mut self,
        max_request_bytes: usize,
        max_response_bytes: usize,
    ) -> Result<Self, TopologyProviderBuildError> {
        if !(1..=MAX_CONFIGURED_REQUEST_BYTES).contains(&max_request_bytes) {
            return Err(TopologyProviderBuildError::InvalidRequestLimit);
        }
        if !(1..=MAX_CONFIGURED_RESPONSE_BYTES).contains(&max_response_bytes) {
            return Err(TopologyProviderBuildError::InvalidResponseLimit);
        }
        self.max_request_bytes = max_request_bytes;
        self.max_response_bytes = max_response_bytes;
        Ok(self)
    }
}

#[derive(Debug, Error, PartialEq, Eq)]
pub(crate) enum TopologyProviderBuildError {
    #[error("Gateway topology management provider is not configured")]
    MissingGateway,
    #[error("Auth topology management provider is not configured")]
    MissingAuth,
    #[error("management provider origin is invalid: {0}")]
    InvalidOrigin(String),
    #[error("management provider bearer token is empty, too long, or not header-safe")]
    InvalidBearerToken,
    #[error("provider timeout must be between 1 nanosecond and 5 seconds")]
    InvalidTimeout,
    #[error("provider request limit must be between 1 byte and 64 MiB")]
    InvalidRequestLimit,
    #[error("provider response limit must be between 1 byte and 1 MiB")]
    InvalidResponseLimit,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub(crate) enum TopologyProviderApplyState {
    Succeeded,
    Failed,
    Degraded,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub(crate) enum TopologyProviderStage {
    Validation,
    GatewayApply,
    AuthApply,
    AuthCompensation,
    GatewayCompensation,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub(crate) enum GatewayCompensation {
    NotRequired,
    RestoredPrevious,
    DeletedTopology,
    Failed,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub(crate) enum AuthCompensation {
    NotRequired,
    RestoredPrevious,
    DeletedTopology,
    Failed,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub(crate) struct TopologyProviderApplyReceipt {
    pub(crate) state: TopologyProviderApplyState,
    pub(crate) topology_id: String,
    pub(crate) revision_id: String,
    pub(crate) operation_id: String,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub(crate) struct TopologyProviderApplyFailure {
    pub(crate) state: TopologyProviderApplyState,
    pub(crate) failed_stage: TopologyProviderStage,
    pub(crate) auth_compensation: AuthCompensation,
    pub(crate) gateway_compensation: GatewayCompensation,
    pub(crate) detail: String,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub(crate) enum TopologyProviderObservedState {
    Present,
    Absent,
    Unreachable,
}

/// One provider's independently observed topology state.  Observation never
/// falls back to the last apply acknowledgement: an unavailable or malformed
/// management endpoint is represented as `UNREACHABLE` so the reconciler can
/// expose drift instead of reporting a false `IN_SYNC` state.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub(crate) struct TopologyProviderObservation {
    pub(crate) provider: String,
    pub(crate) state: TopologyProviderObservedState,
    pub(crate) observed_revision_id: Option<String>,
    pub(crate) observed_content_sha256: Option<String>,
    /// Digest of the provider's effective route/grant projection.  Older
    /// providers may omit this field while rolling forward, but an omitted
    /// digest never matches desired state and is therefore repaired
    /// fail-closed by the reconciler.
    pub(crate) observed_projection_sha256: Option<String>,
    pub(crate) endpoints: Vec<TopologyEndpointStatus>,
    pub(crate) links: Vec<TopologyLinkStatus>,
    pub(crate) detail: String,
}

impl TopologyProviderObservation {
    fn unreachable(provider: ProviderKind, detail: impl Into<String>) -> Self {
        Self {
            provider: provider.as_str().to_string(),
            state: TopologyProviderObservedState::Unreachable,
            observed_revision_id: None,
            observed_content_sha256: None,
            observed_projection_sha256: None,
            endpoints: Vec::new(),
            links: Vec::new(),
            detail: detail.into(),
        }
    }

    pub(crate) fn matches(
        &self,
        revision_id: &str,
        content_sha256: &str,
        projection_sha256: &str,
    ) -> bool {
        self.state == TopologyProviderObservedState::Present
            && self.observed_revision_id.as_deref() == Some(revision_id)
            && self.observed_content_sha256.as_deref() == Some(content_sha256)
            && self.observed_projection_sha256.as_deref() == Some(projection_sha256)
    }
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub(crate) struct TopologyProvidersObservation {
    pub(crate) gateway: TopologyProviderObservation,
    pub(crate) auth: TopologyProviderObservation,
}

impl fmt::Display for TopologyProviderApplyFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "topology provider apply {:?} at {:?}: {}",
            self.state, self.failed_stage, self.detail
        )
    }
}

impl std::error::Error for TopologyProviderApplyFailure {}

#[derive(Clone)]
pub(crate) struct TopologyProviderSaga {
    gateway: HttpManagementProviderConfig,
    auth: HttpManagementProviderConfig,
    agent: Agent,
    max_request_bytes: usize,
    max_response_bytes: usize,
}

/// Runtime availability changes do not create a new immutable Topology
/// revision, but they still have to update the exact Gateway route and Auth
/// grant projection for that revision.  Revocation is deliberately ordered
/// Gateway-first, while restoration is Auth-first, so a partial provider
/// failure can only leave the workload denied.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RuntimeProjectionOrder {
    RevokeFirst,
    GrantFirst,
}

impl TopologyProviderSaga {
    pub(crate) fn from_config(
        config: TopologyProviderConfig,
    ) -> Result<Self, TopologyProviderBuildError> {
        let gateway = config
            .gateway
            .ok_or(TopologyProviderBuildError::MissingGateway)?;
        let auth = config.auth.ok_or(TopologyProviderBuildError::MissingAuth)?;
        let agent: Agent = Agent::config_builder()
            .timeout_global(Some(config.timeout))
            .http_status_as_error(false)
            .max_redirects(0)
            .proxy(None)
            .build()
            .into();
        Ok(Self {
            gateway,
            auth,
            agent,
            max_request_bytes: config.max_request_bytes,
            max_response_bytes: config.max_response_bytes,
        })
    }

    /// Applies Gateway first and Auth second. This function performs network I/O
    /// and must therefore be invoked outside database transactions.
    ///
    /// If Auth fails, Gateway is restored to `previous`, or deleted when this is
    /// the first applied revision. A successful compensation yields `FAILED`; a
    /// failed compensation yields `DEGRADED` and must be reconciled later.
    #[cfg(test)]
    pub(crate) fn apply(
        &self,
        topology_id: &str,
        revision_id: &str,
        spec: &TopologySpec,
        previous_revision_id: Option<&str>,
        previous: Option<&TopologySpec>,
        operation_id: &str,
    ) -> Result<TopologyProviderApplyReceipt, TopologyProviderApplyFailure> {
        self.apply_with_bindings(
            topology_id,
            revision_id,
            spec,
            &[],
            previous_revision_id,
            previous,
            &[],
            operation_id,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn apply_with_bindings(
        &self,
        topology_id: &str,
        revision_id: &str,
        spec: &TopologySpec,
        bindings: &[ApiBinding],
        previous_revision_id: Option<&str>,
        previous: Option<&TopologySpec>,
        previous_bindings: &[ApiBinding],
        operation_id: &str,
    ) -> Result<TopologyProviderApplyReceipt, TopologyProviderApplyFailure> {
        self.validate_apply(
            topology_id,
            revision_id,
            spec,
            previous_revision_id,
            previous,
            operation_id,
        )?;

        let desired_sha256 = spec.content_sha256().map_err(validation_failure)?;
        let previous_sha256 = previous
            .map(TopologySpec::content_sha256)
            .transpose()
            .map_err(validation_failure)?;

        // Pre-serialize and size-check every possible request before the first
        // external side effect. This prevents a local validation error from
        // stranding a partially applied topology.
        let desired_projection = provider_projection(bindings).map_err(validation_failure)?;
        let previous_projection =
            provider_projection(previous_bindings).map_err(validation_failure)?;
        let gateway_apply = self.encode_request(&ProviderRequest {
            api_version: PROVIDER_API_VERSION,
            provider: ProviderKind::Gateway.as_str(),
            action: ProviderAction::Apply.as_str(),
            topology_id,
            attempted_revision_id: revision_id,
            desired_revision_id: Some(revision_id),
            desired_content_sha256: Some(&desired_sha256),
            operation_id,
            spec: Some(spec),
            routes: &desired_projection.routes,
            grants: &desired_projection.grants,
        })?;
        let auth_apply = self.encode_request(&ProviderRequest {
            api_version: PROVIDER_API_VERSION,
            provider: ProviderKind::Auth.as_str(),
            action: ProviderAction::Apply.as_str(),
            topology_id,
            attempted_revision_id: revision_id,
            desired_revision_id: Some(revision_id),
            desired_content_sha256: Some(&desired_sha256),
            operation_id,
            spec: Some(spec),
            routes: &desired_projection.routes,
            grants: &desired_projection.grants,
        })?;
        let compensation_action = if previous.is_some() {
            ProviderAction::RestorePrevious
        } else {
            ProviderAction::Delete
        };
        let gateway_compensation = self.encode_request(&ProviderRequest {
            api_version: PROVIDER_API_VERSION,
            provider: ProviderKind::Gateway.as_str(),
            action: compensation_action.as_str(),
            topology_id,
            attempted_revision_id: revision_id,
            desired_revision_id: previous_revision_id,
            desired_content_sha256: previous_sha256.as_deref(),
            operation_id,
            spec: previous,
            routes: &previous_projection.routes,
            grants: &previous_projection.grants,
        })?;
        let auth_compensation = self.encode_request(&ProviderRequest {
            api_version: PROVIDER_API_VERSION,
            provider: ProviderKind::Auth.as_str(),
            action: compensation_action.as_str(),
            topology_id,
            attempted_revision_id: revision_id,
            desired_revision_id: previous_revision_id,
            desired_content_sha256: previous_sha256.as_deref(),
            operation_id,
            spec: previous,
            routes: &previous_projection.routes,
            grants: &previous_projection.grants,
        })?;
        let desired_state = ExpectedProviderState::present(revision_id, &desired_sha256);
        let compensated_state = match (previous_revision_id, previous_sha256.as_deref()) {
            (Some(previous_revision_id), Some(previous_sha256)) => {
                ExpectedProviderState::present(previous_revision_id, previous_sha256)
            }
            (None, None) => ExpectedProviderState::absent(),
            _ => unreachable!("previous revision and spec are validated as one unit"),
        };

        if let Err(gateway_failure) = self.call_provider(
            &self.gateway,
            ProviderKind::Gateway,
            ProviderAction::Apply,
            topology_id,
            operation_id,
            &gateway_apply,
            desired_state,
        ) {
            if !gateway_failure.is_outcome_unknown() {
                return Err(TopologyProviderApplyFailure {
                    state: TopologyProviderApplyState::Failed,
                    failed_stage: TopologyProviderStage::GatewayApply,
                    auth_compensation: AuthCompensation::NotRequired,
                    gateway_compensation: GatewayCompensation::NotRequired,
                    detail: gateway_failure.to_string(),
                });
            }
            return match self.call_provider(
                &self.gateway,
                ProviderKind::Gateway,
                compensation_action,
                topology_id,
                operation_id,
                &gateway_compensation,
                compensated_state,
            ) {
                Ok(()) => Err(TopologyProviderApplyFailure {
                    state: TopologyProviderApplyState::Failed,
                    failed_stage: TopologyProviderStage::GatewayApply,
                    auth_compensation: AuthCompensation::NotRequired,
                    gateway_compensation: gateway_compensation_status(previous.is_some()),
                    detail: format!(
                        "Gateway apply result was unknown, but compensation proved the previous state: {gateway_failure}"
                    ),
                }),
                Err(compensation_failure) => Err(TopologyProviderApplyFailure {
                    state: TopologyProviderApplyState::Degraded,
                    failed_stage: TopologyProviderStage::GatewayCompensation,
                    auth_compensation: AuthCompensation::NotRequired,
                    gateway_compensation: GatewayCompensation::Failed,
                    detail: format!(
                        "Gateway apply result was unknown ({gateway_failure}); Gateway compensation failed ({compensation_failure})"
                    ),
                }),
            };
        }

        if let Err(auth_failure) = self.call_provider(
            &self.auth,
            ProviderKind::Auth,
            ProviderAction::Apply,
            topology_id,
            operation_id,
            &auth_apply,
            desired_state,
        ) {
            let auth_was_unknown = auth_failure.is_outcome_unknown();
            let auth_compensation_result = auth_was_unknown.then(|| {
                self.call_provider(
                    &self.auth,
                    ProviderKind::Auth,
                    compensation_action,
                    topology_id,
                    operation_id,
                    &auth_compensation,
                    compensated_state,
                )
            });
            let gateway_compensation_result = self.call_provider(
                &self.gateway,
                ProviderKind::Gateway,
                compensation_action,
                topology_id,
                operation_id,
                &gateway_compensation,
                compensated_state,
            );
            let auth_compensated = auth_compensation_result.as_ref().is_none_or(Result::is_ok);
            let gateway_compensated = gateway_compensation_result.is_ok();
            if auth_compensated && gateway_compensated {
                return Err(TopologyProviderApplyFailure {
                    state: TopologyProviderApplyState::Failed,
                    failed_stage: TopologyProviderStage::AuthApply,
                    auth_compensation: if auth_was_unknown {
                        auth_compensation_status(previous.is_some())
                    } else {
                        AuthCompensation::NotRequired
                    },
                    gateway_compensation: gateway_compensation_status(previous.is_some()),
                    detail: format!(
                        "Auth apply failed, and provider compensation proved the previous state: {auth_failure}"
                    ),
                });
            }
            let auth_compensation_detail = auth_compensation_result
                .and_then(Result::err)
                .map(|failure| failure.to_string())
                .unwrap_or_else(|| "not required".to_string());
            let gateway_compensation_detail = gateway_compensation_result
                .err()
                .map(|failure| failure.to_string())
                .unwrap_or_else(|| "succeeded".to_string());
            return Err(TopologyProviderApplyFailure {
                state: TopologyProviderApplyState::Degraded,
                failed_stage: if !gateway_compensated {
                    TopologyProviderStage::GatewayCompensation
                } else {
                    TopologyProviderStage::AuthCompensation
                },
                auth_compensation: if auth_was_unknown && !auth_compensated {
                    AuthCompensation::Failed
                } else if auth_was_unknown {
                    auth_compensation_status(previous.is_some())
                } else {
                    AuthCompensation::NotRequired
                },
                gateway_compensation: if gateway_compensated {
                    gateway_compensation_status(previous.is_some())
                } else {
                    GatewayCompensation::Failed
                },
                detail: format!(
                    "Auth apply failed ({auth_failure}); Auth compensation: {auth_compensation_detail}; Gateway compensation: {gateway_compensation_detail}"
                ),
            });
        }

        Ok(TopologyProviderApplyReceipt {
            state: TopologyProviderApplyState::Succeeded,
            topology_id: topology_id.to_string(),
            revision_id: revision_id.to_string(),
            operation_id: operation_id.to_string(),
        })
    }

    /// Reprojects the runtime-effective bindings of an already-applied,
    /// immutable revision.  This is intentionally separate from the topology
    /// apply saga: a stopped, unhealthy, stale, missing, or reassigned runtime
    /// must lose its live route without manufacturing a new revision.
    ///
    /// No compensation restores the previous projection.  In revoke order the
    /// Gateway is narrowed before Auth; in grant order Auth is populated before
    /// Gateway.  Therefore every partial failure remains fail-closed and can be
    /// retried idempotently by the reconciler.
    pub(crate) fn apply_runtime_projection(
        &self,
        topology_id: &str,
        revision_id: &str,
        spec: &TopologySpec,
        bindings: &[ApiBinding],
        operation_id: &str,
        order: RuntimeProjectionOrder,
    ) -> Result<(), String> {
        spec.validate().map_err(|error| error.to_string())?;
        if spec.topology_id != topology_id {
            return Err("runtime projection TopologySpec belongs to another topology".to_string());
        }
        validate_identifier("topology_id", topology_id, 256)?;
        validate_identifier("revision_id", revision_id, 512)?;
        validate_operation_id(operation_id)?;
        if bindings.iter().any(|binding| {
            binding.topology_id != topology_id
                || binding.topology_revision_id != revision_id
                || binding.desired_state != "ACTIVE"
                || binding.state != ApiBindingState::Active
        }) {
            return Err(
                "runtime projection accepts only ACTIVE bindings owned by the applied revision"
                    .to_string(),
            );
        }

        let content_sha256 = spec.content_sha256().map_err(|error| error.to_string())?;
        let projection = provider_projection(bindings)?;
        let gateway_body = self
            .encode_request(&ProviderRequest {
                api_version: PROVIDER_API_VERSION,
                provider: ProviderKind::Gateway.as_str(),
                action: ProviderAction::Apply.as_str(),
                topology_id,
                attempted_revision_id: revision_id,
                desired_revision_id: Some(revision_id),
                desired_content_sha256: Some(&content_sha256),
                operation_id,
                spec: Some(spec),
                routes: &projection.routes,
                grants: &projection.grants,
            })
            .map_err(|error| error.to_string())?;
        let auth_body = self
            .encode_request(&ProviderRequest {
                api_version: PROVIDER_API_VERSION,
                provider: ProviderKind::Auth.as_str(),
                action: ProviderAction::Apply.as_str(),
                topology_id,
                attempted_revision_id: revision_id,
                desired_revision_id: Some(revision_id),
                desired_content_sha256: Some(&content_sha256),
                operation_id,
                spec: Some(spec),
                routes: &projection.routes,
                grants: &projection.grants,
            })
            .map_err(|error| error.to_string())?;
        let expected = ExpectedProviderState::present(revision_id, &content_sha256);
        let gateway = || {
            self.call_provider(
                &self.gateway,
                ProviderKind::Gateway,
                ProviderAction::Apply,
                topology_id,
                operation_id,
                &gateway_body,
                expected,
            )
            .map_err(|error| error.to_string())
        };
        let auth = || {
            self.call_provider(
                &self.auth,
                ProviderKind::Auth,
                ProviderAction::Apply,
                topology_id,
                operation_id,
                &auth_body,
                expected,
            )
            .map_err(|error| error.to_string())
        };
        match order {
            RuntimeProjectionOrder::RevokeFirst => {
                gateway()?;
                auth()?;
            }
            RuntimeProjectionOrder::GrantFirst => {
                auth()?;
                gateway()?;
            }
        }
        Ok(())
    }

    /// Reads both provider projections independently.  Network I/O is bounded
    /// by the same per-request timeout as apply and must be invoked outside a
    /// database transaction.
    pub(crate) fn observe(&self, topology_id: &str) -> TopologyProvidersObservation {
        let invalid = validate_identifier("topology_id", topology_id, 256).err();
        let observe = |config: &HttpManagementProviderConfig, provider: ProviderKind| {
            if let Some(detail) = invalid.as_deref() {
                return TopologyProviderObservation::unreachable(provider, detail);
            }
            self.observe_provider(config, provider, topology_id)
                .unwrap_or_else(|detail| TopologyProviderObservation::unreachable(provider, detail))
        };
        TopologyProvidersObservation {
            gateway: observe(&self.gateway, ProviderKind::Gateway),
            auth: observe(&self.auth, ProviderKind::Auth),
        }
    }

    /// Restores the previously proven provider projection after both provider
    /// applies succeeded but the consumer health gate failed. Gateway is
    /// restored first so newly-issued or old workload tokens lose the failed
    /// route immediately; Auth is then brought to the same revision.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn compensate_applied_revision(
        &self,
        topology_id: &str,
        attempted_revision_id: &str,
        previous_revision_id: Option<&str>,
        previous: Option<&TopologySpec>,
        previous_bindings: &[ApiBinding],
        operation_id: &str,
    ) -> Result<(), String> {
        if previous_revision_id.is_some() != previous.is_some() {
            return Err("previous revision and spec must be supplied together".to_string());
        }
        let previous_sha256 = previous
            .map(TopologySpec::content_sha256)
            .transpose()
            .map_err(|error| error.to_string())?;
        let projection = provider_projection(previous_bindings)?;
        let action = if previous.is_some() {
            ProviderAction::RestorePrevious
        } else {
            ProviderAction::Delete
        };
        let expected = match (previous_revision_id, previous_sha256.as_deref()) {
            (Some(revision), Some(hash)) => ExpectedProviderState::present(revision, hash),
            (None, None) => ExpectedProviderState::absent(),
            _ => return Err("previous revision state is incomplete".to_string()),
        };
        let gateway_body = self
            .encode_request(&ProviderRequest {
                api_version: PROVIDER_API_VERSION,
                provider: ProviderKind::Gateway.as_str(),
                action: action.as_str(),
                topology_id,
                attempted_revision_id,
                desired_revision_id: previous_revision_id,
                desired_content_sha256: previous_sha256.as_deref(),
                operation_id,
                spec: previous,
                routes: &projection.routes,
                grants: &projection.grants,
            })
            .map_err(|error| error.to_string())?;
        let auth_body = self
            .encode_request(&ProviderRequest {
                api_version: PROVIDER_API_VERSION,
                provider: ProviderKind::Auth.as_str(),
                action: action.as_str(),
                topology_id,
                attempted_revision_id,
                desired_revision_id: previous_revision_id,
                desired_content_sha256: previous_sha256.as_deref(),
                operation_id,
                spec: previous,
                routes: &projection.routes,
                grants: &projection.grants,
            })
            .map_err(|error| error.to_string())?;
        let gateway = self.call_provider(
            &self.gateway,
            ProviderKind::Gateway,
            action,
            topology_id,
            operation_id,
            &gateway_body,
            expected,
        );
        let auth = self.call_provider(
            &self.auth,
            ProviderKind::Auth,
            action,
            topology_id,
            operation_id,
            &auth_body,
            expected,
        );
        match (gateway, auth) {
            (Ok(()), Ok(())) => Ok(()),
            (gateway, auth) => Err(format!(
                "post-health compensation failed; Gateway: {}; Auth: {}",
                gateway
                    .err()
                    .map(|error| error.to_string())
                    .unwrap_or_else(|| "succeeded".to_string()),
                auth.err()
                    .map(|error| error.to_string())
                    .unwrap_or_else(|| "succeeded".to_string())
            )),
        }
    }

    fn validate_apply(
        &self,
        topology_id: &str,
        revision_id: &str,
        spec: &TopologySpec,
        previous_revision_id: Option<&str>,
        previous: Option<&TopologySpec>,
        operation_id: &str,
    ) -> Result<(), TopologyProviderApplyFailure> {
        let fail = |detail: String| TopologyProviderApplyFailure {
            state: TopologyProviderApplyState::Failed,
            failed_stage: TopologyProviderStage::Validation,
            auth_compensation: AuthCompensation::NotRequired,
            gateway_compensation: GatewayCompensation::NotRequired,
            detail,
        };
        spec.validate().map_err(|error| fail(error.to_string()))?;
        if topology_id != spec.topology_id {
            return Err(fail(
                "topology_id must match the desired TopologySpec".to_string(),
            ));
        }
        if previous_revision_id.is_some() != previous.is_some() {
            return Err(fail(
                "previous_revision_id and previous TopologySpec must be supplied together"
                    .to_string(),
            ));
        }
        if let Some(previous) = previous {
            previous
                .validate()
                .map_err(|error| fail(error.to_string()))?;
            if previous.topology_id != topology_id {
                return Err(fail(
                    "previous TopologySpec must belong to the same topology".to_string(),
                ));
            }
        }
        validate_identifier("revision_id", revision_id, 512).map_err(fail)?;
        if let Some(previous_revision_id) = previous_revision_id {
            validate_identifier("previous_revision_id", previous_revision_id, 512).map_err(fail)?;
            if previous_revision_id == revision_id {
                return Err(fail(
                    "previous_revision_id must differ from revision_id".to_string(),
                ));
            }
        }
        validate_operation_id(operation_id).map_err(fail)
    }

    fn encode_request(
        &self,
        request: &ProviderRequest<'_>,
    ) -> Result<Vec<u8>, TopologyProviderApplyFailure> {
        let body = serde_json::to_vec(request).map_err(|error| TopologyProviderApplyFailure {
            state: TopologyProviderApplyState::Failed,
            failed_stage: TopologyProviderStage::Validation,
            auth_compensation: AuthCompensation::NotRequired,
            gateway_compensation: GatewayCompensation::NotRequired,
            detail: format!("serialize provider request: {error}"),
        })?;
        if body.len() > self.max_request_bytes {
            return Err(TopologyProviderApplyFailure {
                state: TopologyProviderApplyState::Failed,
                failed_stage: TopologyProviderStage::Validation,
                auth_compensation: AuthCompensation::NotRequired,
                gateway_compensation: GatewayCompensation::NotRequired,
                detail: format!(
                    "provider request is {} bytes; configured limit is {} bytes",
                    body.len(),
                    self.max_request_bytes
                ),
            });
        }
        Ok(body)
    }

    #[allow(clippy::too_many_arguments)]
    fn call_provider(
        &self,
        config: &HttpManagementProviderConfig,
        provider: ProviderKind,
        action: ProviderAction,
        topology_id: &str,
        operation_id: &str,
        body: &[u8],
        expected: ExpectedProviderState<'_>,
    ) -> Result<(), ProviderCallFailure> {
        let url = format!(
            "{}/api/v1/topologies/{}",
            config.origin,
            percent_encode_path_segment(topology_id)
        );
        let idempotency_key = format!("{operation_id}:{}:{}", provider.as_str(), action.as_str());
        let request = match action {
            ProviderAction::Delete => self.agent.delete(&url).force_send_body(),
            ProviderAction::Apply | ProviderAction::RestorePrevious => self.agent.put(&url),
        };
        let mut request = request
            .header("Content-Type", "application/json")
            .header("Accept", "application/json")
            .header("Idempotency-Key", &idempotency_key)
            .header("X-Orchestrator-Operation-Id", operation_id);
        if let Some(token) = config.bearer_token.as_deref() {
            request = request.header("Authorization", format!("Bearer {token}"));
        }
        let response = request.send(body).map_err(|error| {
            ProviderCallFailure::unknown(format!(
                "{} {} request failed: {error}",
                provider.as_str(),
                action.as_str()
            ))
        })?;
        let status = response.status().as_u16();
        let content_type = response
            .headers()
            .get("content-type")
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .split(';')
            .next()
            .unwrap_or_default()
            .trim()
            .to_ascii_lowercase();
        if content_type != "application/json" && content_type != "application/problem+json" {
            return Err(ProviderCallFailure::unknown(format!(
                "{} {} returned unsupported Content-Type",
                provider.as_str(),
                action.as_str()
            )));
        }
        let mut response_body = Vec::new();
        response
            .into_body()
            .into_reader()
            .take(self.max_response_bytes as u64 + 1)
            .read_to_end(&mut response_body)
            .map_err(|error| {
                ProviderCallFailure::unknown(format!(
                    "{} {} response read failed: {error}",
                    provider.as_str(),
                    action.as_str()
                ))
            })?;
        if response_body.len() > self.max_response_bytes {
            return Err(ProviderCallFailure::unknown(format!(
                "{} {} response exceeded {} bytes",
                provider.as_str(),
                action.as_str(),
                self.max_response_bytes
            )));
        }
        serde_json::from_slice::<serde_json::Value>(&response_body).map_err(|_| {
            ProviderCallFailure::unknown(format!(
                "{} {} returned invalid JSON",
                provider.as_str(),
                action.as_str()
            ))
        })?;
        if !(200..=299).contains(&status) {
            return Err(ProviderCallFailure::known_rejected(format!(
                "{} {} returned HTTP {status}",
                provider.as_str(),
                action.as_str()
            )));
        }
        if status != 200 {
            return Err(ProviderCallFailure::unknown(format!(
                "{} {} returned non-terminal HTTP {status}; only synchronous HTTP 200 is accepted",
                provider.as_str(),
                action.as_str()
            )));
        }
        let ack: ProviderAck = serde_json::from_slice(&response_body).map_err(|_| {
            ProviderCallFailure::unknown(format!(
                "{} {} returned an invalid acknowledgement",
                provider.as_str(),
                action.as_str()
            ))
        })?;
        ack.verify(provider, action, topology_id, operation_id, expected)
            .map_err(ProviderCallFailure::unknown)
    }

    fn observe_provider(
        &self,
        config: &HttpManagementProviderConfig,
        provider: ProviderKind,
        topology_id: &str,
    ) -> Result<TopologyProviderObservation, String> {
        let url = format!(
            "{}/api/v1/topologies/{}",
            config.origin,
            percent_encode_path_segment(topology_id)
        );
        let mut request = self.agent.get(&url).header("Accept", "application/json");
        if let Some(token) = config.bearer_token.as_deref() {
            request = request.header("Authorization", format!("Bearer {token}"));
        }
        let response = request
            .call()
            .map_err(|error| format!("{} observe request failed: {error}", provider.as_str()))?;
        let status = response.status().as_u16();
        let content_type = response
            .headers()
            .get("content-type")
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .split(';')
            .next()
            .unwrap_or_default()
            .trim()
            .to_ascii_lowercase();
        if content_type != "application/json" && content_type != "application/problem+json" {
            return Err(format!(
                "{} observe returned unsupported Content-Type",
                provider.as_str()
            ));
        }
        let mut response_body = Vec::new();
        response
            .into_body()
            .into_reader()
            .take(self.max_response_bytes as u64 + 1)
            .read_to_end(&mut response_body)
            .map_err(|error| {
                format!(
                    "{} observe response read failed: {error}",
                    provider.as_str()
                )
            })?;
        if response_body.len() > self.max_response_bytes {
            return Err(format!(
                "{} observe response exceeded {} bytes",
                provider.as_str(),
                self.max_response_bytes
            ));
        }
        if status != 200 {
            return Err(format!(
                "{} observe returned HTTP {status}",
                provider.as_str()
            ));
        }
        let observation: ProviderStatus = serde_json::from_slice(&response_body)
            .map_err(|_| format!("{} observe returned invalid JSON", provider.as_str()))?;
        observation.verify(provider, topology_id)
    }
}

#[derive(Debug, Clone, Copy)]
enum ProviderKind {
    Gateway,
    Auth,
}

impl ProviderKind {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Gateway => "gateway",
            Self::Auth => "auth",
        }
    }
}

#[derive(Debug, Clone, Copy)]
enum ProviderAction {
    Apply,
    RestorePrevious,
    Delete,
}

impl ProviderAction {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Apply => "apply",
            Self::RestorePrevious => "restore_previous",
            Self::Delete => "delete",
        }
    }
}

#[derive(Serialize)]
#[serde(deny_unknown_fields)]
struct ProviderRequest<'a> {
    api_version: &'static str,
    provider: &'static str,
    action: &'static str,
    topology_id: &'a str,
    attempted_revision_id: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    desired_revision_id: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    desired_content_sha256: Option<&'a str>,
    operation_id: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    spec: Option<&'a TopologySpec>,
    routes: &'a [ProviderBindingRoute],
    grants: &'a [ProviderBindingGrant],
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct ProviderBindingRoute {
    binding_id: String,
    requirement_name: String,
    consumer_deployment_id: String,
    consumer_service_id: String,
    consumer_node_id: String,
    credential_generation: u64,
    api_id: String,
    provider_deployment_id: String,
    provider_service_id: String,
    provider_node_id: String,
    provider_endpoint: String,
    upstream_base: String,
    provider_path: String,
    virtual_path: String,
    auth_mode: String,
    provider_auth_mode: String,
    permission: String,
    methods: Vec<String>,
    timeout_ms: u64,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct ProviderBindingGrant {
    binding_id: String,
    requirement_name: String,
    consumer_deployment_id: String,
    consumer_service_id: String,
    consumer_node_id: String,
    credential_generation: u64,
    api_id: String,
    permission: String,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ProviderProjection {
    routes: Vec<ProviderBindingRoute>,
    grants: Vec<ProviderBindingGrant>,
}

impl ProviderProjection {
    /// Produces the cross-language canonical representation used by Gateway,
    /// Auth and the control plane.  Request order is deliberately irrelevant:
    /// providers persist by binding id, so the digest follows the same stable
    /// ordering and serializes the typed `{routes,grants}` object only.
    fn canonical_json(mut self) -> Result<Vec<u8>, String> {
        self.routes
            .sort_by(|left, right| left.binding_id.cmp(&right.binding_id));
        self.grants
            .sort_by(|left, right| left.binding_id.cmp(&right.binding_id));
        let encoded = serde_json::to_vec(&self)
            .map_err(|error| format!("serialize provider projection digest: {error}"))?;
        Ok(go_json_compatible_string_escaping(encoded))
    }

    fn canonical_sha256(self) -> Result<String, String> {
        Ok(format!("{:x}", Sha256::digest(self.canonical_json()?)))
    }
}

/// Go's `encoding/json` deliberately escapes the three HTML-sensitive ASCII
/// characters plus the two JavaScript line separators.  The providers use
/// that encoder while Rust uses `serde_json`, so normalize those five byte
/// sequences explicitly to keep the digest cross-language for every valid
/// UTF-8 identifier/path, not only the common ASCII subset.
fn go_json_compatible_string_escaping(encoded: Vec<u8>) -> Vec<u8> {
    let mut normalized = Vec::with_capacity(encoded.len());
    let mut index = 0;
    while index < encoded.len() {
        match encoded[index] {
            b'<' => normalized.extend_from_slice(br"\u003c"),
            b'>' => normalized.extend_from_slice(br"\u003e"),
            b'&' => normalized.extend_from_slice(br"\u0026"),
            0xe2 if encoded.get(index + 1) == Some(&0x80)
                && matches!(encoded.get(index + 2), Some(0xa8 | 0xa9)) =>
            {
                let suffix = if encoded[index + 2] == 0xa8 {
                    b'8'
                } else {
                    b'9'
                };
                normalized.extend_from_slice(br"\u202");
                normalized.push(suffix);
                index += 2;
            }
            byte => normalized.push(byte),
        }
        index += 1;
    }
    normalized
}

pub(crate) fn provider_projection_sha256(bindings: &[ApiBinding]) -> Result<String, String> {
    provider_projection(bindings)?.canonical_sha256()
}

#[cfg(test)]
pub(crate) fn provider_projection_sha256_from_json(
    routes: &serde_json::Value,
    grants: &serde_json::Value,
) -> Result<String, String> {
    ProviderProjection {
        routes: serde_json::from_value(routes.clone())
            .map_err(|error| format!("decode provider routes for digest: {error}"))?,
        grants: serde_json::from_value(grants.clone())
            .map_err(|error| format!("decode provider grants for digest: {error}"))?,
    }
    .canonical_sha256()
}

fn provider_projection(bindings: &[ApiBinding]) -> Result<ProviderProjection, String> {
    let mut projection = ProviderProjection::default();
    for binding in bindings.iter().filter(|binding| {
        binding.desired_state == "ACTIVE"
            && matches!(
                binding.state,
                ApiBindingState::Pending | ApiBindingState::Resolved | ApiBindingState::Active
            )
    }) {
        binding.validate().map_err(|error| error.to_string())?;
        if binding.auth_mode != "workload" {
            return Err(format!(
                "binding {} must use workload Gateway authentication",
                binding.binding_id
            ));
        }
        let identity = parse_endpoint_id(&binding.provider_endpoint)
            .map_err(|error| format!("binding provider endpoint is invalid: {error}"))?;
        let host = if identity.host.contains(':') {
            format!("[{}]", identity.host)
        } else {
            identity.host.to_string()
        };
        projection.routes.push(ProviderBindingRoute {
            binding_id: binding.binding_id.clone(),
            requirement_name: binding.requirement_name.clone(),
            consumer_deployment_id: binding.consumer_deployment_id.clone(),
            consumer_service_id: binding.consumer_service_id.clone(),
            consumer_node_id: binding.consumer_node_id.clone(),
            credential_generation: binding.credential_generation,
            api_id: binding.api_id.clone(),
            provider_deployment_id: binding.provider_deployment_id.clone(),
            provider_service_id: binding.provider_service_id.clone(),
            provider_node_id: binding.provider_node_id.clone(),
            provider_endpoint: binding.provider_endpoint.clone(),
            upstream_base: format!("{}://{host}:{}", binding.protocol, identity.port),
            provider_path: binding.provider_path.clone(),
            virtual_path: binding.virtual_endpoint.clone(),
            auth_mode: binding.auth_mode.clone(),
            provider_auth_mode: binding.provider_auth_mode.clone(),
            permission: binding.permission.clone(),
            methods: binding.methods.clone(),
            timeout_ms: binding.timeout_ms.unwrap_or(30_000),
        });
        projection.grants.push(ProviderBindingGrant {
            binding_id: binding.binding_id.clone(),
            requirement_name: binding.requirement_name.clone(),
            consumer_deployment_id: binding.consumer_deployment_id.clone(),
            consumer_service_id: binding.consumer_service_id.clone(),
            consumer_node_id: binding.consumer_node_id.clone(),
            credential_generation: binding.credential_generation,
            api_id: binding.api_id.clone(),
            permission: binding.permission.clone(),
        });
    }
    projection.routes.sort_by(|left, right| {
        (&left.consumer_deployment_id, &left.requirement_name)
            .cmp(&(&right.consumer_deployment_id, &right.requirement_name))
    });
    projection.grants.sort_by(|left, right| {
        (&left.consumer_deployment_id, &left.requirement_name)
            .cmp(&(&right.consumer_deployment_id, &right.requirement_name))
    });
    let unique = projection
        .routes
        .iter()
        .map(|route| (&route.consumer_deployment_id, &route.requirement_name))
        .collect::<std::collections::BTreeSet<_>>();
    if unique.len() != projection.routes.len() {
        return Err("provider projection contains duplicate consumer requirements".to_string());
    }
    Ok(projection)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ProviderAck {
    api_version: String,
    provider: String,
    action: String,
    topology_id: String,
    operation_id: String,
    completed: bool,
    observed_revision_id: Option<String>,
    observed_content_sha256: Option<String>,
    absent: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ProviderStatus {
    api_version: String,
    provider: String,
    topology_id: String,
    observed_revision_id: Option<String>,
    observed_content_sha256: Option<String>,
    #[serde(default)]
    observed_projection_sha256: Option<String>,
    absent: bool,
    #[serde(default)]
    endpoints: Vec<TopologyEndpointStatus>,
    #[serde(default)]
    links: Vec<TopologyLinkStatus>,
}

impl ProviderStatus {
    fn verify(
        self,
        provider: ProviderKind,
        topology_id: &str,
    ) -> Result<TopologyProviderObservation, String> {
        if self.api_version != PROVIDER_API_VERSION
            || self.provider != provider.as_str()
            || self.topology_id != topology_id
        {
            return Err(format!(
                "{} observe response identity did not match the request",
                provider.as_str()
            ));
        }
        if self.absent {
            if self.observed_revision_id.is_some()
                || self.observed_content_sha256.is_some()
                || self.observed_projection_sha256.is_some()
                || !self.endpoints.is_empty()
                || !self.links.is_empty()
            {
                return Err(format!(
                    "{} observe response marked absent but included observed state",
                    provider.as_str()
                ));
            }
            return Ok(TopologyProviderObservation {
                provider: provider.as_str().to_string(),
                state: TopologyProviderObservedState::Absent,
                observed_revision_id: None,
                observed_content_sha256: None,
                observed_projection_sha256: None,
                endpoints: Vec::new(),
                links: Vec::new(),
                detail: String::new(),
            });
        }
        let revision_id = self.observed_revision_id.ok_or_else(|| {
            format!(
                "{} observe response omitted observed_revision_id",
                provider.as_str()
            )
        })?;
        validate_identifier("observed_revision_id", &revision_id, 512)?;
        let content_sha256 = self.observed_content_sha256.ok_or_else(|| {
            format!(
                "{} observe response omitted observed_content_sha256",
                provider.as_str()
            )
        })?;
        if !is_lowercase_sha256(&content_sha256) {
            return Err(format!(
                "{} observe response contained an invalid content hash",
                provider.as_str()
            ));
        }
        if let Some(projection_sha256) = self.observed_projection_sha256.as_deref()
            && !is_lowercase_sha256(projection_sha256)
        {
            return Err(format!(
                "{} observe response contained an invalid projection hash",
                provider.as_str()
            ));
        }
        let mut endpoint_ids = std::collections::BTreeSet::new();
        let mut endpoints = self.endpoints;
        for endpoint in &endpoints {
            validate_endpoint_id(&endpoint.endpoint).map_err(|error| {
                format!(
                    "{} observe response contained an invalid endpoint: {error}",
                    provider.as_str()
                )
            })?;
            validate_optional_observed_text("endpoint message", &endpoint.message, 4_096)?;
            validate_observed_text("endpoint observed_at", &endpoint.observed_at, 512)?;
            if !endpoint_ids.insert(endpoint.endpoint.as_str()) {
                return Err(format!(
                    "{} observe response repeated endpoint {}",
                    provider.as_str(),
                    endpoint.endpoint
                ));
            }
        }
        endpoints.sort_by(|left, right| left.endpoint.cmp(&right.endpoint));
        let mut link_ids = std::collections::BTreeSet::new();
        let mut links = self.links;
        for link in &links {
            validate_endpoint_id(&link.source_endpoint).map_err(|error| {
                format!(
                    "{} observe response contained an invalid link source: {error}",
                    provider.as_str()
                )
            })?;
            validate_endpoint_id(&link.target_endpoint).map_err(|error| {
                format!(
                    "{} observe response contained an invalid link target: {error}",
                    provider.as_str()
                )
            })?;
            validate_optional_observed_text("link message", &link.message, 4_096)?;
            validate_observed_text("link observed_at", &link.observed_at, 512)?;
            let key = (link.source_endpoint.as_str(), link.target_endpoint.as_str());
            if !link_ids.insert(key) {
                return Err(format!(
                    "{} observe response repeated link {} -> {}",
                    provider.as_str(),
                    link.source_endpoint,
                    link.target_endpoint
                ));
            }
        }
        links.sort_by(|left, right| {
            (&left.source_endpoint, &left.target_endpoint)
                .cmp(&(&right.source_endpoint, &right.target_endpoint))
        });
        Ok(TopologyProviderObservation {
            provider: provider.as_str().to_string(),
            state: TopologyProviderObservedState::Present,
            observed_revision_id: Some(revision_id),
            observed_content_sha256: Some(content_sha256),
            observed_projection_sha256: self.observed_projection_sha256,
            endpoints,
            links,
            detail: String::new(),
        })
    }
}

impl ProviderAck {
    fn verify(
        self,
        provider: ProviderKind,
        action: ProviderAction,
        topology_id: &str,
        operation_id: &str,
        expected: ExpectedProviderState<'_>,
    ) -> Result<(), String> {
        if self.api_version != PROVIDER_API_VERSION
            || self.provider != provider.as_str()
            || self.action != action.as_str()
            || self.topology_id != topology_id
            || self.operation_id != operation_id
            || !self.completed
            || self.observed_revision_id.as_deref() != expected.revision_id
            || self.observed_content_sha256.as_deref() != expected.content_sha256
            || self.absent != expected.absent
        {
            return Err(format!(
                "{} {} acknowledgement did not match the request",
                provider.as_str(),
                action.as_str()
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy)]
struct ExpectedProviderState<'a> {
    revision_id: Option<&'a str>,
    content_sha256: Option<&'a str>,
    absent: bool,
}

impl<'a> ExpectedProviderState<'a> {
    const fn present(revision_id: &'a str, content_sha256: &'a str) -> Self {
        Self {
            revision_id: Some(revision_id),
            content_sha256: Some(content_sha256),
            absent: false,
        }
    }

    const fn absent() -> Self {
        Self {
            revision_id: None,
            content_sha256: None,
            absent: true,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ProviderCallCertainty {
    KnownRejected,
    OutcomeUnknown,
}

#[derive(Debug)]
struct ProviderCallFailure {
    certainty: ProviderCallCertainty,
    detail: String,
}

impl ProviderCallFailure {
    fn known_rejected(detail: String) -> Self {
        Self {
            certainty: ProviderCallCertainty::KnownRejected,
            detail,
        }
    }

    fn unknown(detail: String) -> Self {
        Self {
            certainty: ProviderCallCertainty::OutcomeUnknown,
            detail,
        }
    }

    fn is_outcome_unknown(&self) -> bool {
        self.certainty == ProviderCallCertainty::OutcomeUnknown
    }
}

impl fmt::Display for ProviderCallFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{} ({:?})", self.detail, self.certainty)
    }
}

fn validation_failure(detail: impl ToString) -> TopologyProviderApplyFailure {
    TopologyProviderApplyFailure {
        state: TopologyProviderApplyState::Failed,
        failed_stage: TopologyProviderStage::Validation,
        auth_compensation: AuthCompensation::NotRequired,
        gateway_compensation: GatewayCompensation::NotRequired,
        detail: detail.to_string(),
    }
}

const fn gateway_compensation_status(had_previous: bool) -> GatewayCompensation {
    if had_previous {
        GatewayCompensation::RestoredPrevious
    } else {
        GatewayCompensation::DeletedTopology
    }
}

const fn auth_compensation_status(had_previous: bool) -> AuthCompensation {
    if had_previous {
        AuthCompensation::RestoredPrevious
    } else {
        AuthCompensation::DeletedTopology
    }
}

fn normalize_origin(raw: &str) -> Result<String, TopologyProviderBuildError> {
    if raw.is_empty() || raw != raw.trim() || raw.len() > 2048 {
        return Err(TopologyProviderBuildError::InvalidOrigin(
            "origin is empty, padded, or too long".to_string(),
        ));
    }
    let uri = raw.parse::<ureq::http::Uri>().map_err(|_| {
        TopologyProviderBuildError::InvalidOrigin("origin is not a valid URI".to_string())
    })?;
    if !matches!(uri.scheme_str(), Some("http" | "https")) || uri.authority().is_none() {
        return Err(TopologyProviderBuildError::InvalidOrigin(
            "origin must use http or https and include an authority".to_string(),
        ));
    }
    if uri
        .authority()
        .is_some_and(|authority| authority.as_str().contains('@'))
    {
        return Err(TopologyProviderBuildError::InvalidOrigin(
            "origin must not contain embedded credentials".to_string(),
        ));
    }
    if !matches!(uri.path(), "" | "/") || uri.query().is_some() {
        return Err(TopologyProviderBuildError::InvalidOrigin(
            "origin must not contain a path or query".to_string(),
        ));
    }
    Ok(raw.trim_end_matches('/').to_string())
}

fn validate_identifier(name: &str, value: &str, max_len: usize) -> Result<(), String> {
    if value.is_empty()
        || value != value.trim()
        || value.len() > max_len
        || value.chars().any(char::is_control)
    {
        return Err(format!(
            "{name} is empty, padded, too long, or contains control characters"
        ));
    }
    Ok(())
}

fn is_lowercase_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn validate_observed_text(name: &str, value: &str, max_len: usize) -> Result<(), String> {
    if value.is_empty()
        || value != value.trim()
        || value.len() > max_len
        || value.chars().any(char::is_control)
    {
        return Err(format!(
            "{name} is empty, padded, too long, or contains control characters"
        ));
    }
    Ok(())
}

fn validate_optional_observed_text(name: &str, value: &str, max_len: usize) -> Result<(), String> {
    if value != value.trim() || value.len() > max_len || value.chars().any(char::is_control) {
        return Err(format!(
            "{name} is padded, too long, or contains control characters"
        ));
    }
    Ok(())
}

fn validate_operation_id(value: &str) -> Result<(), String> {
    validate_identifier("operation_id", value, 128)?;
    if !value
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b':'))
    {
        return Err(
            "operation_id contains characters that are unsafe in an idempotency key".to_string(),
        );
    }
    Ok(())
}

fn percent_encode_path_segment(value: &str) -> String {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut encoded = String::with_capacity(value.len());
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            encoded.push(char::from(byte));
        } else {
            encoded.push('%');
            encoded.push(char::from(HEX[(byte >> 4) as usize]));
            encoded.push(char::from(HEX[(byte & 0x0f) as usize]));
        }
    }
    encoded
}

#[cfg(test)]
mod tests {
    use super::*;
    use orchestrator_legacy::{TopologyEndpointSpec, TopologyLinkSpec};
    use serde_json::{Value, json};
    use std::collections::BTreeMap;
    use std::io::Write;
    use std::net::{TcpListener, TcpStream};
    use std::thread::{self, JoinHandle};
    use std::time::Instant;

    #[derive(Clone, Copy)]
    struct ExpectedCall {
        provider: &'static str,
        action: &'static str,
        method: &'static str,
        status: u16,
    }

    struct MockProvider {
        origin: String,
        thread: JoinHandle<()>,
    }

    fn topology(note: &str) -> TopologySpec {
        let gateway = TopologyEndpointSpec {
            endpoint: "127.0.0.1:8080:gateway".to_string(),
            service_id: "gateway".to_string(),
            protocol: "http".to_string(),
            health_path: "/health".to_string(),
            display_name: "Gateway".to_string(),
            note: String::new(),
            config: json!({}),
        };
        let service = TopologyEndpointSpec {
            endpoint: "127.0.0.1:8083:problem-service".to_string(),
            service_id: "problem-service".to_string(),
            protocol: "http".to_string(),
            health_path: "/health".to_string(),
            display_name: "Problems".to_string(),
            note: note.to_string(),
            config: json!({}),
        };
        TopologySpec::new(
            "primary",
            gateway.endpoint.clone(),
            "root-only",
            vec![gateway.clone(), service.clone()],
            vec![TopologyLinkSpec {
                source_endpoint: gateway.endpoint,
                target_endpoint: service.endpoint,
                protocol: "http".to_string(),
                auth_mode: "internal".to_string(),
                scope: "api".to_string(),
                enabled: true,
                config_ref: String::new(),
                secret_ref: String::new(),
                policy: json!({}),
                api_bindings: Vec::new(),
            }],
        )
        .expect("valid topology")
    }

    fn saga(gateway_origin: &str, auth_origin: &str) -> TopologyProviderSaga {
        let gateway = HttpManagementProviderConfig::new(gateway_origin).unwrap();
        let auth = HttpManagementProviderConfig::new(auth_origin).unwrap();
        TopologyProviderSaga::from_config(
            TopologyProviderConfig::new(Some(gateway), Some(auth))
                .with_timeout(Duration::from_secs(2))
                .unwrap()
                .with_size_limits(1024 * 1024, 16 * 1024)
                .unwrap(),
        )
        .unwrap()
    }

    #[test]
    fn canonical_projection_digest_matches_the_go_contract() {
        assert_eq!(
            ProviderProjection::default().canonical_sha256().unwrap(),
            "fa9d28278a0d02b19bfebeae5afd5aa6dde1c685d8396acc8defe8832848865c"
        );
        let projection = ProviderProjection {
            routes: vec![ProviderBindingRoute {
                binding_id: "binding-1".to_string(),
                requirement_name: "storage_get".to_string(),
                consumer_deployment_id: "worker-b".to_string(),
                consumer_service_id: "judge-worker".to_string(),
                consumer_node_id: "node-b".to_string(),
                credential_generation: 3,
                api_id: "storage.object.get".to_string(),
                provider_deployment_id: "storage-a".to_string(),
                provider_service_id: "storage".to_string(),
                provider_node_id: "node-a".to_string(),
                provider_endpoint: "10.0.0.1:8080:storage".to_string(),
                upstream_base: "https://10.0.0.1:8080".to_string(),
                provider_path: "/objects".to_string(),
                virtual_path: "/internal/apis/storage.object.get".to_string(),
                auth_mode: "workload".to_string(),
                provider_auth_mode: "workload".to_string(),
                permission: "storage.object.read".to_string(),
                methods: vec!["GET".to_string()],
                timeout_ms: 300_000,
            }],
            grants: vec![ProviderBindingGrant {
                binding_id: "binding-1".to_string(),
                requirement_name: "storage_get".to_string(),
                consumer_deployment_id: "worker-b".to_string(),
                consumer_service_id: "judge-worker".to_string(),
                consumer_node_id: "node-b".to_string(),
                credential_generation: 3,
                api_id: "storage.object.get".to_string(),
                permission: "storage.object.read".to_string(),
            }],
        };
        assert_eq!(
            projection.clone().canonical_sha256().unwrap(),
            "afcaf1f6a8b8be8ae64fa9f7e14d645e3a66657fdeac42cfe8db349b2ba0efbd"
        );
        let mut escaped = projection;
        escaped.routes[0].consumer_service_id = "judge<&>\u{2028}\u{2029}".to_string();
        let canonical = String::from_utf8(escaped.canonical_json().unwrap()).unwrap();
        assert!(
            canonical.contains(r#""consumer_service_id":"judge\u003c\u0026\u003e\u2028\u2029""#)
        );
    }

    #[test]
    fn missing_or_tampered_projection_digest_never_matches_present_state() {
        let revision_id = "primary:r1:0123456789abcdef";
        let content_sha256 = "a".repeat(64);
        let expected_projection_sha256 = "b".repeat(64);
        let status = |projection_sha256: Option<&str>| {
            let mut value = json!({
                "api_version": "v1",
                "provider": "gateway",
                "topology_id": "primary",
                "observed_revision_id": revision_id,
                "observed_content_sha256": content_sha256,
                "absent": false,
                "endpoints": [],
                "links": []
            });
            if let Some(projection_sha256) = projection_sha256 {
                value["observed_projection_sha256"] = json!(projection_sha256);
            }
            serde_json::from_value::<ProviderStatus>(value)
                .unwrap()
                .verify(ProviderKind::Gateway, "primary")
                .unwrap()
        };

        let matching = status(Some(&expected_projection_sha256));
        assert!(matching.matches(revision_id, &content_sha256, &expected_projection_sha256));
        let legacy_missing = status(None);
        assert_eq!(legacy_missing.state, TopologyProviderObservedState::Present);
        assert!(legacy_missing.observed_projection_sha256.is_none());
        assert!(!legacy_missing.matches(revision_id, &content_sha256, &expected_projection_sha256));
        assert!(!status(Some(&"c".repeat(64))).matches(
            revision_id,
            &content_sha256,
            &expected_projection_sha256
        ));
    }

    #[test]
    fn missing_provider_fails_closed_before_any_apply() {
        let gateway = HttpManagementProviderConfig::new("http://127.0.0.1:1234").unwrap();
        let error = match TopologyProviderSaga::from_config(TopologyProviderConfig::new(
            Some(gateway),
            None,
        )) {
            Ok(_) => panic!("missing Auth provider must fail closed"),
            Err(error) => error,
        };
        assert_eq!(error, TopologyProviderBuildError::MissingAuth);
    }

    #[test]
    fn apply_succeeds_only_after_gateway_and_auth_acknowledge() {
        let gateway = spawn_mock(vec![ExpectedCall {
            provider: "gateway",
            action: "apply",
            method: "PUT",
            status: 200,
        }]);
        let auth = spawn_mock(vec![ExpectedCall {
            provider: "auth",
            action: "apply",
            method: "PUT",
            status: 200,
        }]);
        let saga = saga(&gateway.origin, &auth.origin);
        let receipt = saga
            .apply(
                "primary",
                "primary:r2:0123456789abcdef",
                &topology("desired"),
                Some("primary:r1:fedcba9876543210"),
                Some(&topology("previous")),
                "operation-1",
            )
            .unwrap();
        assert_eq!(receipt.state, TopologyProviderApplyState::Succeeded);
        join_mock(gateway);
        join_mock(auth);
    }

    #[test]
    fn runtime_revocation_is_gateway_first_and_never_restores_the_old_route() {
        let gateway = spawn_mock(vec![ExpectedCall {
            provider: "gateway",
            action: "apply",
            method: "PUT",
            status: 503,
        }]);
        // A revoke implementation that contacted Auth first would fail to
        // reach the Gateway mock and make this test time out.
        let auth = spawn_mock(Vec::new());
        let saga = saga(&gateway.origin, &auth.origin);
        let error = saga
            .apply_runtime_projection(
                "primary",
                "primary:r2:0123456789abcdef",
                &topology("applied"),
                &[],
                "runtime-revoke-1",
                RuntimeProjectionOrder::RevokeFirst,
            )
            .unwrap_err();
        assert!(error.contains("gateway apply returned HTTP 503"));
        join_mock(gateway);
        join_mock(auth);
    }

    #[test]
    fn runtime_restoration_is_auth_first_and_keeps_gateway_denied_on_failure() {
        let gateway = spawn_mock(Vec::new());
        let auth = spawn_mock(vec![ExpectedCall {
            provider: "auth",
            action: "apply",
            method: "PUT",
            status: 503,
        }]);
        let saga = saga(&gateway.origin, &auth.origin);
        let error = saga
            .apply_runtime_projection(
                "primary",
                "primary:r2:0123456789abcdef",
                &topology("applied"),
                &[],
                "runtime-grant-1",
                RuntimeProjectionOrder::GrantFirst,
            )
            .unwrap_err();
        assert!(error.contains("auth apply returned HTTP 503"));
        join_mock(gateway);
        join_mock(auth);
    }

    #[test]
    fn auth_failure_restores_previous_gateway_spec_and_reports_failed() {
        let gateway = spawn_mock(vec![
            ExpectedCall {
                provider: "gateway",
                action: "apply",
                method: "PUT",
                status: 200,
            },
            ExpectedCall {
                provider: "gateway",
                action: "restore_previous",
                method: "PUT",
                status: 200,
            },
        ]);
        let auth = spawn_mock(vec![ExpectedCall {
            provider: "auth",
            action: "apply",
            method: "PUT",
            status: 500,
        }]);
        let saga = saga(&gateway.origin, &auth.origin);
        let failure = saga
            .apply(
                "primary",
                "primary:r2:0123456789abcdef",
                &topology("desired"),
                Some("primary:r1:fedcba9876543210"),
                Some(&topology("previous")),
                "operation-2",
            )
            .unwrap_err();
        assert_eq!(failure.state, TopologyProviderApplyState::Failed);
        assert_eq!(failure.failed_stage, TopologyProviderStage::AuthApply);
        assert_eq!(
            failure.gateway_compensation,
            GatewayCompensation::RestoredPrevious
        );
        join_mock(gateway);
        join_mock(auth);
    }

    #[test]
    fn asynchronous_ack_is_not_mistaken_for_applied_state() {
        let gateway = spawn_mock(vec![
            ExpectedCall {
                provider: "gateway",
                action: "apply",
                method: "PUT",
                status: 202,
            },
            ExpectedCall {
                provider: "gateway",
                action: "restore_previous",
                method: "PUT",
                status: 200,
            },
        ]);
        let auth = spawn_mock(Vec::new());
        let saga = saga(&gateway.origin, &auth.origin);
        let failure = saga
            .apply(
                "primary",
                "primary:r2:0123456789abcdef",
                &topology("desired"),
                Some("primary:r1:fedcba9876543210"),
                Some(&topology("previous")),
                "operation-202",
            )
            .unwrap_err();
        assert_eq!(failure.state, TopologyProviderApplyState::Failed);
        assert_eq!(failure.failed_stage, TopologyProviderStage::GatewayApply);
        assert_eq!(
            failure.gateway_compensation,
            GatewayCompensation::RestoredPrevious
        );
        join_mock(gateway);
        join_mock(auth);
    }

    #[test]
    fn first_revision_auth_failure_deletes_gateway_resource() {
        let gateway = spawn_mock(vec![
            ExpectedCall {
                provider: "gateway",
                action: "apply",
                method: "PUT",
                status: 200,
            },
            ExpectedCall {
                provider: "gateway",
                action: "delete",
                method: "DELETE",
                status: 200,
            },
        ]);
        let auth = spawn_mock(vec![ExpectedCall {
            provider: "auth",
            action: "apply",
            method: "PUT",
            status: 503,
        }]);
        let saga = saga(&gateway.origin, &auth.origin);
        let failure = saga
            .apply(
                "primary",
                "primary:r1:0123456789abcdef",
                &topology("desired"),
                None,
                None,
                "operation-3",
            )
            .unwrap_err();
        assert_eq!(failure.state, TopologyProviderApplyState::Failed);
        assert_eq!(
            failure.gateway_compensation,
            GatewayCompensation::DeletedTopology
        );
        join_mock(gateway);
        join_mock(auth);
    }

    #[test]
    fn unknown_auth_result_compensates_both_auth_and_gateway() {
        let gateway = spawn_mock(vec![
            ExpectedCall {
                provider: "gateway",
                action: "apply",
                method: "PUT",
                status: 200,
            },
            ExpectedCall {
                provider: "gateway",
                action: "restore_previous",
                method: "PUT",
                status: 200,
            },
        ]);
        let auth = spawn_mock(vec![
            ExpectedCall {
                provider: "auth",
                action: "apply",
                method: "PUT",
                status: 202,
            },
            ExpectedCall {
                provider: "auth",
                action: "restore_previous",
                method: "PUT",
                status: 200,
            },
        ]);
        let saga = saga(&gateway.origin, &auth.origin);
        let failure = saga
            .apply(
                "primary",
                "primary:r2:0123456789abcdef",
                &topology("desired"),
                Some("primary:r1:fedcba9876543210"),
                Some(&topology("previous")),
                "operation-auth-unknown",
            )
            .unwrap_err();
        assert_eq!(failure.state, TopologyProviderApplyState::Failed);
        assert_eq!(
            failure.auth_compensation,
            AuthCompensation::RestoredPrevious
        );
        assert_eq!(
            failure.gateway_compensation,
            GatewayCompensation::RestoredPrevious
        );
        join_mock(gateway);
        join_mock(auth);
    }

    #[test]
    fn failed_gateway_compensation_reports_degraded() {
        let gateway = spawn_mock(vec![
            ExpectedCall {
                provider: "gateway",
                action: "apply",
                method: "PUT",
                status: 200,
            },
            ExpectedCall {
                provider: "gateway",
                action: "restore_previous",
                method: "PUT",
                status: 500,
            },
        ]);
        let auth = spawn_mock(vec![ExpectedCall {
            provider: "auth",
            action: "apply",
            method: "PUT",
            status: 500,
        }]);
        let saga = saga(&gateway.origin, &auth.origin);
        let failure = saga
            .apply(
                "primary",
                "primary:r2:0123456789abcdef",
                &topology("desired"),
                Some("primary:r1:fedcba9876543210"),
                Some(&topology("previous")),
                "operation-4",
            )
            .unwrap_err();
        assert_eq!(failure.state, TopologyProviderApplyState::Degraded);
        assert_eq!(
            failure.failed_stage,
            TopologyProviderStage::GatewayCompensation
        );
        assert_eq!(failure.gateway_compensation, GatewayCompensation::Failed);
        join_mock(gateway);
        join_mock(auth);
    }

    #[test]
    fn observe_requires_fresh_matching_state_from_both_providers() {
        let revision_id = "primary:r2:0123456789abcdef";
        let content_sha256 = topology("desired").content_sha256().unwrap();
        let projection_sha256 = provider_projection_sha256(&[]).unwrap();
        let gateway = spawn_observe_mock(
            "gateway",
            200,
            json!({
                "api_version": "v1",
                "provider": "gateway",
                "topology_id": "primary",
                "observed_revision_id": revision_id,
                "observed_content_sha256": content_sha256.clone(),
                "observed_projection_sha256": projection_sha256.clone(),
                "absent": false,
                "endpoints": [{
                    "endpoint": "127.0.0.1:8080:gateway",
                    "health": "HEALTHY",
                    "reachable": true,
                    "latency_ms": 3,
                    "message": "",
                    "observed_at": "unix-ms:1"
                }],
                "links": [{
                    "source_endpoint": "127.0.0.1:8080:gateway",
                    "target_endpoint": "127.0.0.1:8083:problem-service",
                    "health": "HEALTHY",
                    "latency_ms": 4,
                    "message": "",
                    "observed_at": "unix-ms:1"
                }],
            }),
        );
        let auth = spawn_observe_mock(
            "auth",
            200,
            json!({
                "api_version": "v1",
                "provider": "auth",
                "topology_id": "primary",
                "observed_revision_id": revision_id,
                "observed_content_sha256": content_sha256,
                "observed_projection_sha256": projection_sha256,
                "absent": false,
                "links": [{
                    "source_endpoint": "127.0.0.1:8080:gateway",
                    "target_endpoint": "127.0.0.1:8083:problem-service",
                    "health": "HEALTHY",
                    "latency_ms": 2,
                    "message": "",
                    "observed_at": "unix-ms:1"
                }],
            }),
        );
        let observation = saga(&gateway.origin, &auth.origin).observe("primary");
        assert!(
            observation
                .gateway
                .matches(revision_id, &content_sha256, &projection_sha256)
        );
        assert!(
            observation
                .auth
                .matches(revision_id, &content_sha256, &projection_sha256)
        );
        assert_eq!(observation.gateway.endpoints.len(), 1);
        assert_eq!(observation.gateway.links.len(), 1);
        assert_eq!(observation.auth.links.len(), 1);
        join_mock(gateway);
        join_mock(auth);
    }

    #[test]
    fn observe_accepts_present_projection_with_empty_status_arrays() {
        let revision_id = "primary:r1:0123456789abcdef";
        let content_sha256 = topology("empty bindings").content_sha256().unwrap();
        let projection_sha256 = provider_projection_sha256(&[]).unwrap();
        let status = |provider| {
            json!({
                "api_version": "v1",
                "provider": provider,
                "topology_id": "primary",
                "observed_revision_id": revision_id,
                "observed_content_sha256": content_sha256,
                "observed_projection_sha256": projection_sha256,
                "absent": false,
                "endpoints": [],
                "links": [],
            })
        };
        let gateway = spawn_observe_mock("gateway", 200, status("gateway"));
        let auth = spawn_observe_mock("auth", 200, status("auth"));

        let observation = saga(&gateway.origin, &auth.origin).observe("primary");
        assert!(
            observation
                .gateway
                .matches(revision_id, &content_sha256, &projection_sha256)
        );
        assert!(
            observation
                .auth
                .matches(revision_id, &content_sha256, &projection_sha256)
        );
        assert!(observation.gateway.endpoints.is_empty());
        assert!(observation.gateway.links.is_empty());
        assert!(observation.auth.endpoints.is_empty());
        assert!(observation.auth.links.is_empty());
        join_mock(gateway);
        join_mock(auth);
    }

    #[test]
    fn observe_keeps_provider_mismatch_and_unreachable_state_explicit() {
        let gateway = spawn_observe_mock(
            "gateway",
            200,
            json!({
                "api_version": "v1",
                "provider": "gateway",
                "topology_id": "primary",
                "observed_revision_id": "primary:r1:old",
                "observed_content_sha256": "a".repeat(64),
                "observed_projection_sha256": "c".repeat(64),
                "absent": false,
            }),
        );
        let auth = spawn_observe_mock(
            "auth",
            503,
            json!({"code": "UNAVAILABLE", "detail": "try later"}),
        );
        let observation = saga(&gateway.origin, &auth.origin).observe("primary");
        assert_eq!(
            observation.gateway.state,
            TopologyProviderObservedState::Present
        );
        assert!(
            !observation
                .gateway
                .matches("primary:r2:new", &"b".repeat(64), &"d".repeat(64))
        );
        assert_eq!(
            observation.auth.state,
            TopologyProviderObservedState::Unreachable
        );
        assert!(observation.auth.detail.contains("HTTP 503"));
        join_mock(gateway);
        join_mock(auth);
    }

    fn spawn_mock(expected_calls: Vec<ExpectedCall>) -> MockProvider {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind provider mock");
        listener
            .set_nonblocking(true)
            .expect("make provider mock nonblocking");
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let thread = thread::spawn(move || {
            for expected in expected_calls {
                let mut stream = accept_before(&listener, Instant::now() + Duration::from_secs(3));
                stream
                    .set_read_timeout(Some(Duration::from_secs(2)))
                    .expect("set provider mock read timeout");
                let request = read_request(&mut stream);
                assert_eq!(request.method, expected.method);
                assert_eq!(request.path, "/api/v1/topologies/primary");
                assert_eq!(
                    request.headers.get("content-type").map(String::as_str),
                    Some("application/json")
                );
                let body: Value = serde_json::from_slice(&request.body).expect("strict JSON body");
                assert_eq!(body["api_version"], "v1");
                assert_eq!(body["provider"], expected.provider);
                assert_eq!(body["action"], expected.action);
                assert_eq!(body["topology_id"], "primary");
                assert!(body["attempted_revision_id"].as_str().is_some());
                assert!(body["operation_id"].as_str().is_some());
                if expected.action == "delete" {
                    assert!(body.get("spec").is_none());
                    assert!(body.get("desired_revision_id").is_none());
                    assert!(body.get("desired_content_sha256").is_none());
                } else if expected.action == "restore_previous" {
                    assert!(body["spec"].is_object());
                    assert_ne!(body["desired_revision_id"], body["attempted_revision_id"]);
                    assert!(
                        body["desired_revision_id"]
                            .as_str()
                            .is_some_and(|revision| revision.contains(":r1:"))
                    );
                    assert!(body["desired_content_sha256"].as_str().is_some());
                } else {
                    assert!(body["spec"].is_object());
                    assert_eq!(body["desired_revision_id"], body["attempted_revision_id"]);
                    assert!(body["desired_content_sha256"].as_str().is_some());
                }
                let expected_key = format!(
                    "{}:{}:{}",
                    body["operation_id"].as_str().unwrap(),
                    expected.provider,
                    expected.action
                );
                assert_eq!(request.headers.get("idempotency-key"), Some(&expected_key));
                write_response(
                    &mut stream,
                    expected.status,
                    expected.provider,
                    expected.action,
                    &body,
                );
            }
        });
        MockProvider { origin, thread }
    }

    fn spawn_observe_mock(provider: &'static str, status: u16, body: Value) -> MockProvider {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind provider mock");
        listener
            .set_nonblocking(true)
            .expect("make provider mock nonblocking");
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let thread = thread::spawn(move || {
            let mut stream = accept_before(&listener, Instant::now() + Duration::from_secs(3));
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .expect("set provider mock read timeout");
            let request = read_request(&mut stream);
            assert_eq!(request.method, "GET");
            assert_eq!(request.path, "/api/v1/topologies/primary");
            assert_eq!(
                request.headers.get("accept").map(String::as_str),
                Some("application/json")
            );
            assert!(request.body.is_empty());
            let body = body.to_string();
            let reason = if status == 200 {
                "OK"
            } else {
                "Service Unavailable"
            };
            write!(
                stream,
                "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\nX-Mock-Provider: {provider}\r\n\r\n{body}",
                body.len()
            )
            .expect("write provider observation");
            stream.flush().expect("flush provider observation");
        });
        MockProvider { origin, thread }
    }

    fn join_mock(mock_provider: MockProvider) {
        mock_provider
            .thread
            .join()
            .expect("provider mock completed");
    }

    fn accept_before(listener: &TcpListener, deadline: Instant) -> TcpStream {
        loop {
            match listener.accept() {
                Ok((stream, _)) => {
                    stream
                        .set_nonblocking(false)
                        .expect("make accepted provider stream blocking");
                    return stream;
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    assert!(Instant::now() < deadline, "provider call was not received");
                    thread::sleep(Duration::from_millis(5));
                }
                Err(error) => panic!("accept provider request: {error}"),
            }
        }
    }

    struct MockRequest {
        method: String,
        path: String,
        headers: BTreeMap<String, String>,
        body: Vec<u8>,
    }

    fn read_request(stream: &mut TcpStream) -> MockRequest {
        const MAX_MOCK_REQUEST: usize = 2 * 1024 * 1024;
        let mut bytes = Vec::new();
        let header_end = loop {
            let mut chunk = [0_u8; 4096];
            let read = stream.read(&mut chunk).expect("read provider request");
            assert!(read > 0, "provider request closed before headers");
            bytes.extend_from_slice(&chunk[..read]);
            assert!(
                bytes.len() <= MAX_MOCK_REQUEST,
                "provider request too large"
            );
            if let Some(index) = find_bytes(&bytes, b"\r\n\r\n") {
                break index + 4;
            }
        };
        let head = std::str::from_utf8(&bytes[..header_end]).expect("ASCII provider request");
        let mut lines = head.split("\r\n");
        let request_line = lines.next().expect("provider request line");
        let mut request_line = request_line.split_whitespace();
        let method = request_line.next().unwrap().to_string();
        let path = request_line.next().unwrap().to_string();
        let headers = lines
            .filter_map(|line| line.split_once(':'))
            .map(|(name, value)| (name.trim().to_ascii_lowercase(), value.trim().to_string()))
            .collect::<BTreeMap<_, _>>();
        let content_length = headers
            .get("content-length")
            .map(|value| {
                value
                    .parse::<usize>()
                    .expect("numeric provider Content-Length")
            })
            .unwrap_or(0);
        while bytes.len() < header_end + content_length {
            let mut chunk = [0_u8; 4096];
            let read = stream.read(&mut chunk).expect("read provider body");
            assert!(read > 0, "provider request closed before body");
            bytes.extend_from_slice(&chunk[..read]);
            assert!(
                bytes.len() <= MAX_MOCK_REQUEST,
                "provider request too large"
            );
        }
        MockRequest {
            method,
            path,
            headers,
            body: bytes[header_end..header_end + content_length].to_vec(),
        }
    }

    fn find_bytes(haystack: &[u8], needle: &[u8]) -> Option<usize> {
        haystack
            .windows(needle.len())
            .position(|window| window == needle)
    }

    fn write_response(
        stream: &mut TcpStream,
        status: u16,
        provider: &str,
        action: &str,
        request: &Value,
    ) {
        let body = if (200..=299).contains(&status) {
            json!({
                "api_version": "v1",
                "provider": provider,
                "action": action,
                "topology_id": request["topology_id"],
                "operation_id": request["operation_id"],
                "completed": true,
                "observed_revision_id": request["desired_revision_id"],
                "observed_content_sha256": request["desired_content_sha256"],
                "absent": action == "delete",
            })
        } else {
            json!({"code": "MOCK_FAILURE", "detail": "provider rejected request"})
        }
        .to_string();
        let reason = if status == 200 {
            "OK"
        } else {
            "Service Unavailable"
        };
        write!(
            stream,
            "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )
        .expect("write provider response");
        stream.flush().expect("flush provider response");
    }
}
