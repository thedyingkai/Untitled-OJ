//! Background external health responsibilities.
use crate::durable::DurableStore;
use crate::topology_worker::context::{now_marker, now_ms};
use crate::topology_worker::payload::ExternalHealthPayload;
use orchestrator_control_plane::CompletionStatus;
use orchestrator_legacy::Endpoint;
use orchestrator_legacy::EndpointProbe;
use orchestrator_legacy::TcpEndpointProbe;
use orchestrator_runtime::RuntimeDesiredState;
use orchestrator_runtime::RuntimeInstance;
use orchestrator_runtime::RuntimeObservedState;
use orchestrator_storage::RuntimeManagementMode;
use orchestrator_storage::StoredRuntimeInstance;
use serde_json::Value;
use std::net::TcpStream;
use std::net::ToSocketAddrs;
use std::time::Duration;

pub(super) const EXTERNAL_REPROBE_INTERVAL_MS: i64 = 30_000;

#[derive(Debug)]
pub(super) struct ExternalHealthFailure {
    pub(super) status: CompletionStatus,
    pub(super) code: &'static str,
    pub(super) detail: String,
}

pub(super) fn process_external_health(
    storage: &DurableStore,
    payload: &Value,
) -> Result<Value, ExternalHealthFailure> {
    let payload: ExternalHealthPayload =
        serde_json::from_value(payload.clone()).map_err(|error| ExternalHealthFailure {
            status: CompletionStatus::Failed,
            code: "INVALID_EXTERNAL_HEALTH_PAYLOAD",
            detail: format!("invalid External health payload: {error}"),
        })?;
    if payload.deployment_id.trim().is_empty()
        || payload.service_id.trim().is_empty()
        || payload.endpoint.trim().is_empty()
        || payload.protocol.trim().is_empty()
        || semver::Version::parse(payload.version.trim()).is_err()
        || orchestrator_runtime::OciImageReference::parse(&payload.artifact_digest).is_err()
    {
        return Err(ExternalHealthFailure {
            status: CompletionStatus::Failed,
            code: "INVALID_EXTERNAL_HEALTH_PAYLOAD",
            detail: "External health payload requires a deployment, service, semver, endpoint and immutable OCI digest"
                .to_string(),
        });
    }
    let existing = storage
        .runtime_instance(&payload.deployment_id)
        .map_err(external_storage_failure)?;
    if let Some(existing) = existing.as_ref() {
        if existing.management_mode == RuntimeManagementMode::External
            && existing.endpoint == payload.endpoint
            && existing.instance.service_id == payload.service_id
            && existing.instance.release_version == payload.version
            && existing.instance.artifact_digest == payload.artifact_digest
            && (existing.external_probe_protocol.is_empty()
                || existing.external_probe_protocol == payload.protocol)
            && (existing.external_probe_health_path.is_empty()
                || existing.external_probe_health_path == payload.health_path)
        {
            // A replay is a new observation, never a cache hit. Continue to
            // the real protocol probe below and atomically replace evidence.
        } else {
            return Err(ExternalHealthFailure {
                status: CompletionStatus::NeedsAttention,
                code: "EXTERNAL_DEPLOYMENT_CONFLICT",
                detail: format!(
                    "deployment {} already has a different runtime projection",
                    payload.deployment_id
                ),
            });
        }
    }

    let probe_failure;
    let evidence = match probe_external_endpoint(&payload) {
        Ok(evidence) => {
            probe_failure = None;
            evidence
        }
        Err(failure) if existing.is_some() => {
            let detail = failure.detail.clone();
            probe_failure = Some(failure);
            serde_json::json!({
                "healthy": false,
                "reachable": false,
                "health": "unhealthy",
                "latency_ms": Value::Null,
                "message": detail,
                "endpoint": payload.endpoint,
                "protocol": payload.protocol,
            })
        }
        Err(failure) => return Err(failure),
    };
    let healthy = evidence
        .get("healthy")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if existing.is_none() && !healthy {
        return Err(external_unhealthy_failure(&evidence));
    }
    let stored = existing.unwrap_or_else(|| StoredRuntimeInstance {
        node_id: "external".to_string(),
        instance: RuntimeInstance {
            deployment_id: payload.deployment_id.clone(),
            service_id: payload.service_id.clone(),
            release_version: payload.version.clone(),
            container_id: String::new(),
            artifact_digest: payload.artifact_digest.clone(),
            runtime_contract: orchestrator_runtime::RuntimeContract::standard_v1(),
            runtime_policy_sha256: String::new(),
            effective_runtime_sha256: String::new(),
            runtime_attested: false,
            desired_state: RuntimeDesiredState::Running,
            observed_state: RuntimeObservedState::Unknown,
            health: "UNKNOWN".to_string(),
        },
        management_mode: RuntimeManagementMode::External,
        endpoint: payload.endpoint.clone(),
        external_probe_protocol: payload.protocol.clone(),
        external_probe_health_path: payload.health_path.clone(),
        last_observed_at_ms: 0,
        drift_reason: String::new(),
        credential_expires_at_ms: 0,
        credential_last_success_at_ms: 0,
        credential_last_error: String::new(),
        updated_at: now_marker(),
    });
    let stored = persist_external_probe_evidence(storage, stored, &payload, &evidence)?;
    if !healthy {
        return Err(probe_failure.unwrap_or_else(|| external_unhealthy_failure(&evidence)));
    }
    Ok(serde_json::json!({
        "instance": stored,
        "health": evidence,
        "version": payload.version,
    }))
}

pub(super) fn refresh_external_runtime_health(storage: &DurableStore) -> Result<(), String> {
    let scan_at_ms = now_ms();
    let external = storage
        .runtime_instances(None)
        .map_err(|error| error.to_string())?
        .into_iter()
        .filter(|runtime| {
            runtime.management_mode == RuntimeManagementMode::External
                && (runtime.last_observed_at_ms <= 0
                    || !runtime.instance.health.eq_ignore_ascii_case("HEALTHY")
                    || scan_at_ms.saturating_sub(runtime.last_observed_at_ms)
                        >= EXTERNAL_REPROBE_INTERVAL_MS)
        })
        .collect::<Vec<_>>();
    let mut failures = Vec::new();
    for runtime in external {
        if runtime.external_probe_protocol.trim().is_empty() {
            // Legacy imports intentionally remain Unknown until a formal
            // probe contract is supplied by a new Store health operation.
            continue;
        }
        let payload = ExternalHealthPayload {
            deployment_id: runtime.instance.deployment_id.clone(),
            service_id: runtime.instance.service_id.clone(),
            version: runtime.instance.release_version.clone(),
            endpoint: runtime.endpoint.clone(),
            protocol: runtime.external_probe_protocol.clone(),
            health_path: runtime.external_probe_health_path.clone(),
            artifact_digest: runtime.instance.artifact_digest.clone(),
        };
        let evidence = match probe_external_endpoint(&payload) {
            Ok(evidence) => evidence,
            Err(failure) => serde_json::json!({
                "healthy": false,
                "reachable": false,
                "health": "unhealthy",
                "latency_ms": Value::Null,
                "message": failure.detail,
                "endpoint": payload.endpoint,
                "protocol": payload.protocol,
            }),
        };
        if let Err(error) = persist_external_probe_evidence(storage, runtime, &payload, &evidence) {
            failures.push(format!("{}: {}", payload.deployment_id, error.detail));
        }
    }
    if failures.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "External runtime health projection failed: {}",
            failures.join("; ")
        ))
    }
}

pub(super) fn persist_external_probe_evidence(
    storage: &DurableStore,
    mut stored: StoredRuntimeInstance,
    payload: &ExternalHealthPayload,
    evidence: &Value,
) -> Result<StoredRuntimeInstance, ExternalHealthFailure> {
    let healthy = evidence
        .get("healthy")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    stored.external_probe_protocol = payload.protocol.clone();
    stored.external_probe_health_path = payload.health_path.clone();
    stored.last_observed_at_ms = now_ms();
    stored.updated_at = now_marker();
    if healthy {
        stored.instance.observed_state = RuntimeObservedState::Running;
        stored.instance.health = "HEALTHY".to_string();
        stored.drift_reason.clear();
    } else {
        stored.instance.observed_state = RuntimeObservedState::Unknown;
        stored.instance.health = "UNHEALTHY".to_string();
        stored.drift_reason = bounded_external_probe_detail(
            evidence
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("External endpoint did not pass its protocol health probe"),
        );
    }
    storage
        .put_runtime_instance(&stored)
        .map_err(external_storage_failure)?;
    Ok(stored)
}

pub(super) fn external_unhealthy_failure(evidence: &Value) -> ExternalHealthFailure {
    ExternalHealthFailure {
        status: CompletionStatus::Failed,
        code: "EXTERNAL_ENDPOINT_UNHEALTHY",
        detail: evidence
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("External endpoint did not pass its protocol health probe")
            .to_string(),
    }
}

pub(super) fn bounded_external_probe_detail(detail: &str) -> String {
    let mut printable = String::new();
    for character in detail.chars().map(|character| {
        if character.is_control() {
            ' '
        } else {
            character
        }
    }) {
        if printable.len() + character.len_utf8() > 512 {
            break;
        }
        printable.push(character);
    }
    if printable.trim().is_empty() {
        "External endpoint is unhealthy".to_string()
    } else {
        printable
    }
}

pub(super) fn external_storage_failure(
    error: crate::durable::DurableError,
) -> ExternalHealthFailure {
    ExternalHealthFailure {
        status: CompletionStatus::RetryableFailure,
        code: "EXTERNAL_PROJECTION_FAILED",
        detail: error.to_string(),
    }
}

pub(super) fn probe_external_endpoint(
    payload: &ExternalHealthPayload,
) -> Result<Value, ExternalHealthFailure> {
    let timeout = external_health_timeout();
    if payload.endpoint.contains("://") {
        return probe_external_uri(payload, timeout);
    }
    let endpoint = Endpoint {
        endpoint: payload.endpoint.clone(),
        service_id: payload.service_id.clone(),
        protocol: payload.protocol.clone(),
        health_path: payload.health_path.clone(),
        health: String::new(),
        reachable: false,
        display_name: String::new(),
        note: String::new(),
        config: Value::Object(Default::default()),
        created_at: String::new(),
        updated_at: String::new(),
    };
    let result = TcpEndpointProbe::new(timeout)
        .probe(&endpoint)
        .map_err(|error| ExternalHealthFailure {
            status: CompletionStatus::Failed,
            code: "EXTERNAL_ENDPOINT_INVALID",
            detail: error.to_string(),
        })?;
    Ok(serde_json::json!({
        "healthy": result.reachable && result.health.eq_ignore_ascii_case("healthy"),
        "reachable": result.reachable,
        "health": result.health,
        "latency_ms": result.latency_ms,
        "message": result.message,
        "endpoint": result.endpoint,
        "protocol": payload.protocol,
    }))
}

pub(super) fn probe_external_uri(
    payload: &ExternalHealthPayload,
    timeout: Duration,
) -> Result<Value, ExternalHealthFailure> {
    let uri = payload
        .endpoint
        .parse::<ureq::http::Uri>()
        .map_err(|error| ExternalHealthFailure {
            status: CompletionStatus::Failed,
            code: "EXTERNAL_ENDPOINT_INVALID",
            detail: format!("External endpoint URI is invalid: {error}"),
        })?;
    if uri.scheme_str() != Some(payload.protocol.as_str()) {
        return Err(ExternalHealthFailure {
            status: CompletionStatus::Failed,
            code: "EXTERNAL_PROTOCOL_MISMATCH",
            detail: format!(
                "endpoint scheme {:?} does not match release protocol {}",
                uri.scheme_str(),
                payload.protocol
            ),
        });
    }
    if matches!(payload.protocol.as_str(), "http" | "https") {
        let mut url = payload.endpoint.trim_end_matches('/').to_string();
        if !payload.health_path.trim().is_empty() {
            if !payload.health_path.starts_with('/') {
                return Err(ExternalHealthFailure {
                    status: CompletionStatus::Failed,
                    code: "EXTERNAL_HEALTH_PATH_INVALID",
                    detail: "HTTP health_path must begin with /".to_string(),
                });
            }
            url.push_str(&payload.health_path);
        }
        let started = std::time::Instant::now();
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .timeout_global(Some(timeout))
            .http_status_as_error(false)
            .max_redirects(0)
            .proxy(None)
            .build()
            .into();
        return match agent.get(&url).call() {
            Ok(response) => {
                let status = response.status().as_u16();
                Ok(serde_json::json!({
                    "healthy": (200..=399).contains(&status),
                    "reachable": true,
                    "health": if (200..=399).contains(&status) { "healthy" } else { "unhealthy" },
                    "latency_ms": started.elapsed().as_millis().min(u128::from(u32::MAX)) as u32,
                    "message": format!("{} health probe returned HTTP {status}", payload.protocol),
                    "endpoint": payload.endpoint,
                    "probe_url": url,
                    "protocol": payload.protocol,
                }))
            }
            Err(error) => Ok(serde_json::json!({
                "healthy": false,
                "reachable": false,
                "health": "unreachable",
                "latency_ms": Value::Null,
                "message": format!("{} health probe failed: {error}", payload.protocol),
                "endpoint": payload.endpoint,
                "probe_url": url,
                "protocol": payload.protocol,
            })),
        };
    }
    let authority = uri.authority().ok_or_else(|| ExternalHealthFailure {
        status: CompletionStatus::Failed,
        code: "EXTERNAL_ENDPOINT_INVALID",
        detail: "External TCP endpoint URI has no authority".to_string(),
    })?;
    if authority.as_str().contains('@') {
        return Err(ExternalHealthFailure {
            status: CompletionStatus::Failed,
            code: "EXTERNAL_ENDPOINT_INVALID",
            detail: "External health endpoint must not embed credentials".to_string(),
        });
    }
    let mut addresses =
        authority
            .as_str()
            .to_socket_addrs()
            .map_err(|error| ExternalHealthFailure {
                status: CompletionStatus::Failed,
                code: "EXTERNAL_ENDPOINT_INVALID",
                detail: format!("External endpoint cannot resolve: {error}"),
            })?;
    let address = addresses.next().ok_or_else(|| ExternalHealthFailure {
        status: CompletionStatus::Failed,
        code: "EXTERNAL_ENDPOINT_INVALID",
        detail: "External endpoint resolved to no socket address".to_string(),
    })?;
    let started = std::time::Instant::now();
    match TcpStream::connect_timeout(&address, timeout) {
        Ok(_) => Ok(serde_json::json!({
            "healthy": true,
            "reachable": true,
            "health": "healthy",
            "latency_ms": started.elapsed().as_millis().min(u128::from(u32::MAX)) as u32,
            "message": format!("{} TCP health probe connected", payload.protocol),
            "endpoint": payload.endpoint,
            "protocol": payload.protocol,
        })),
        Err(error) => Ok(serde_json::json!({
            "healthy": false,
            "reachable": false,
            "health": "unreachable",
            "latency_ms": Value::Null,
            "message": format!("{} TCP health probe failed: {error}", payload.protocol),
            "endpoint": payload.endpoint,
            "protocol": payload.protocol,
        })),
    }
}

pub(super) fn external_health_timeout() -> Duration {
    let millis = std::env::var("ORCHESTRATOR_EXTERNAL_HEALTH_TIMEOUT_MS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(5_000)
        .clamp(100, 30_000);
    Duration::from_millis(millis)
}
