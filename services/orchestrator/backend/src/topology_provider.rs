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
