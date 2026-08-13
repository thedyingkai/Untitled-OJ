//! Credential-scoped Prometheus HTTP service discovery.
//!
//! The durable Contribution head is the publication pointer.  Targets are
//! emitted only when that exact ACTIVE revision owns a current, healthy and
//! attested runtime projection.  In particular, container names and
//! deployment-derived DNS names are never used as monitoring identities.

use crate::durable::DurableStore;
use crate::http::ApiRequest;
use anyhow::{Context, Result, anyhow};
use orchestrator_legacy::{
    ContributionRevisionStatusV1, ContributionRevisionV1, endpoint_socket_addr, parse_endpoint_id,
};
use orchestrator_runtime::{RuntimeDesiredState, RuntimeObservedState};
use orchestrator_storage::{ContributionRepository, RuntimeManagementMode, StoredRuntimeInstance};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::net::IpAddr;
use std::path::Path;
use url::Url;

pub(crate) const OBSERVABILITY_TOKEN_FILE_ENV: &str = "ORCHESTRATOR_OBSERVABILITY_TOKEN_FILE";
pub(crate) const OBSERVABILITY_METRICS_PATH: &str = "/internal/v1/observability/metrics";
pub(crate) const METRICS_DISCOVERY_PATH: &str = "/internal/v1/observability/metrics/targets";
pub(crate) const HEALTH_DISCOVERY_PATH: &str = "/internal/v1/observability/health/targets";

const MAX_TOKEN_FILE_BYTES: u64 = 4_096;
const MAX_TOKEN_BYTES: usize = 512;
const CONTRIBUTION_ACK_VERIFIER_ENVS: [&str; 2] = [
    "ORCHESTRATOR_CONTRIBUTION_GATEWAY_ACK_TOKEN_SHA256",
    "ORCHESTRATOR_CONTRIBUTION_AUTH_ACK_TOKEN_SHA256",
];

#[derive(Debug, Clone)]
pub(crate) struct ObservabilityDiscoveryAuth {
    expected_hash: Option<[u8; 32]>,
    gateway_origin: Option<String>,
}

impl ObservabilityDiscoveryAuth {
    pub(crate) fn from_env(production: bool, internal_token: Option<&str>) -> Result<Self> {
        let configured = std::env::var(OBSERVABILITY_TOKEN_FILE_ENV)
            .ok()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty());
        let Some(path) = configured else {
            if production {
                return Err(anyhow!(
                    "production PostgreSQL mode requires {OBSERVABILITY_TOKEN_FILE_ENV}"
                ));
            }
            return Ok(Self {
                expected_hash: None,
                gateway_origin: gateway_observability_origin(production)?,
            });
        };
        let forbidden = [
            internal_token.map(str::to_string),
            env_secret("ORCHESTRATOR_GATEWAY_ADMIN_TOKEN"),
            env_secret("ORCHESTRATOR_AUTH_ADMIN_TOKEN"),
            env_secret("ORCHESTRATOR_AUTH_WORKLOAD_TOKEN"),
        ];
        let mut auth = Self::from_file(Path::new(&path), production, forbidden.iter().flatten())?;
        if let Some(expected_hash) = auth.expected_hash.as_ref() {
            reject_contribution_ack_verifier_reuse(expected_hash)?;
        }
        auth.gateway_origin = gateway_observability_origin(production)?;
        Ok(auth)
    }

    fn from_file<'a>(
        path: &Path,
        production: bool,
        forbidden: impl IntoIterator<Item = &'a String>,
    ) -> Result<Self> {
        if production && !path.is_absolute() {
            return Err(anyhow!(
                "{OBSERVABILITY_TOKEN_FILE_ENV} must name an absolute file"
            ));
        }
        let metadata = fs::symlink_metadata(path).with_context(|| {
            format!(
                "inspect the dedicated observability token file {}",
                path.display()
            )
        })?;
        if metadata.file_type().is_symlink() || !metadata.file_type().is_file() {
            return Err(anyhow!(
                "{OBSERVABILITY_TOKEN_FILE_ENV} must name a regular non-symlink file"
            ));
        }
        if metadata.len() == 0 || metadata.len() > MAX_TOKEN_FILE_BYTES {
            return Err(anyhow!(
                "{OBSERVABILITY_TOKEN_FILE_ENV} must contain 32..={MAX_TOKEN_BYTES} bytes"
            ));
        }
        #[cfg(unix)]
        if production {
            use std::os::unix::fs::PermissionsExt;
            if metadata.permissions().mode() & 0o077 != 0 {
                return Err(anyhow!(
                    "{OBSERVABILITY_TOKEN_FILE_ENV} must not be readable by group or other users"
                ));
            }
        }
        let raw = fs::read(path).with_context(|| {
            format!(
                "read the dedicated observability token file {}",
                path.display()
            )
        })?;
        let token = std::str::from_utf8(&raw)
            .context("the dedicated observability token file must be UTF-8")?
            .trim_end_matches(['\r', '\n']);
        validate_token(token)?;
        for other in forbidden {
            if constant_time_hash_eq(&hash(token), &hash(other)) {
                return Err(anyhow!(
                    "the observability token must not reuse a control-plane, admin, ACK, or workload credential"
                ));
            }
        }
        Ok(Self {
            expected_hash: Some(hash(token)),
            gateway_origin: None,
        })
    }

    /// Non-production embedded servers keep the historic loopback-only
    /// metrics behavior unless an explicit token file is configured.
    pub(crate) fn authorize(&self, request: &ApiRequest) -> bool {
        let Some(expected) = self.expected_hash else {
            return true;
        };
        let Some(header) = request.headers.get("authorization") else {
            return false;
        };
        let Some(token) = header.strip_prefix("Bearer ") else {
            return false;
        };
        validate_token(token).is_ok() && constant_time_hash_eq(&expected, &hash(token))
    }

    pub(crate) fn platform_targets(
        &self,
        kind: DiscoveryKind,
        discovered: &[HttpSdGroup],
    ) -> Vec<HttpSdGroup> {
        if discovered
            .iter()
            .any(|group| group.labels.get("service").map(String::as_str) == Some("gateway"))
        {
            return Vec::new();
        }
        let Some(origin) = self.gateway_origin.as_deref() else {
            return Vec::new();
        };
        let suffix = match kind {
            DiscoveryKind::Metrics => "/metrics",
            DiscoveryKind::Health => "/readyz",
        };
        let target = if kind == DiscoveryKind::Metrics {
            origin
                .strip_prefix("https://")
                .expect("validated Gateway observability origin")
                .to_string()
        } else {
            format!("{origin}{suffix}")
        };
        let mut labels = BTreeMap::from([
            ("service".to_string(), "gateway".to_string()),
            ("platform".to_string(), "gateway".to_string()),
            ("source".to_string(), "explicit-platform".to_string()),
        ]);
        if kind == DiscoveryKind::Metrics {
            labels.insert("__scheme__".to_string(), "https".to_string());
            labels.insert("__metrics_path__".to_string(), suffix.to_string());
        }
        vec![HttpSdGroup {
            targets: vec![target],
            labels,
        }]
    }
}

fn gateway_observability_origin(production: bool) -> Result<Option<String>> {
    let configured = std::env::var("ORCHESTRATOR_GATEWAY_OBSERVABILITY_ORIGIN")
        .ok()
        .map(|value| value.trim_end_matches('/').trim().to_string())
        .filter(|value| !value.is_empty());
    let Some(origin) = configured else {
        if production {
            return Err(anyhow!(
                "production PostgreSQL mode requires ORCHESTRATOR_GATEWAY_OBSERVABILITY_ORIGIN"
            ));
        }
        return Ok(None);
    };
    let parsed = Url::parse(&origin)
        .context("ORCHESTRATOR_GATEWAY_OBSERVABILITY_ORIGIN must be an absolute HTTPS origin")?;
    let host = parsed
        .host_str()
        .ok_or_else(|| anyhow!("Gateway observability origin has no host"))?;
    let legacy_names = [
        "gateway",
        "auth-service",
        "judge-api",
        "problem-service",
        "storage-service",
        "user-service",
    ];
    if parsed.scheme() != "https"
        || parsed.username() != ""
        || parsed.password().is_some()
        || parsed.query().is_some()
        || parsed.fragment().is_some()
        || !matches!(parsed.path(), "" | "/")
        || legacy_names.contains(&host.to_ascii_lowercase().as_str())
        || host
            .parse::<IpAddr>()
            .is_ok_and(|ip| ip.is_loopback() || ip.is_unspecified() || ip.is_multicast())
    {
        return Err(anyhow!(
            "ORCHESTRATOR_GATEWAY_OBSERVABILITY_ORIGIN must be a credential-free external HTTPS origin, not a legacy Compose DNS name"
        ));
    }
    Ok(Some(origin))
}

fn env_secret(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn reject_contribution_ack_verifier_reuse(observability_hash: &[u8; 32]) -> Result<()> {
    for name in CONTRIBUTION_ACK_VERIFIER_ENVS {
        let Some(value) = env_secret(name) else {
            continue;
        };
        let verifier = parse_sha256_verifier(name, &value)?;
        if constant_time_hash_eq(observability_hash, &verifier) {
            return Err(anyhow!(
                "the observability token must not reuse a Contribution ACK credential ({name})"
            ));
        }
    }
    Ok(())
}

fn parse_sha256_verifier(name: &str, value: &str) -> Result<[u8; 32]> {
    let hex = value
        .strip_prefix("sha256:")
        .ok_or_else(|| anyhow!("{name} must be a canonical sha256:<64 lowercase hex> verifier"))?;
    if hex.len() != 64
        || !hex
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(anyhow!(
            "{name} must be a canonical sha256:<64 lowercase hex> verifier"
        ));
    }
    let mut decoded = [0_u8; 32];
    for (index, pair) in hex.as_bytes().chunks_exact(2).enumerate() {
        decoded[index] = (hex_nibble(pair[0]) << 4) | hex_nibble(pair[1]);
    }
    Ok(decoded)
}

fn hex_nibble(byte: u8) -> u8 {
    match byte {
        b'0'..=b'9' => byte - b'0',
        b'a'..=b'f' => byte - b'a' + 10,
        _ => unreachable!("canonical verifier was validated"),
    }
}

fn validate_token(token: &str) -> Result<()> {
    if token.len() < 32
        || token.len() > MAX_TOKEN_BYTES
        || token.trim() != token
        || !token.bytes().all(|byte| (0x21..=0x7e).contains(&byte))
    {
        return Err(anyhow!(
            "the observability token must be 32..={MAX_TOKEN_BYTES} visible ASCII bytes without surrounding whitespace"
        ));
    }
    Ok(())
}

fn hash(value: &str) -> [u8; 32] {
    Sha256::digest(value.as_bytes()).into()
}

fn constant_time_hash_eq(left: &[u8; 32], right: &[u8; 32]) -> bool {
    left.iter()
        .zip(right)
        .fold(0_u8, |difference, (left, right)| {
            difference | (left ^ right)
        })
        == 0
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DiscoveryKind {
    Metrics,
    Health,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub(crate) struct HttpSdGroup {
    targets: Vec<String>,
    labels: BTreeMap<String, String>,
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum DiscoveryError {
    #[error("observability discovery storage failure: {0}")]
    Storage(String),
    #[error("observability discovery invariant failed")]
    Invariant,
}

/// Prometheus retains its previous target set when HTTP SD returns an error.
/// A safe, deliberately failing sentinel therefore closes the target set on
/// invariant/storage failure and makes the failure alertable without exposing
/// the underlying topology detail.
pub(crate) fn fail_closed_groups(kind: DiscoveryKind) -> Vec<HttpSdGroup> {
    let mut labels = BTreeMap::from([
        ("service".to_string(), "observability-discovery".to_string()),
        ("source".to_string(), "orchestrator-http-sd".to_string()),
        ("discovery_error".to_string(), "true".to_string()),
    ]);
    let target = match kind {
        DiscoveryKind::Metrics => {
            labels.insert("__scheme__".to_string(), "http".to_string());
            labels.insert("__metrics_path__".to_string(), "/metrics".to_string());
            "127.0.0.1:1".to_string()
        }
        DiscoveryKind::Health => "http://127.0.0.1:1/".to_string(),
    };
    vec![HttpSdGroup {
        targets: vec![target],
        labels,
    }]
}

pub(crate) fn active_targets(
    storage: &DurableStore,
    scope_id: &str,
    kind: DiscoveryKind,
    now_ms: i64,
) -> Result<Vec<HttpSdGroup>, DiscoveryError> {
    let revisions = active_revisions(storage, scope_id)?;
    let mut targets = Vec::new();
    let mut sockets = BTreeSet::new();
    for revision in revisions {
        let Some(runtime) = current_ready_runtime(storage, &revision, now_ms)? else {
            continue;
        };
        let Some(target) = target_for_revision(storage, &revision, &runtime, kind)? else {
            continue;
        };
        let socket =
            endpoint_socket_addr(&runtime.endpoint).map_err(|_| DiscoveryError::Invariant)?;
        if !sockets.insert(socket) {
            return Err(DiscoveryError::Invariant);
        }
        targets.push(target);
    }
    targets.sort_by(|left, right| {
        left.labels
            .get("service")
            .cmp(&right.labels.get("service"))
            .then(left.targets.cmp(&right.targets))
    });
    Ok(targets)
}

fn active_revisions(
    storage: &DurableStore,
    scope_id: &str,
) -> Result<Vec<ContributionRevisionV1>, DiscoveryError> {
    let revisions = storage
        .contribution_revisions(scope_id, None)
        .map_err(|error| DiscoveryError::Storage(error.to_string()))?;
    let mut active = Vec::new();
    for revision in revisions.into_iter().filter(|revision| {
        revision.status() == ContributionRevisionStatusV1::Active
            && has_observable_contribution(revision)
    }) {
        let head = storage
            .contribution_head(scope_id, revision.service_id())
            .map_err(|error| DiscoveryError::Storage(error.to_string()))?;
        if head.as_ref().map(|head| head.active_revision_id()) == Some(revision.revision_id()) {
            active.push(revision);
        }
    }
    active.sort_by(|left, right| {
        left.service_id()
            .cmp(right.service_id())
            .then(left.generation().cmp(&right.generation()))
    });
    Ok(active)
}

fn has_observable_contribution(revision: &ContributionRevisionV1) -> bool {
    !revision.api_surfaces().is_empty()
        || !revision.operation_routes().is_empty()
        || !revision.permission_definitions().is_empty()
        || !revision.user_frontend_modules().is_empty()
        || !revision.admin_frontend_modules().is_empty()
}

fn current_ready_runtime(
    storage: &DurableStore,
    revision: &ContributionRevisionV1,
    now_ms: i64,
) -> Result<Option<StoredRuntimeInstance>, DiscoveryError> {
    let Some(runtime) = storage
        .runtime_instance(revision.deployment_id())
        .map_err(|error| DiscoveryError::Storage(error.to_string()))?
    else {
        return Ok(None);
    };
    let runtime = storage
        .runtime_with_current_evidence(runtime, now_ms)
        .map_err(|error| DiscoveryError::Storage(error.to_string()))?;
    let ready = runtime.instance.deployment_id == revision.deployment_id()
        && runtime.instance.service_id == revision.service_id()
        && runtime.instance.artifact_digest == revision.release_digest()
        && runtime.instance.desired_state == RuntimeDesiredState::Running
        && runtime.instance.observed_state == RuntimeObservedState::Running
        && runtime.instance.health.eq_ignore_ascii_case("HEALTHY")
        && runtime.drift_reason.trim().is_empty()
        && (runtime.management_mode == RuntimeManagementMode::External
            || runtime.instance.runtime_attested);
    Ok(ready.then_some(runtime))
}

fn target_for_revision(
    storage: &DurableStore,
    revision: &ContributionRevisionV1,
    runtime: &StoredRuntimeInstance,
    kind: DiscoveryKind,
) -> Result<Option<HttpSdGroup>, DiscoveryError> {
    let contract = storage
        .service_release_contract(revision.service_id(), &runtime.instance.release_version)
        .map_err(|error| DiscoveryError::Storage(error.to_string()))?
        .ok_or(DiscoveryError::Invariant)?;
    if contract.release.service_name != revision.service_id()
        || contract
            .platform
            .as_ref()
            .map(|platform| platform.contract_digest.as_str())
            != Some(revision.contract_digest())
    {
        return Err(DiscoveryError::Invariant);
    }
    if kind == DiscoveryKind::Metrics && !contract.release.observability.metrics {
        return Ok(None);
    }
    let protocol = contract
        .release
        .backend
        .protocol
        .trim()
        .to_ascii_lowercase();
    if !matches!(protocol.as_str(), "http" | "https") {
        return Err(DiscoveryError::Invariant);
    }
    if runtime.management_mode == RuntimeManagementMode::External
        && (!runtime
            .external_probe_protocol
            .eq_ignore_ascii_case(&protocol)
            || runtime.external_probe_health_path != contract.release.backend.health_path)
    {
        return Err(DiscoveryError::Invariant);
    }
    let identity = parse_endpoint_id(&runtime.endpoint).map_err(|_| DiscoveryError::Invariant)?;
    if identity.service_name != revision.service_id() {
        return Err(DiscoveryError::Invariant);
    }
    let host = identity
        .host
        .parse::<IpAddr>()
        .map_err(|_| DiscoveryError::Invariant)?;
    if host.is_loopback() || host.is_unspecified() || host.is_multicast() {
        return Err(DiscoveryError::Invariant);
    }
    let port = identity
        .port
        .parse::<u16>()
        .map_err(|_| DiscoveryError::Invariant)?;
    if port == 0 {
        return Err(DiscoveryError::Invariant);
    }
    let socket = endpoint_socket_addr(&runtime.endpoint).map_err(|_| DiscoveryError::Invariant)?;
    let health_path = contract.release.backend.health_path.as_str();
    if health_path.is_empty()
        || !health_path.starts_with('/')
        || health_path.len() > 512
        || health_path.contains(['?', '#'])
        || health_path.chars().any(char::is_control)
    {
        return Err(DiscoveryError::Invariant);
    }
    let mut labels = BTreeMap::from([
        ("service".to_string(), revision.service_id().to_string()),
        (
            "deployment_id".to_string(),
            revision.deployment_id().to_string(),
        ),
        (
            "contribution_revision".to_string(),
            revision.revision_id().to_string(),
        ),
        (
            "contribution_generation".to_string(),
            revision.generation().to_string(),
        ),
        ("source".to_string(), "active-contribution".to_string()),
    ]);
    let target = match kind {
        DiscoveryKind::Metrics => {
            labels.insert("__scheme__".to_string(), protocol);
            labels.insert("__metrics_path__".to_string(), "/metrics".to_string());
            socket
        }
        DiscoveryKind::Health => format!("{protocol}://{socket}{health_path}"),
    };
    Ok(Some(HttpSdGroup {
        targets: vec![target],
        labels,
    }))
}

/// State gauge is emitted by the already-protected Orchestrator metrics
/// endpoint.  An unhealthy active runtime is deliberately absent from HTTP SD
/// but remains alertable here.
pub(crate) fn render_target_readiness(storage: &DurableStore, now_ms: i64) -> String {
    let mut output = "# HELP ojos_orchestrator_observability_discovery_collection_error Whether active Contribution monitoring discovery could be evaluated.\n# TYPE ojos_orchestrator_observability_discovery_collection_error gauge\n".to_string();
    let revisions = match active_revisions(storage, "default") {
        Ok(revisions) => revisions,
        Err(_) => {
            output.push_str("ojos_orchestrator_observability_discovery_collection_error 1\n");
            return output;
        }
    };
    output.push_str("ojos_orchestrator_observability_discovery_collection_error 0\n# HELP ojos_orchestrator_observability_target_ready Whether the active Contribution has a current healthy runtime and valid signed monitoring target.\n# TYPE ojos_orchestrator_observability_target_ready gauge\n");
    for revision in revisions {
        let ready = current_ready_runtime(storage, &revision, now_ms)
            .ok()
            .flatten()
            .and_then(|runtime| {
                target_for_revision(storage, &revision, &runtime, DiscoveryKind::Health)
                    .ok()
                    .flatten()
            })
            .is_some();
        output.push_str(&format!(
            "ojos_orchestrator_observability_target_ready{{service=\"{}\",deployment_id=\"{}\",contribution_revision=\"{}\",contribution_generation=\"{}\"}} {}\n",
            prometheus_escape(revision.service_id()),
            prometheus_escape(revision.deployment_id()),
            prometheus_escape(revision.revision_id()),
            revision.generation(),
            u8::from(ready),
        ));
    }
    output
}

fn prometheus_escape(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('\n', "\\n")
        .replace('"', "\\\"")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_env::TestEnv;
    use std::io::Write;

    fn request(token: Option<&str>) -> ApiRequest {
        let mut headers = BTreeMap::new();
        if let Some(token) = token {
            headers.insert("authorization".to_string(), format!("Bearer {token}"));
        }
        ApiRequest {
            method: "GET".to_string(),
            path: METRICS_DISCOVERY_PATH.to_string(),
            headers,
            body: String::new(),
        }
    }

    #[test]
    fn dedicated_file_token_is_exact_and_cannot_reuse_forbidden_credential() {
        let token = "ObservabilityOnly_0123456789abcdef0123456789";
        let mut file = tempfile::NamedTempFile::new().unwrap();
        writeln!(file, "{token}").unwrap();
        let auth = ObservabilityDiscoveryAuth::from_file(file.path(), false, [].iter()).unwrap();
        assert!(auth.authorize(&request(Some(token))));
        assert!(!auth.authorize(&request(None)));
        assert!(!auth.authorize(&request(Some(
            "ObservabilityOnly_0123456789abcdef0123456780"
        ))));
        assert!(
            ObservabilityDiscoveryAuth::from_file(file.path(), false, [token.to_string()].iter())
                .is_err()
        );
    }

    #[test]
    fn startup_rejects_observability_token_reused_by_ack_verifier() {
        let token = "ObservabilityOnly_0123456789abcdef0123456789";
        let mut file = tempfile::NamedTempFile::new().unwrap();
        writeln!(file, "{token}").unwrap();
        let mut environment = TestEnv::lock();
        environment.set(OBSERVABILITY_TOKEN_FILE_ENV, file.path().to_str().unwrap());
        environment.set(
            CONTRIBUTION_ACK_VERIFIER_ENVS[0],
            &format!("sha256:{:x}", Sha256::digest(token.as_bytes())),
        );
        environment.remove(CONTRIBUTION_ACK_VERIFIER_ENVS[1]);
        environment.remove("ORCHESTRATOR_GATEWAY_OBSERVABILITY_ORIGIN");

        let error = ObservabilityDiscoveryAuth::from_env(false, None).unwrap_err();
        assert!(error.to_string().contains("Contribution ACK credential"));
    }

    #[test]
    fn startup_rejects_noncanonical_ack_verifier() {
        let token = "ObservabilityOnly_0123456789abcdef0123456789";
        let mut file = tempfile::NamedTempFile::new().unwrap();
        writeln!(file, "{token}").unwrap();
        let mut environment = TestEnv::lock();
        environment.set(OBSERVABILITY_TOKEN_FILE_ENV, file.path().to_str().unwrap());
        environment.set(
            CONTRIBUTION_ACK_VERIFIER_ENVS[0],
            &format!("SHA256:{:X}", Sha256::digest(b"another credential")),
        );
        environment.remove(CONTRIBUTION_ACK_VERIFIER_ENVS[1]);
        environment.remove("ORCHESTRATOR_GATEWAY_OBSERVABILITY_ORIGIN");

        let error = ObservabilityDiscoveryAuth::from_env(false, None).unwrap_err();
        assert!(error.to_string().contains("canonical sha256"));
    }

    #[test]
    fn fail_closed_groups_never_reuse_a_previous_real_target() {
        let metrics = fail_closed_groups(DiscoveryKind::Metrics);
        assert_eq!(metrics[0].targets, ["127.0.0.1:1"]);
        assert_eq!(
            metrics[0].labels.get("discovery_error").map(String::as_str),
            Some("true")
        );
        let health = fail_closed_groups(DiscoveryKind::Health);
        assert_eq!(health[0].targets, ["http://127.0.0.1:1/"]);
    }

    #[test]
    fn prometheus_labels_are_escaped() {
        assert_eq!(prometheus_escape("a\\\"b\n"), "a\\\\\\\"b\\n");
    }
}
