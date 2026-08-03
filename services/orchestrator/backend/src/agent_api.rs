use crate::artifact_store::{ArtifactStore, ArtifactStoreError, DEFAULT_CHUNK_BYTES};
use crate::durable::{DurableJobStore, DurableStore};
use crate::http::{ApiRequest, ApiResponse};
use crate::node_identity::{NodeIdentityService, NodePeerIdentity};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use getrandom::fill as random_fill;
use orchestrator_control_plane::{
    ClaimRequest, CompleteRequest, CompletionStatus, DEFAULT_LEASE_MS, DEFAULT_LONG_POLL_MS,
    HeartbeatRequest, JobError, JobStatus, JobStore, NewJobEvent, OperationCoordinator,
    OperationError,
};
use orchestrator_runtime::{ArtifactReference, RuntimeInstance};
use orchestrator_storage::{RuntimeManagementMode, StoredRuntimeInstance};
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::BTreeSet;
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const AGENT_LONG_POLL_PREFERENCE: &str = "wait=25";
const CLAIM_POLL_INTERVAL: Duration = Duration::from_millis(500);

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ClaimBody {
    instance_id: String,
    protocol_version: String,
    capabilities: Vec<String>,
    max_jobs: u8,
}

const REQUIRED_V1_AGENT_CAPABILITIES: &[&str] = &[
    "install",
    "release_pipeline",
    "upgrade",
    "start",
    "stop",
    "restart",
    "uninstall",
    "rollback",
    "health",
];

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct LeaseBody {
    lease_token: String,
    events: Vec<NewJobEvent>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CompleteBody {
    lease_token: String,
    status: CompletionStatus,
    result: Value,
    #[serde(default)]
    error_message: String,
    events: Vec<NewJobEvent>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct EnrollmentBody {
    enrollment_code: String,
    csr_pem: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RenewalBody {
    csr_pem: String,
}

#[derive(Debug, Clone, Copy)]
pub(crate) enum AgentCaller<'a> {
    LocalBootstrap { node_id: &'a str },
    Mtls(&'a NodePeerIdentity),
    AnonymousTls,
}

pub(crate) fn is_agent_path(path: &str) -> bool {
    path.starts_with("/api/v1/agent/")
}

#[cfg(test)]
pub(crate) fn route(
    storage: Option<&DurableStore>,
    jobs: Option<&Mutex<DurableJobStore>>,
    request: ApiRequest,
) -> ApiResponse {
    let local_node_id = request
        .path
        .split('?')
        .next()
        .and_then(|path| path.strip_prefix("/api/v1/agent/nodes/"))
        .and_then(|suffix| suffix.split('/').next())
        .and_then(|segment| segment.split(':').next())
        .unwrap_or("desktop-local")
        .to_string();
    route_authenticated(
        storage,
        jobs,
        None,
        None,
        AgentCaller::LocalBootstrap {
            node_id: &local_node_id,
        },
        request,
    )
}

pub(crate) fn route_authenticated(
    storage: Option<&DurableStore>,
    jobs: Option<&Mutex<DurableJobStore>>,
    artifact_store: Option<&ArtifactStore>,
    identity_service: Option<&NodeIdentityService>,
    caller: AgentCaller<'_>,
    request: ApiRequest,
) -> ApiResponse {
    if request.headers.contains_key("authorization") {
        return ApiResponse::problem(
            401,
            "AGENT_BEARER_FORBIDDEN",
            "the Agent protocol does not accept bearer credentials",
            "req-agent-auth",
            None,
        );
    }
    let Some(storage) = storage else {
        return ApiResponse::error(503, "durable Node identity storage is unavailable");
    };
    let path = request.path.split('?').next().unwrap_or("/");
    if path == "/api/v1/agent/enroll" {
        if request.method != "POST" {
            return ApiResponse::problem(
                405,
                "AGENT_METHOD_NOT_ALLOWED",
                "Node enrollment requires POST",
                "req-agent-enroll",
                None,
            );
        }
        return match enroll(storage, identity_service, caller, &request) {
            Ok(response) => response,
            Err(error) => ApiResponse::problem(
                error.status,
                error.code,
                error.detail,
                "req-agent-enroll",
                None,
            ),
        };
    }
    if path == "/api/v1/agent/certificates:renew" {
        if request.method != "POST" {
            return ApiResponse::problem(
                405,
                "AGENT_METHOD_NOT_ALLOWED",
                "Node certificate renewal requires POST",
                "req-agent-renew",
                None,
            );
        }
        return match renew_certificate(storage, identity_service, caller, &request) {
            Ok(response) => response,
            Err(error) => ApiResponse::problem(
                error.status,
                error.code,
                error.detail,
                "req-agent-renew",
                None,
            ),
        };
    }
    if path == "/api/v1/agent/certificates:activate" {
        if request.method != "POST" {
            return ApiResponse::problem(
                405,
                "AGENT_METHOD_NOT_ALLOWED",
                "Node certificate activation requires POST",
                "req-agent-activate",
                None,
            );
        }
        return match activate_certificate(storage, identity_service, caller) {
            Ok(response) => response,
            Err(error) => ApiResponse::problem(
                error.status,
                error.code,
                error.detail,
                "req-agent-activate",
                None,
            ),
        };
    }
    if let Err(error) = authorize_job_protocol(storage, identity_service, caller, path) {
        return ApiResponse::problem(
            error.status,
            error.code,
            error.detail,
            "req-agent-auth",
            None,
        );
    }
    let segments = path.trim_start_matches('/').split('/').collect::<Vec<_>>();
    if let ["api", "v1", "agent", "nodes", _node_id, "identity"] = segments.as_slice() {
        if request.method != "GET" {
            return ApiResponse::problem(
                405,
                "AGENT_METHOD_NOT_ALLOWED",
                "Node identity verification requires GET",
                "req-agent-identity",
                None,
            );
        }
        return match verify_identity(caller) {
            Ok(response) => response,
            Err(error) => ApiResponse::problem(
                error.status,
                error.code,
                error.detail,
                "req-agent-identity",
                None,
            ),
        };
    }
    let Some(jobs) = jobs else {
        return ApiResponse::error(503, "durable job storage is unavailable");
    };
    match route_with_store(storage, jobs, artifact_store, &request, &segments) {
        Ok(response) => response,
        Err(error) => ApiResponse::problem(
            error.status,
            error.code,
            error.detail,
            "req-agent-protocol",
            None,
        ),
    }
}

fn verify_identity(caller: AgentCaller<'_>) -> Result<ApiResponse, AgentApiError> {
    let AgentCaller::Mtls(peer) = caller else {
        return Err(AgentApiError {
            status: 401,
            code: "AGENT_MTLS_REQUIRED",
            detail: "identity verification requires a verified Node mTLS certificate".to_string(),
        });
    };
    Ok(ApiResponse::ok(json!({
        "node_id": peer.node_id,
        "spiffe_id": peer.spiffe_id,
        "serial_hex": peer.serial_hex,
        "status": "ACTIVE",
    })))
}

fn authorize_job_protocol(
    storage: &DurableStore,
    identity_service: Option<&NodeIdentityService>,
    caller: AgentCaller<'_>,
    path: &str,
) -> Result<(), AgentApiError> {
    let node_id = path
        .strip_prefix("/api/v1/agent/nodes/")
        .and_then(|suffix| suffix.split('/').next())
        .and_then(|segment| segment.split(':').next())
        .filter(|node_id| !node_id.is_empty())
        .ok_or_else(|| AgentApiError {
            status: 404,
            code: "AGENT_ROUTE_NOT_FOUND",
            detail: "the requested Agent route has no Node identity".to_string(),
        })?;
    match caller {
        AgentCaller::LocalBootstrap {
            node_id: local_node,
        } if local_node == node_id => Ok(()),
        AgentCaller::LocalBootstrap { .. } => Err(AgentApiError {
            status: 403,
            code: "AGENT_IDENTITY_MISMATCH",
            detail: "Desktop bootstrap identity does not match the path node_id".to_string(),
        }),
        AgentCaller::AnonymousTls => Err(AgentApiError {
            status: 401,
            code: "AGENT_MTLS_REQUIRED",
            detail: "a verified Node mTLS client certificate is required".to_string(),
        }),
        AgentCaller::Mtls(peer) => {
            if peer.node_id != node_id {
                return Err(AgentApiError {
                    status: 403,
                    code: "AGENT_IDENTITY_MISMATCH",
                    detail: format!(
                        "certificate Node {} cannot access path Node {node_id}",
                        peer.node_id
                    ),
                });
            }
            identity_service
                .ok_or_else(|| AgentApiError {
                    status: 503,
                    code: "AGENT_IDENTITY_UNAVAILABLE",
                    detail: "Node certificate authority is not configured".to_string(),
                })?
                .authenticate(storage, peer, now_ms())
                .map(|_| ())
                .map_err(|error| AgentApiError {
                    status: 401,
                    code: "AGENT_CERTIFICATE_REJECTED",
                    detail: error.to_string(),
                })
        }
    }
}

fn enroll(
    storage: &DurableStore,
    identity_service: Option<&NodeIdentityService>,
    caller: AgentCaller<'_>,
    request: &ApiRequest,
) -> Result<ApiResponse, AgentApiError> {
    if matches!(caller, AgentCaller::Mtls(_)) {
        return Err(AgentApiError {
            status: 409,
            code: "AGENT_ALREADY_ENROLLED",
            detail: "an authenticated Node must renew rather than redeem an enrollment code"
                .to_string(),
        });
    }
    if matches!(caller, AgentCaller::LocalBootstrap { .. }) {
        return Err(AgentApiError {
            status: 403,
            code: "AGENT_ENROLLMENT_TLS_REQUIRED",
            detail: "Node enrollment is available only on the TLS listener".to_string(),
        });
    }
    let identity = identity_service.ok_or_else(|| AgentApiError {
        status: 503,
        code: "AGENT_IDENTITY_UNAVAILABLE",
        detail: "Node certificate authority is not configured".to_string(),
    })?;
    let body: EnrollmentBody = serde_json::from_str(&request.body).map_err(invalid_json)?;
    if body.enrollment_code.trim().is_empty() || body.csr_pem.trim().is_empty() {
        return Err(invalid("enrollment_code and csr_pem are required"));
    }
    match crate::node_identity::redeem(
        storage,
        identity,
        body.enrollment_code.trim(),
        &body.csr_pem,
        now_ms(),
    )
    .map_err(|error| AgentApiError {
        status: 422,
        code: "AGENT_ENROLLMENT_REJECTED",
        detail: error.to_string(),
    })? {
        orchestrator_storage::EnrollmentRedemption::Redeemed(certificate)
        | orchestrator_storage::EnrollmentRedemption::Replayed(certificate) => {
            Ok(ApiResponse::created(json!({
                "node_id": certificate.node_id,
                "spiffe_id": certificate.spiffe_id,
                "serial_hex": certificate.serial_hex,
                "certificate_pem": certificate.certificate_pem,
                "ca_certificate_pem": identity.ca_certificate_pem(),
                "not_after_ms": certificate.not_after_ms,
                "renew_after_ms": certificate.not_after_ms - orchestrator_storage::CERTIFICATE_RENEWAL_WINDOW_MS,
            })))
        }
        orchestrator_storage::EnrollmentRedemption::NotFound => Err(AgentApiError {
            status: 401,
            code: "AGENT_ENROLLMENT_CODE_INVALID",
            detail: "enrollment code is invalid".to_string(),
        }),
        orchestrator_storage::EnrollmentRedemption::Expired => Err(AgentApiError {
            status: 410,
            code: "AGENT_ENROLLMENT_CODE_EXPIRED",
            detail: "enrollment code has expired".to_string(),
        }),
        orchestrator_storage::EnrollmentRedemption::AlreadyRedeemed => Err(AgentApiError {
            status: 409,
            code: "AGENT_ENROLLMENT_CODE_REPLAYED",
            detail: "enrollment code has already been redeemed".to_string(),
        }),
        orchestrator_storage::EnrollmentRedemption::ReplayCertificateRevoked => {
            Err(AgentApiError {
                status: 409,
                code: "AGENT_ENROLLMENT_CERTIFICATE_REVOKED",
                detail: "the certificate committed for this enrollment was revoked; issue a new enrollment code"
                    .to_string(),
            })
        }
        orchestrator_storage::EnrollmentRedemption::ReplayCertificateNotYetValid => {
            Err(AgentApiError {
                status: 409,
                code: "AGENT_ENROLLMENT_CERTIFICATE_NOT_YET_VALID",
                detail: "the certificate committed for this enrollment is not yet valid; verify control-plane time"
                    .to_string(),
            })
        }
        orchestrator_storage::EnrollmentRedemption::ReplayCertificateExpired => {
            Err(AgentApiError {
                status: 410,
                code: "AGENT_ENROLLMENT_CERTIFICATE_EXPIRED",
                detail: "the certificate committed for this enrollment expired; issue a new enrollment code"
                    .to_string(),
            })
        }
        orchestrator_storage::EnrollmentRedemption::NodeMismatch => Err(AgentApiError {
            status: 409,
            code: "AGENT_ENROLLMENT_IDENTITY_MISMATCH",
            detail: "issued certificate does not match enrollment Node".to_string(),
        }),
    }
}

fn renew_certificate(
    storage: &DurableStore,
    identity_service: Option<&NodeIdentityService>,
    caller: AgentCaller<'_>,
    request: &ApiRequest,
) -> Result<ApiResponse, AgentApiError> {
    let AgentCaller::Mtls(peer) = caller else {
        return Err(AgentApiError {
            status: 401,
            code: "AGENT_MTLS_REQUIRED",
            detail: "certificate renewal requires the current Node mTLS identity".to_string(),
        });
    };
    let identity = identity_service.ok_or_else(|| AgentApiError {
        status: 503,
        code: "AGENT_IDENTITY_UNAVAILABLE",
        detail: "Node certificate authority is not configured".to_string(),
    })?;
    let body: RenewalBody = serde_json::from_str(&request.body).map_err(invalid_json)?;
    if body.csr_pem.trim().is_empty() {
        return Err(invalid("csr_pem is required"));
    }
    match crate::node_identity::renew(storage, identity, peer, &body.csr_pem, now_ms()).map_err(
        |error| AgentApiError {
            status: 401,
            code: "AGENT_CERTIFICATE_REJECTED",
            detail: error.to_string(),
        },
    )? {
        orchestrator_storage::CertificateRotation::Rotated(certificate) => {
            Ok(ApiResponse::ok(json!({
                "node_id": certificate.node_id,
                "spiffe_id": certificate.spiffe_id,
                "serial_hex": certificate.serial_hex,
                "certificate_pem": certificate.certificate_pem,
                "ca_certificate_pem": identity.ca_certificate_pem(),
                "not_after_ms": certificate.not_after_ms,
                "renew_after_ms": certificate.not_after_ms - orchestrator_storage::CERTIFICATE_RENEWAL_WINDOW_MS,
            })))
        }
        orchestrator_storage::CertificateRotation::NotDue { renew_at_ms } => Err(AgentApiError {
            status: 409,
            code: "AGENT_CERTIFICATE_RENEWAL_NOT_DUE",
            detail: format!("certificate may be renewed at unix-ms:{renew_at_ms}"),
        }),
        orchestrator_storage::CertificateRotation::NotFound
        | orchestrator_storage::CertificateRotation::Revoked
        | orchestrator_storage::CertificateRotation::Expired => Err(AgentApiError {
            status: 401,
            code: "AGENT_CERTIFICATE_REJECTED",
            detail: "current certificate is unknown, revoked, or expired".to_string(),
        }),
        orchestrator_storage::CertificateRotation::NodeMismatch => Err(AgentApiError {
            status: 403,
            code: "AGENT_IDENTITY_MISMATCH",
            detail: "renewal certificate identity does not match the current Node".to_string(),
        }),
    }
}

fn activate_certificate(
    storage: &DurableStore,
    identity_service: Option<&NodeIdentityService>,
    caller: AgentCaller<'_>,
) -> Result<ApiResponse, AgentApiError> {
    let AgentCaller::Mtls(peer) = caller else {
        return Err(AgentApiError {
            status: 401,
            code: "AGENT_MTLS_REQUIRED",
            detail: "certificate activation requires the replacement Node mTLS identity"
                .to_string(),
        });
    };
    let identity = identity_service.ok_or_else(|| AgentApiError {
        status: 503,
        code: "AGENT_IDENTITY_UNAVAILABLE",
        detail: "Node certificate authority is not configured".to_string(),
    })?;
    identity
        .authenticate(storage, peer, now_ms())
        .map_err(|error| AgentApiError {
            status: 401,
            code: "AGENT_CERTIFICATE_REJECTED",
            detail: error.to_string(),
        })?;
    match storage
        .activate_node_certificate(&peer.node_id, &peer.serial_hex, now_ms())
        .map_err(|error| AgentApiError {
            status: 500,
            code: "AGENT_STORAGE_ERROR",
            detail: error.to_string(),
        })? {
        orchestrator_storage::CertificateActivation::Activated { .. } => {
            Ok(ApiResponse::no_content(Value::Null))
        }
        orchestrator_storage::CertificateActivation::NotFound
        | orchestrator_storage::CertificateActivation::Revoked
        | orchestrator_storage::CertificateActivation::Expired => Err(AgentApiError {
            status: 401,
            code: "AGENT_CERTIFICATE_REJECTED",
            detail: "replacement certificate is unknown, revoked, or expired".to_string(),
        }),
        orchestrator_storage::CertificateActivation::Superseded => Err(AgentApiError {
            status: 409,
            code: "AGENT_CERTIFICATE_SUPERSEDED",
            detail: "a newer replacement certificate must be activated".to_string(),
        }),
        orchestrator_storage::CertificateActivation::NodeMismatch => Err(AgentApiError {
            status: 403,
            code: "AGENT_IDENTITY_MISMATCH",
            detail: "replacement certificate does not match its Node".to_string(),
        }),
    }
}

fn route_with_store(
    storage: &DurableStore,
    jobs: &Mutex<DurableJobStore>,
    artifact_store: Option<&ArtifactStore>,
    request: &ApiRequest,
    segments: &[&str],
) -> Result<ApiResponse, AgentApiError> {
    match (request.method.as_str(), segments) {
        (
            "GET",
            [
                "api",
                "v1",
                "agent",
                "nodes",
                node_id,
                "jobs",
                job_id,
                "artifacts",
                artifact_id,
            ],
        ) => artifact_chunk(jobs, artifact_store, request, node_id, job_id, artifact_id),
        ("POST", ["api", "v1", "agent", "nodes", node_id, "jobs:claim"]) => {
            let body: ClaimBody = serde_json::from_str(&request.body).map_err(invalid_json)?;
            if body.protocol_version != "v1" {
                return Err(invalid("agent protocol_version must be v1"));
            }
            if body.instance_id.trim().is_empty() || body.max_jobs != 1 {
                return Err(invalid(
                    "instance_id is required and protocol v1 max_jobs must equal 1",
                ));
            }
            validate_claim_capabilities(&body.capabilities)?;
            let wait = claim_wait(request)?;
            let node = required_node(storage, node_id)?;
            touch_node(storage, node.clone())?;
            if matches!(
                node.status.to_ascii_uppercase().as_str(),
                "DRAINING" | "DRAINED" | "REMOVED"
            ) {
                return Ok(ApiResponse::ok(json!({
                    "jobs": [],
                    "retry_after_ms": 10_000,
                })));
            }
            let deadline = Instant::now() + wait;
            loop {
                let now = now_ms();
                let leased = {
                    // Never retain the queue mutex while waiting. PostgreSQL/SQLite
                    // work is one bounded claim transaction; the 25-second HTTP
                    // wait happens outside both the transaction and the mutex.
                    let mut store = lock_store(jobs)?;
                    store
                        .claim(ClaimRequest {
                            node_id: (*node_id).to_string(),
                            instance_id: body.instance_id.clone(),
                            lease_token: lease_token()?,
                            now_ms: now,
                            lease_ms: DEFAULT_LEASE_MS,
                        })
                        .map_err(job_error)?
                };
                let claimed = leased
                    .into_iter()
                    .map(|job| {
                        json!({
                            "job_id": job.job_id,
                            "kind": job.kind,
                            "payload": job.payload,
                            "payload_sha256": job.payload_sha256,
                            "lease_token": job.lease_token,
                            "lease_expires_at_ms": job.lease_expires_at_ms,
                        })
                    })
                    .collect::<Vec<_>>();
                let now = Instant::now();
                if !claimed.is_empty() || wait.is_zero() || now >= deadline {
                    let response = ApiResponse::ok(json!({
                        "jobs": claimed,
                        "retry_after_ms": if wait.is_zero() { 1000 } else { 0 },
                    }));
                    return Ok(if wait.is_zero() {
                        response
                    } else {
                        response.with_header("Preference-Applied", AGENT_LONG_POLL_PREFERENCE)
                    });
                }
                std::thread::sleep(
                    CLAIM_POLL_INTERVAL.min(deadline.saturating_duration_since(now)),
                );
            }
        }
        ("POST", ["api", "v1", "agent", "nodes", node_id, "jobs", job_action])
            if job_action.ends_with(":heartbeat") =>
        {
            let job_id = job_action.trim_end_matches(":heartbeat");
            let body: LeaseBody = serde_json::from_str(&request.body).map_err(invalid_json)?;
            let mut store = lock_store(jobs)?;
            ensure_job_node(&store, job_id, node_id)?;
            let job = store
                .heartbeat(HeartbeatRequest {
                    job_id: job_id.to_string(),
                    lease_token: body.lease_token,
                    now_ms: now_ms(),
                    lease_ms: DEFAULT_LEASE_MS,
                    events: body.events,
                })
                .map_err(job_error)?;
            touch_node(storage, required_node(storage, node_id)?)?;
            Ok(ApiResponse::ok(json!({
                "cancel_requested": job.status == JobStatus::CancelRequested,
            })))
        }
        ("POST", ["api", "v1", "agent", "nodes", node_id, "jobs", job_action])
            if job_action.ends_with(":complete") =>
        {
            let job_id = job_action.trim_end_matches(":complete");
            let body: CompleteBody = serde_json::from_str(&request.body).map_err(invalid_json)?;
            let mut store = lock_store(jobs)?;
            ensure_job_node(&store, job_id, node_id)?;
            let completed_at_ms = now_ms();
            let completed = store
                .complete(CompleteRequest {
                    job_id: job_id.to_string(),
                    lease_token: body.lease_token,
                    status: body.status,
                    result: body.result,
                    error_message: body.error_message,
                    now_ms: completed_at_ms,
                    events: body.events,
                })
                .map_err(job_error)?;
            touch_node(storage, required_node(storage, node_id)?)?;
            project_runtime_instance(storage, &completed)?;
            let mut operations = storage.operation_store();
            OperationCoordinator::new(&mut operations, &mut *store)
                .project(&completed.operation_id, completed_at_ms)
                .map_err(operation_error)?;
            reconcile_drained_node(storage, &mut store, node_id)?;
            Ok(ApiResponse::no_content(Value::Null))
        }
        _ => Err(AgentApiError {
            status: 404,
            code: "AGENT_ROUTE_NOT_FOUND",
            detail: "the requested agent protocol route does not exist".to_string(),
        }),
    }
}

fn claim_wait(request: &ApiRequest) -> Result<Duration, AgentApiError> {
    match request.headers.get("prefer").map(|value| value.trim()) {
        None | Some("") => Ok(Duration::ZERO),
        Some(AGENT_LONG_POLL_PREFERENCE) => Ok(Duration::from_millis(
            u64::try_from(DEFAULT_LONG_POLL_MS).expect("long-poll default is positive"),
        )),
        Some(_) => Err(invalid(
            "Agent claim Prefer must be exactly wait=25 when long polling is requested",
        )),
    }
}

fn validate_claim_capabilities(capabilities: &[String]) -> Result<(), AgentApiError> {
    let advertised = capabilities
        .iter()
        .map(String::as_str)
        .collect::<BTreeSet<_>>();
    let required = REQUIRED_V1_AGENT_CAPABILITIES
        .iter()
        .copied()
        .collect::<BTreeSet<_>>();
    if advertised.len() != capabilities.len() || advertised != required {
        return Err(invalid(format!(
            "protocol v1 requires the exact Agent capability set: {}",
            REQUIRED_V1_AGENT_CAPABILITIES.join(",")
        )));
    }
    Ok(())
}

fn artifact_chunk(
    jobs: &Mutex<DurableJobStore>,
    artifact_store: Option<&ArtifactStore>,
    request: &ApiRequest,
    node_id: &str,
    job_id: &str,
    artifact_id: &str,
) -> Result<ApiResponse, AgentApiError> {
    let lease_token = request
        .headers
        .get("x-ojos-lease-token")
        .map(String::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| AgentApiError {
            status: 401,
            code: "AGENT_ARTIFACT_LEASE_REQUIRED",
            detail: "artifact download requires the active Job lease token".to_string(),
        })?;
    let query = request
        .path
        .split_once('?')
        .map(|(_, query)| query)
        .unwrap_or("");
    let offset = crate::http::query_value(query, "offset")
        .map_err(|error| invalid(error.to_string()))?
        .unwrap_or_else(|| "0".to_string())
        .parse::<u64>()
        .map_err(|_| invalid("artifact offset must be an unsigned integer"))?;
    let length = crate::http::query_value(query, "length")
        .map_err(|error| invalid(error.to_string()))?
        .map(|value| value.parse::<u32>())
        .transpose()
        .map_err(|_| invalid("artifact length must be an unsigned integer"))?
        .unwrap_or(DEFAULT_CHUNK_BYTES);
    let store = lock_store(jobs)?;
    let job = store
        .get(job_id)
        .map_err(job_error)?
        .ok_or_else(|| AgentApiError {
            status: 404,
            code: "AGENT_JOB_NOT_FOUND",
            detail: format!("job {job_id} was not found"),
        })?;
    if job.node_id != node_id {
        return Err(AgentApiError {
            status: 403,
            code: "AGENT_JOB_NODE_MISMATCH",
            detail: format!("job {job_id} is not assigned to Node {node_id}"),
        });
    }
    if !matches!(job.status, JobStatus::Leased | JobStatus::CancelRequested)
        || job.lease_token.as_deref() != Some(lease_token)
        || job
            .lease_expires_at_ms
            .is_none_or(|expires| expires <= now_ms())
    {
        return Err(AgentApiError {
            status: 409,
            code: "AGENT_ARTIFACT_STALE_LEASE",
            detail: "artifact download lease is stale or no longer active".to_string(),
        });
    }
    let reference = artifact_reference(&job.payload, artifact_id).ok_or_else(|| AgentApiError {
        status: 404,
        code: "AGENT_ARTIFACT_NOT_ASSIGNED",
        detail: format!("artifact {artifact_id} is not assigned to job {job_id}"),
    })?;
    drop(store);
    let artifact_store = artifact_store.ok_or_else(|| AgentApiError {
        status: 503,
        code: "AGENT_ARTIFACT_STORAGE_UNAVAILABLE",
        detail: "artifact storage is unavailable".to_string(),
    })?;
    let chunk = artifact_store
        .read_chunk(&reference, offset, length)
        .map_err(artifact_error)?;
    Ok(ApiResponse::ok(json!({
        "artifact_id": reference.artifact_id,
        "sha256": reference.sha256,
        "offset": chunk.offset,
        "total_size": chunk.total_size,
        "data_base64": BASE64_STANDARD.encode(chunk.bytes),
        "eof": chunk.eof,
    }))
    .with_header("Cache-Control", "no-store"))
}

fn artifact_reference(payload: &Value, artifact_id: &str) -> Option<ArtifactReference> {
    ["/offline_oci_artifact", "/install/offline_oci_artifact"]
        .iter()
        .filter_map(|pointer| payload.pointer(pointer))
        .filter_map(|value| serde_json::from_value::<ArtifactReference>(value.clone()).ok())
        .find(|reference| reference.artifact_id == artifact_id)
}

fn artifact_error(error: ArtifactStoreError) -> AgentApiError {
    match error {
        ArtifactStoreError::NotFound => AgentApiError {
            status: 404,
            code: "AGENT_ARTIFACT_NOT_FOUND",
            detail: error.to_string(),
        },
        ArtifactStoreError::Invalid(_) => AgentApiError {
            status: 422,
            code: "AGENT_ARTIFACT_REQUEST_INVALID",
            detail: error.to_string(),
        },
        ArtifactStoreError::Integrity => AgentApiError {
            status: 500,
            code: "AGENT_ARTIFACT_INTEGRITY_FAILED",
            detail: error.to_string(),
        },
        ArtifactStoreError::Io(_) => AgentApiError {
            status: 500,
            code: "AGENT_ARTIFACT_STORAGE_ERROR",
            detail: error.to_string(),
        },
    }
}

fn required_node(
    storage: &DurableStore,
    node_id: &str,
) -> Result<orchestrator_legacy::NodeRecord, AgentApiError> {
    storage
        .get_node(node_id)
        .map_err(|error| AgentApiError {
            status: 500,
            code: "AGENT_STORAGE_ERROR",
            detail: error.to_string(),
        })?
        .ok_or_else(|| AgentApiError {
            status: 404,
            code: "AGENT_NODE_NOT_FOUND",
            detail: format!("node {node_id} is not registered"),
        })
}

fn touch_node(
    storage: &DurableStore,
    mut node: orchestrator_legacy::NodeRecord,
) -> Result<(), AgentApiError> {
    node.updated_at = format!("unix-ms:{}", now_ms());
    storage.upsert_node(node).map_err(|error| AgentApiError {
        status: 500,
        code: "AGENT_STORAGE_ERROR",
        detail: error.to_string(),
    })
}

fn project_runtime_instance(
    storage: &DurableStore,
    completed: &orchestrator_control_plane::Job,
) -> Result<(), AgentApiError> {
    if completed.status != JobStatus::Succeeded {
        return Ok(());
    }
    match completed.kind {
        orchestrator_control_plane::JobKind::Install
        | orchestrator_control_plane::JobKind::ReleasePipeline => {
            let Some(instance) = completed
                .result
                .as_ref()
                .and_then(|result| result.get("instance"))
            else {
                return Err(AgentApiError {
                    status: 500,
                    code: "AGENT_RUNTIME_PROJECTION_FAILED",
                    detail: format!(
                        "job {} succeeded without a runtime instance projection",
                        completed.job_id
                    ),
                });
            };
            let mut instance: RuntimeInstance =
                serde_json::from_value(instance.clone()).map_err(|error| AgentApiError {
                    status: 500,
                    code: "AGENT_RUNTIME_PROJECTION_FAILED",
                    detail: format!("decode runtime instance projection failed: {error}"),
                })?;
            let (endpoint_pointer, version_pointer) = match completed.kind {
                orchestrator_control_plane::JobKind::Install => (
                    "/spec/published_endpoint/endpoint",
                    "/spec/labels/ojos.release_version",
                ),
                orchestrator_control_plane::JobKind::ReleasePipeline => (
                    "/install/spec/published_endpoint/endpoint",
                    "/install/spec/labels/ojos.release_version",
                ),
                _ => unreachable!("install projection match guards the Job kind"),
            };
            let endpoint = completed
                .payload
                .pointer(endpoint_pointer)
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            let release_version = completed
                .payload
                .pointer(version_pointer)
                .and_then(Value::as_str)
                .filter(|value| !value.trim().is_empty())
                .ok_or_else(|| AgentApiError {
                    status: 500,
                    code: "AGENT_RUNTIME_PROJECTION_FAILED",
                    detail: format!(
                        "job {} has no signed release version binding",
                        completed.job_id
                    ),
                })?;
            if !instance.release_version.is_empty() && instance.release_version != release_version {
                return Err(AgentApiError {
                    status: 500,
                    code: "AGENT_RUNTIME_PROJECTION_FAILED",
                    detail: format!(
                        "job {} runtime release version does not match its signed spec",
                        completed.job_id
                    ),
                });
            }
            instance.release_version = release_version.to_string();
            let stored = StoredRuntimeInstance {
                node_id: completed.node_id.clone(),
                instance,
                management_mode: RuntimeManagementMode::Managed,
                endpoint,
                updated_at: format!("unix-ms:{}", completed.updated_at_ms),
            };
            storage
                .put_runtime_instance(&stored)
                .map_err(|error| AgentApiError {
                    status: 500,
                    code: "AGENT_RUNTIME_PROJECTION_FAILED",
                    detail: format!("persist runtime instance projection failed: {error}"),
                })?;
        }
        orchestrator_control_plane::JobKind::Start
        | orchestrator_control_plane::JobKind::Stop
        | orchestrator_control_plane::JobKind::Restart
        | orchestrator_control_plane::JobKind::Health => {
            let Some(instance) = completed
                .result
                .as_ref()
                .and_then(|result| result.get("instance"))
            else {
                return Err(AgentApiError {
                    status: 500,
                    code: "AGENT_RUNTIME_PROJECTION_FAILED",
                    detail: format!(
                        "job {} succeeded without a runtime instance projection",
                        completed.job_id
                    ),
                });
            };
            let mut instance: RuntimeInstance =
                serde_json::from_value(instance.clone()).map_err(|error| AgentApiError {
                    status: 500,
                    code: "AGENT_RUNTIME_PROJECTION_FAILED",
                    detail: format!("decode runtime instance projection failed: {error}"),
                })?;
            let previous = storage
                .runtime_instance(&instance.deployment_id)
                .map_err(|error| AgentApiError {
                    status: 500,
                    code: "AGENT_RUNTIME_PROJECTION_FAILED",
                    detail: format!("load previous runtime projection failed: {error}"),
                })?
                .ok_or_else(|| AgentApiError {
                    status: 500,
                    code: "AGENT_RUNTIME_PROJECTION_FAILED",
                    detail: format!(
                        "lifecycle job {} cannot preserve a missing runtime projection",
                        completed.job_id
                    ),
                })?;
            if !instance.release_version.is_empty()
                && instance.release_version != previous.instance.release_version
            {
                return Err(AgentApiError {
                    status: 500,
                    code: "AGENT_RUNTIME_PROJECTION_FAILED",
                    detail: format!(
                        "lifecycle job {} changed the persisted release version binding",
                        completed.job_id
                    ),
                });
            }
            instance.release_version = previous.instance.release_version.clone();
            let stored = StoredRuntimeInstance {
                node_id: completed.node_id.clone(),
                instance,
                management_mode: previous.management_mode,
                endpoint: previous.endpoint,
                updated_at: format!("unix-ms:{}", completed.updated_at_ms),
            };
            storage
                .put_runtime_instance(&stored)
                .map_err(|error| AgentApiError {
                    status: 500,
                    code: "AGENT_RUNTIME_PROJECTION_FAILED",
                    detail: format!("persist runtime instance projection failed: {error}"),
                })?;
        }
        orchestrator_control_plane::JobKind::Upgrade
        | orchestrator_control_plane::JobKind::Rollback => {
            let result = completed.result.as_ref().ok_or_else(|| AgentApiError {
                status: 500,
                code: "AGENT_RUNTIME_PROJECTION_FAILED",
                detail: format!(
                    "replacement job {} succeeded without a result",
                    completed.job_id
                ),
            })?;
            let instance = result.get("instance").ok_or_else(|| AgentApiError {
                status: 500,
                code: "AGENT_RUNTIME_PROJECTION_FAILED",
                detail: format!(
                    "replacement job {} succeeded without a runtime instance projection",
                    completed.job_id
                ),
            })?;
            let replaced_deployment_id = result
                .get("replaced_deployment_id")
                .and_then(Value::as_str)
                .filter(|value| !value.trim().is_empty())
                .ok_or_else(|| AgentApiError {
                    status: 500,
                    code: "AGENT_RUNTIME_PROJECTION_FAILED",
                    detail: format!(
                        "replacement job {} omitted replaced_deployment_id",
                        completed.job_id
                    ),
                })?;
            let replaced_container_id = result
                .get("replaced_container_id")
                .and_then(Value::as_str)
                .filter(|value| !value.trim().is_empty())
                .ok_or_else(|| AgentApiError {
                    status: 500,
                    code: "AGENT_RUNTIME_PROJECTION_FAILED",
                    detail: format!(
                        "replacement job {} omitted replaced_container_id",
                        completed.job_id
                    ),
                })?;
            if completed
                .payload
                .get("old_deployment_id")
                .and_then(Value::as_str)
                != Some(replaced_deployment_id)
                || completed
                    .payload
                    .get("old_container_id")
                    .and_then(Value::as_str)
                    != Some(replaced_container_id)
            {
                return Err(AgentApiError {
                    status: 500,
                    code: "AGENT_RUNTIME_PROJECTION_FAILED",
                    detail: format!(
                        "replacement job {} result does not match its persisted old deployment",
                        completed.job_id
                    ),
                });
            }
            let mut instance: RuntimeInstance =
                serde_json::from_value(instance.clone()).map_err(|error| AgentApiError {
                    status: 500,
                    code: "AGENT_RUNTIME_PROJECTION_FAILED",
                    detail: format!("decode replacement runtime projection failed: {error}"),
                })?;
            let release_version = completed
                .payload
                .pointer("/new_spec/labels/ojos.release_version")
                .and_then(Value::as_str)
                .filter(|value| !value.trim().is_empty())
                .ok_or_else(|| AgentApiError {
                    status: 500,
                    code: "AGENT_RUNTIME_PROJECTION_FAILED",
                    detail: format!(
                        "replacement job {} has no signed release version binding",
                        completed.job_id
                    ),
                })?;
            if !instance.release_version.is_empty() && instance.release_version != release_version {
                return Err(AgentApiError {
                    status: 500,
                    code: "AGENT_RUNTIME_PROJECTION_FAILED",
                    detail: format!(
                        "replacement job {} runtime release version does not match its signed spec",
                        completed.job_id
                    ),
                });
            }
            instance.release_version = release_version.to_string();
            let endpoint = completed
                .payload
                .pointer("/new_spec/published_endpoint/endpoint")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            let stored = StoredRuntimeInstance {
                node_id: completed.node_id.clone(),
                instance,
                management_mode: RuntimeManagementMode::Managed,
                endpoint,
                updated_at: format!("unix-ms:{}", completed.updated_at_ms),
            };
            storage
                .replace_runtime_instance(replaced_deployment_id, &stored)
                .map_err(|error| AgentApiError {
                    status: 500,
                    code: "AGENT_RUNTIME_PROJECTION_FAILED",
                    detail: format!("persist atomic runtime replacement failed: {error}"),
                })?;
        }
        orchestrator_control_plane::JobKind::Uninstall => {
            let container_id = completed
                .result
                .as_ref()
                .and_then(|result| result.get("container_id"))
                .and_then(Value::as_str)
                .unwrap_or_default();
            let deployment_id = completed
                .payload
                .get("deployment_id")
                .and_then(Value::as_str)
                .map(str::to_string)
                .or_else(|| {
                    storage
                        .runtime_instances(Some(&completed.node_id))
                        .ok()?
                        .into_iter()
                        .find(|stored| stored.instance.container_id == container_id)
                        .map(|stored| stored.instance.deployment_id)
                })
                .ok_or_else(|| AgentApiError {
                    status: 500,
                    code: "AGENT_RUNTIME_PROJECTION_FAILED",
                    detail: format!(
                        "uninstall job {} has no resolvable deployment projection",
                        completed.job_id
                    ),
                })?;
            storage
                .delete_runtime_instance(&deployment_id)
                .map_err(|error| AgentApiError {
                    status: 500,
                    code: "AGENT_RUNTIME_PROJECTION_FAILED",
                    detail: format!("delete runtime instance projection failed: {error}"),
                })?;
        }
        orchestrator_control_plane::JobKind::Inventory
        | orchestrator_control_plane::JobKind::ExternalHealth
        | orchestrator_control_plane::JobKind::TopologyApply
        | orchestrator_control_plane::JobKind::NodeDrain
        | orchestrator_control_plane::JobKind::NodeRemove => {}
    }
    Ok(())
}

fn reconcile_drained_node(
    storage: &DurableStore,
    job_store: &mut DurableJobStore,
    node_id: &str,
) -> Result<(), AgentApiError> {
    if job_store.active_job_count(node_id).map_err(job_error)? != 0 {
        return Ok(());
    }
    if !storage
        .runtime_instances(Some(node_id))
        .map_err(|error| AgentApiError {
            status: 500,
            code: "AGENT_STORAGE_ERROR",
            detail: error.to_string(),
        })?
        .is_empty()
    {
        return Ok(());
    }
    let Some(mut node) = storage.get_node(node_id).map_err(|error| AgentApiError {
        status: 500,
        code: "AGENT_STORAGE_ERROR",
        detail: error.to_string(),
    })?
    else {
        return Ok(());
    };
    if node.status.eq_ignore_ascii_case("DRAINING") {
        node.status = "DRAINED".to_string();
        node.updated_at = format!("unix-ms:{}", now_ms());
        storage.upsert_node(node).map_err(|error| AgentApiError {
            status: 500,
            code: "AGENT_STORAGE_ERROR",
            detail: error.to_string(),
        })?;
    }
    Ok(())
}

fn ensure_job_node(
    store: &DurableJobStore,
    job_id: &str,
    node_id: &str,
) -> Result<(), AgentApiError> {
    let job = store
        .get(job_id)
        .map_err(job_error)?
        .ok_or_else(|| job_error(JobError::NotFound(job_id.to_string())))?;
    if job.node_id != node_id {
        return Err(AgentApiError {
            status: 404,
            code: "AGENT_JOB_NOT_FOUND",
            detail: format!("job {job_id} is not assigned to node {node_id}"),
        });
    }
    Ok(())
}

fn lock_store(
    store: &Mutex<DurableJobStore>,
) -> Result<std::sync::MutexGuard<'_, DurableJobStore>, AgentApiError> {
    store.lock().map_err(|_| AgentApiError {
        status: 500,
        code: "AGENT_JOB_STORE_UNAVAILABLE",
        detail: "job store lock is unavailable".to_string(),
    })
}

fn lease_token() -> Result<String, AgentApiError> {
    let mut bytes = [0_u8; 32];
    random_fill(&mut bytes).map_err(|_| AgentApiError {
        status: 500,
        code: "AGENT_RANDOM_SOURCE_UNAVAILABLE",
        detail: "failed to generate a lease token".to_string(),
    })?;
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(i64::MAX as u128) as i64
}

#[derive(Debug)]
struct AgentApiError {
    status: u16,
    code: &'static str,
    detail: String,
}

fn job_error(error: JobError) -> AgentApiError {
    let status = match error {
        JobError::NotFound(_) => 404,
        JobError::IdempotencyConflict
        | JobError::StaleLease
        | JobError::InvalidTransition { .. }
        | JobError::EventConflict { .. }
        | JobError::JobIdConflict(_) => 409,
        JobError::InvalidJob(_) => 422,
        JobError::Persistence(_) => 500,
    };
    AgentApiError {
        status,
        code: "AGENT_JOB_STATE_REJECTED",
        detail: error.to_string(),
    }
}

fn operation_error(error: OperationError) -> AgentApiError {
    let status = match error {
        OperationError::NotFound(_) => 500,
        OperationError::IdempotencyConflict | OperationError::InvalidTransition { .. } => 409,
        OperationError::InvalidPlan(_) | OperationError::Store(_) | OperationError::Job(_) => 500,
    };
    AgentApiError {
        status,
        code: "AGENT_OPERATION_PROJECTION_FAILED",
        detail: format!("job completed but its Operation projection failed: {error}"),
    }
}

fn invalid_json(error: serde_json::Error) -> AgentApiError {
    AgentApiError {
        status: 400,
        code: "AGENT_REQUEST_INVALID",
        detail: error.to_string(),
    }
}

fn invalid(detail: impl Into<String>) -> AgentApiError {
    AgentApiError {
        status: 422,
        code: "AGENT_REQUEST_INVALID",
        detail: detail.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use orchestrator_control_plane::{
        DurableOperationStatus, Job, JobKind, NewJob, OperationRepository, PlanOperation,
        PlannedJob,
    };
    use orchestrator_legacy::{NodeRecord, OrchestratorStore};
    use orchestrator_storage::{SqliteJobStore, SqliteOperationStore, SqliteOrchestratorStore};
    use serde_json::json;
    use tempfile::tempdir;

    #[test]
    fn agent_path_is_scoped_to_internal_protocol() {
        assert!(is_agent_path("/api/v1/agent/nodes/node-1/jobs:claim"));
        assert!(!is_agent_path("/api/v1/nodes"));
    }

    #[test]
    fn protocol_v1_requires_the_exact_capability_set() {
        let exact = REQUIRED_V1_AGENT_CAPABILITIES
            .iter()
            .map(|value| (*value).to_string())
            .collect::<Vec<_>>();
        assert!(validate_claim_capabilities(&exact).is_ok());

        let partial = vec!["health".to_string()];
        let error = validate_claim_capabilities(&partial).unwrap_err();
        assert_eq!(error.status, 422);
        assert_eq!(error.code, "AGENT_REQUEST_INVALID");

        let mut duplicated = exact;
        duplicated.push("health".to_string());
        assert!(validate_claim_capabilities(&duplicated).is_err());
    }

    #[test]
    fn agent_claim_hot_path_does_not_run_expired_lease_recovery() {
        let directory = tempdir().unwrap();
        let mut storage =
            SqliteOrchestratorStore::open(directory.path().join("claim-hot-path.db")).unwrap();
        storage
            .upsert_node(NodeRecord {
                node_id: "node-claim-hot-path".to_string(),
                host_ip: "127.0.0.9".to_string(),
                parent_node_id: String::new(),
                role: "standalone".to_string(),
                labels: json!({}),
                status: "READY".to_string(),
                created_at: "t0".to_string(),
                updated_at: "t0".to_string(),
            })
            .unwrap();
        let durable = DurableStore::Sqlite(storage);
        let mut job_store = durable.job_store();
        job_store
            .enqueue(
                NewJob {
                    job_id: "expired-but-not-recovered-by-claim".to_string(),
                    operation_id: "op-claim-hot-path".to_string(),
                    node_id: "node-claim-hot-path".to_string(),
                    kind: JobKind::Health,
                    payload: json!({}),
                    idempotency_key: "claim-hot-path".to_string(),
                    max_attempts: 3,
                },
                1,
            )
            .unwrap();
        job_store
            .claim(ClaimRequest {
                node_id: "node-claim-hot-path".to_string(),
                instance_id: "agent-old".to_string(),
                lease_token: "old-token".to_string(),
                now_ms: 1,
                lease_ms: 1,
            })
            .unwrap();
        let jobs = Mutex::new(job_store);
        let response = route(
            Some(&durable),
            Some(&jobs),
            ApiRequest {
                method: "POST".to_string(),
                path: "/api/v1/agent/nodes/node-claim-hot-path/jobs:claim".to_string(),
                headers: Default::default(),
                body: json!({
                    "instance_id": "agent-new",
                    "protocol_version": "v1",
                    "capabilities": REQUIRED_V1_AGENT_CAPABILITIES,
                    "max_jobs": 1
                })
                .to_string(),
            },
        );
        assert_eq!(response.status, 200);
        assert!(response.body["jobs"].as_array().unwrap().is_empty());
        assert_eq!(
            jobs.lock()
                .unwrap()
                .get("expired-but-not-recovered-by-claim")
                .unwrap()
                .unwrap()
                .status,
            JobStatus::Leased
        );
    }

    #[test]
    fn agent_claim_long_poll_waits_outside_the_queue_lock_and_wakes_for_a_job() {
        let directory = tempdir().unwrap();
        let mut storage =
            SqliteOrchestratorStore::open(directory.path().join("long-poll.db")).unwrap();
        storage
            .upsert_node(NodeRecord {
                node_id: "node-long-poll".to_string(),
                host_ip: "127.0.0.8".to_string(),
                parent_node_id: String::new(),
                role: "standalone".to_string(),
                labels: json!({}),
                status: "READY".to_string(),
                created_at: "t0".to_string(),
                updated_at: "t0".to_string(),
            })
            .unwrap();
        let durable = DurableStore::Sqlite(storage);
        let jobs = Mutex::new(durable.job_store());
        let mut operations = durable.operation_store();
        {
            let mut locked_jobs = jobs.lock().unwrap();
            let mut coordinator = OperationCoordinator::new(&mut operations, &mut *locked_jobs);
            coordinator
                .plan(
                    PlanOperation {
                        operation_id: "op-long-poll".to_string(),
                        action: "deployment.health".to_string(),
                        target_type: "Deployment".to_string(),
                        target_id: "deployment-long-poll".to_string(),
                        request: json!({}),
                        jobs: vec![PlannedJob {
                            step_id: "health".to_string(),
                            node_id: "node-long-poll".to_string(),
                            kind: JobKind::Health,
                            depends_on: vec![],
                            condition: Default::default(),
                            payload: json!({"container_id": "container-long-poll"}),
                            max_attempts: 3,
                        }],
                    },
                    1,
                )
                .unwrap();
            coordinator.confirm("op-long-poll", 2).unwrap();
        }

        let started = Instant::now();
        std::thread::scope(|scope| {
            let claim = scope.spawn(|| {
                route(
                    Some(&durable),
                    Some(&jobs),
                    ApiRequest {
                        method: "POST".to_string(),
                        path: "/api/v1/agent/nodes/node-long-poll/jobs:claim".to_string(),
                        headers: [("prefer".to_string(), AGENT_LONG_POLL_PREFERENCE.to_string())]
                            .into(),
                        body: json!({
                            "instance_id": "agent-long-poll",
                            "protocol_version": "v1",
                            "capabilities": REQUIRED_V1_AGENT_CAPABILITIES,
                            "max_jobs": 1
                        })
                        .to_string(),
                    },
                )
            });
            std::thread::sleep(Duration::from_millis(100));
            {
                let mut locked_jobs = jobs.lock().unwrap();
                OperationCoordinator::new(&mut operations, &mut *locked_jobs)
                    .enqueue("op-long-poll", 3)
                    .unwrap();
            }
            let response = claim.join().unwrap();
            assert_eq!(response.status, 200);
            assert_eq!(response.body["jobs"].as_array().unwrap().len(), 1);
            assert_eq!(
                response
                    .headers
                    .get("Preference-Applied")
                    .map(String::as_str),
                Some(AGENT_LONG_POLL_PREFERENCE)
            );
        });
        assert!(started.elapsed() >= Duration::from_millis(100));
        assert!(started.elapsed() < Duration::from_secs(3));
    }

    #[test]
    fn agent_claim_rejects_non_v1_long_poll_preferences() {
        let request = ApiRequest {
            method: "POST".to_string(),
            path: "/api/v1/agent/nodes/node-1/jobs:claim".to_string(),
            headers: [("prefer".to_string(), "wait=60".to_string())].into(),
            body: String::new(),
        };
        let error = claim_wait(&request).unwrap_err();
        assert_eq!(error.status, 422);
        assert_eq!(
            claim_wait(&ApiRequest {
                headers: Default::default(),
                ..request
            })
            .unwrap(),
            Duration::ZERO
        );
    }

    #[test]
    fn agent_protocol_rejects_jwt_bearer_credentials() {
        let response = route_authenticated(
            None,
            None,
            None,
            None,
            AgentCaller::AnonymousTls,
            ApiRequest {
                method: "POST".into(),
                path: "/api/v1/agent/nodes/node-1/jobs:claim".into(),
                headers: std::collections::BTreeMap::from([(
                    "authorization".into(),
                    "Bearer legacy-node-token".into(),
                )]),
                body: "{}".into(),
            },
        );
        assert_eq!(response.status, 401);
        assert_eq!(response.body["code"], "AGENT_BEARER_FORBIDDEN");
    }

    #[test]
    fn certificate_lifecycle_routes_accept_only_post() {
        let directory = tempdir().unwrap();
        let storage = DurableStore::Sqlite(
            SqliteOrchestratorStore::open(directory.path().join("orchestrator.db")).unwrap(),
        );
        for path in [
            "/api/v1/agent/enroll",
            "/api/v1/agent/certificates:renew",
            "/api/v1/agent/certificates:activate",
        ] {
            let response = route_authenticated(
                Some(&storage),
                None,
                None,
                None,
                AgentCaller::AnonymousTls,
                ApiRequest {
                    method: "GET".into(),
                    path: path.into(),
                    headers: Default::default(),
                    body: String::new(),
                },
            );
            assert_eq!(response.status, 405, "{path}");
            assert_eq!(response.body["code"], "AGENT_METHOD_NOT_ALLOWED");
        }
    }

    #[test]
    fn certificate_node_must_match_the_agent_path_node() {
        let directory = tempdir().unwrap();
        let storage = DurableStore::Sqlite(
            SqliteOrchestratorStore::open(directory.path().join("orchestrator.db")).unwrap(),
        );
        let peer = NodePeerIdentity {
            node_id: "node-1".into(),
            spiffe_id: "spiffe://ojos.local/node/node-1".into(),
            serial_hex: "01".into(),
            fingerprint_sha256: "sha256:test".into(),
        };
        let response = route_authenticated(
            Some(&storage),
            None,
            None,
            None,
            AgentCaller::Mtls(&peer),
            ApiRequest {
                method: "POST".into(),
                path: "/api/v1/agent/nodes/node-2/jobs:claim".into(),
                headers: std::collections::BTreeMap::new(),
                body: "{}".into(),
            },
        );
        assert_eq!(response.status, 403);
        assert_eq!(response.body["code"], "AGENT_IDENTITY_MISMATCH");
    }

    #[test]
    fn identity_probe_returns_only_the_authenticated_peer_binding() {
        let peer = NodePeerIdentity {
            node_id: "node-1".into(),
            spiffe_id: "spiffe://ojos.local/node/node-1".into(),
            serial_hex: "0a".into(),
            fingerprint_sha256: "sha256:test".into(),
        };
        let response = verify_identity(AgentCaller::Mtls(&peer)).unwrap();
        assert_eq!(response.status, 200);
        assert_eq!(response.body["node_id"], "node-1");
        assert_eq!(response.body["spiffe_id"], peer.spiffe_id);
        assert_eq!(response.body["serial_hex"], "0a");
        assert_eq!(response.body["status"], "ACTIVE");
        assert!(matches!(
            verify_identity(AgentCaller::LocalBootstrap { node_id: "node-1" }),
            Err(AgentApiError {
                code: "AGENT_MTLS_REQUIRED",
                ..
            })
        ));
    }

    fn runtime_instance(
        deployment_id: &str,
        service_id: &str,
        release_version: &str,
        container_id: &str,
    ) -> Value {
        json!({
            "deployment_id": deployment_id,
            "service_id": service_id,
            "release_version": release_version,
            "container_id": container_id,
            "artifact_digest": format!("sha256:{}", "a".repeat(64)),
            "desired_state": "RUNNING",
            "observed_state": "RUNNING",
            "health": "HEALTHY",
        })
    }

    #[test]
    fn successful_replacement_atomically_swaps_runtime_projection() {
        let directory = tempdir().unwrap();
        let storage =
            SqliteOrchestratorStore::open(directory.path().join("orchestrator.db")).unwrap();
        let durable = DurableStore::Sqlite(storage);
        let old: orchestrator_storage::StoredRuntimeInstance = serde_json::from_value(json!({
            "node_id": "node-1",
            "instance": runtime_instance("deployment-old", "service-1", "1.0.0", "container-old"),
            "endpoint": "127.0.0.2:20000:service-1",
            "updated_at": "t0",
        }))
        .unwrap();
        durable.put_runtime_instance(&old).unwrap();
        let completed = Job {
            job_id: "job-upgrade".to_string(),
            operation_id: "op-upgrade".to_string(),
            node_id: "node-1".to_string(),
            kind: JobKind::Upgrade,
            payload: json!({
                "old_deployment_id": "deployment-old",
                "old_container_id": "container-old",
                "new_spec": {
                    "labels": {"ojos.release_version": "2.0.0"},
                    "published_endpoint": {
                        "endpoint": "127.0.0.2:20001:service-1"
                    }
                }
            }),
            payload_sha256: "hash".to_string(),
            idempotency_key: "upgrade-1".to_string(),
            status: JobStatus::Succeeded,
            attempt: 1,
            max_attempts: 3,
            available_at_ms: 0,
            lease_owner: None,
            lease_token: Some("lease".to_string()),
            lease_expires_at_ms: None,
            result: Some(json!({
                "action": "upgrade",
                "instance": runtime_instance("deployment-new", "service-1", "2.0.0", "container-new"),
                "replaced_deployment_id": "deployment-old",
                "replaced_container_id": "container-old",
            })),
            error_message: None,
            completion_fingerprint: Some("fingerprint".to_string()),
            created_at_ms: 1,
            started_at_ms: Some(2),
            completed_at_ms: Some(3),
            updated_at_ms: 3,
        };

        project_runtime_instance(&durable, &completed).unwrap();
        project_runtime_instance(&durable, &completed).unwrap();

        assert!(
            durable
                .runtime_instance("deployment-old")
                .unwrap()
                .is_none()
        );
        assert_eq!(
            durable
                .runtime_instance("deployment-new")
                .unwrap()
                .unwrap()
                .instance
                .container_id,
            "container-new"
        );
        let replacement = durable.runtime_instance("deployment-new").unwrap().unwrap();
        assert_eq!(replacement.instance.release_version, "2.0.0");
        assert_eq!(replacement.endpoint, "127.0.0.2:20001:service-1");
    }

    #[test]
    fn artifact_chunks_require_the_assigned_active_lease() {
        let directory = tempdir().unwrap();
        let layout = directory.path().join("oci-layout");
        std::fs::create_dir_all(&layout).unwrap();
        std::fs::write(
            layout.join("oci-layout"),
            br#"{"imageLayoutVersion":"1.0.0"}"#,
        )
        .unwrap();
        let artifact_store = ArtifactStore::open(&directory.path().join("artifacts")).unwrap();
        let reference = artifact_store.create_oci_archive(&layout).unwrap();

        let mut storage =
            SqliteOrchestratorStore::open(directory.path().join("orchestrator.db")).unwrap();
        storage
            .upsert_node(NodeRecord {
                node_id: "node-artifact".to_string(),
                host_ip: "127.0.0.9".to_string(),
                parent_node_id: String::new(),
                role: "standalone".to_string(),
                labels: json!({}),
                status: "READY".to_string(),
                created_at: "t0".to_string(),
                updated_at: "t0".to_string(),
            })
            .unwrap();
        let durable = DurableStore::Sqlite(storage.clone());
        let mut sqlite_jobs = SqliteJobStore::new(storage);
        let enqueued_at = now_ms();
        sqlite_jobs
            .enqueue(
                NewJob {
                    job_id: "job-artifact".to_string(),
                    operation_id: "op-artifact".to_string(),
                    node_id: "node-artifact".to_string(),
                    kind: JobKind::Install,
                    payload: json!({"offline_oci_artifact": reference}),
                    idempotency_key: "artifact-install-1".to_string(),
                    max_attempts: 3,
                },
                enqueued_at,
            )
            .unwrap();
        sqlite_jobs
            .claim(ClaimRequest {
                node_id: "node-artifact".to_string(),
                instance_id: "agent-artifact".to_string(),
                lease_token: "lease-artifact".to_string(),
                now_ms: enqueued_at,
                lease_ms: DEFAULT_LEASE_MS,
            })
            .unwrap()
            .unwrap();
        let jobs = Mutex::new(DurableJobStore::Sqlite(sqlite_jobs));

        let chunk = route_authenticated(
            Some(&durable),
            Some(&jobs),
            Some(&artifact_store),
            None,
            AgentCaller::LocalBootstrap {
                node_id: "node-artifact",
            },
            ApiRequest {
                method: "GET".to_string(),
                path: format!(
                    "/api/v1/agent/nodes/node-artifact/jobs/job-artifact/artifacts/{}?offset=0&length=64",
                    reference.artifact_id
                ),
                headers: std::collections::BTreeMap::from([(
                    "x-ojos-lease-token".to_string(),
                    "lease-artifact".to_string(),
                )]),
                body: String::new(),
            },
        );
        assert_eq!(chunk.status, 200, "{}", chunk.body);
        assert_eq!(chunk.body["artifact_id"], json!(reference.artifact_id));
        assert_eq!(chunk.body["sha256"], json!(reference.sha256));
        assert_eq!(chunk.body["total_size"], json!(reference.size_bytes));
        assert_eq!(
            BASE64_STANDARD
                .decode(chunk.body["data_base64"].as_str().unwrap())
                .unwrap()
                .len(),
            64
        );
        assert_eq!(
            chunk.headers.get("Cache-Control").map(String::as_str),
            Some("no-store")
        );

        let stale = route_authenticated(
            Some(&durable),
            Some(&jobs),
            Some(&artifact_store),
            None,
            AgentCaller::LocalBootstrap {
                node_id: "node-artifact",
            },
            ApiRequest {
                method: "GET".to_string(),
                path: format!(
                    "/api/v1/agent/nodes/node-artifact/jobs/job-artifact/artifacts/{}",
                    reference.artifact_id
                ),
                headers: std::collections::BTreeMap::from([(
                    "x-ojos-lease-token".to_string(),
                    "stale-lease".to_string(),
                )]),
                body: String::new(),
            },
        );
        assert_eq!(stale.status, 409);
        assert_eq!(stale.body["code"], "AGENT_ARTIFACT_STALE_LEASE");

        let unassigned_id = "f".repeat(64);
        let unassigned = route_authenticated(
            Some(&durable),
            Some(&jobs),
            Some(&artifact_store),
            None,
            AgentCaller::LocalBootstrap {
                node_id: "node-artifact",
            },
            ApiRequest {
                method: "GET".to_string(),
                path: format!(
                    "/api/v1/agent/nodes/node-artifact/jobs/job-artifact/artifacts/{unassigned_id}"
                ),
                headers: std::collections::BTreeMap::from([(
                    "x-ojos-lease-token".to_string(),
                    "lease-artifact".to_string(),
                )]),
                body: String::new(),
            },
        );
        assert_eq!(unassigned.status, 404);
        assert_eq!(unassigned.body["code"], "AGENT_ARTIFACT_NOT_ASSIGNED");
    }

    #[test]
    fn claim_heartbeat_and_complete_use_the_durable_job_store() {
        let directory = tempdir().unwrap();
        let mut storage =
            SqliteOrchestratorStore::open(directory.path().join("orchestrator.db")).unwrap();
        storage
            .upsert_node(NodeRecord {
                node_id: "node-1".to_string(),
                host_ip: "127.0.0.2".to_string(),
                parent_node_id: String::new(),
                role: "standalone".to_string(),
                labels: json!({}),
                status: "READY".to_string(),
                created_at: "t0".to_string(),
                updated_at: "t0".to_string(),
            })
            .unwrap();
        let durable = DurableStore::Sqlite(storage.clone());
        durable
            .put_runtime_instance(
                &serde_json::from_value(json!({
                    "node_id": "node-1",
                    "instance": runtime_instance(
                        "deployment-1",
                        "service-1",
                        "1.0.0",
                        "container-old"
                    ),
                    "endpoint": "127.0.0.2:20000:service-1",
                    "updated_at": "t0"
                }))
                .unwrap(),
            )
            .unwrap();
        let mut operations = SqliteOperationStore::new(storage.clone());
        let mut jobs = SqliteJobStore::new(storage);
        {
            let mut coordinator = OperationCoordinator::new(&mut operations, &mut jobs);
            coordinator
                .plan(
                    PlanOperation {
                        operation_id: "op-1".to_string(),
                        action: "deployment.health".to_string(),
                        target_type: "deployment".to_string(),
                        target_id: "deployment-1".to_string(),
                        request: json!({}),
                        jobs: vec![PlannedJob {
                            step_id: "health".to_string(),
                            node_id: "node-1".to_string(),
                            kind: JobKind::Health,
                            depends_on: vec![],
                            condition: Default::default(),
                            payload: json!({"deployment_id": "deployment-1"}),
                            max_attempts: 3,
                        }],
                    },
                    1,
                )
                .unwrap();
            coordinator.confirm("op-1", 2).unwrap();
            coordinator.enqueue("op-1", 3).unwrap();
        }
        let jobs = Mutex::new(DurableJobStore::Sqlite(jobs));
        let claim = route(
            Some(&durable),
            Some(&jobs),
            ApiRequest {
                method: "POST".to_string(),
                path: "/api/v1/agent/nodes/node-1/jobs:claim".to_string(),
                headers: Default::default(),
                body: json!({
                    "instance_id": "agent-1",
                    "protocol_version": "v1",
                    "capabilities": REQUIRED_V1_AGENT_CAPABILITIES,
                    "max_jobs": 1
                })
                .to_string(),
            },
        );
        assert_eq!(claim.status, 200);
        assert!(
            durable
                .get_node("node-1")
                .unwrap()
                .unwrap()
                .updated_at
                .starts_with("unix-ms:")
        );
        let token = claim.body["jobs"][0]["lease_token"]
            .as_str()
            .unwrap()
            .to_string();
        let job_id = claim.body["jobs"][0]["job_id"]
            .as_str()
            .unwrap()
            .to_string();
        let heartbeat = route(
            Some(&durable),
            Some(&jobs),
            ApiRequest {
                method: "POST".to_string(),
                path: format!("/api/v1/agent/nodes/node-1/jobs/{job_id}:heartbeat"),
                headers: Default::default(),
                body: json!({"lease_token": token, "events": []}).to_string(),
            },
        );
        assert_eq!(heartbeat.status, 200);
        let complete = route(
            Some(&durable),
            Some(&jobs),
            ApiRequest {
                method: "POST".to_string(),
                path: format!("/api/v1/agent/nodes/node-1/jobs/{job_id}:complete"),
                headers: Default::default(),
                body: json!({
                    "lease_token": token,
                    "status": "SUCCEEDED",
                    "result": {"instance": runtime_instance("deployment-1", "service-1", "1.0.0", "container-1")},
                    "error_message": "",
                    "events": []
                })
                .to_string(),
            },
        );
        assert_eq!(complete.status, 204);
        assert_eq!(
            jobs.lock().unwrap().get(&job_id).unwrap().unwrap().status,
            JobStatus::Succeeded
        );
        assert_eq!(
            operations.get("op-1").unwrap().unwrap().status,
            DurableOperationStatus::Succeeded
        );
        assert_eq!(
            durable
                .runtime_instance("deployment-1")
                .unwrap()
                .unwrap()
                .instance
                .container_id,
            "container-1"
        );
        let projection = durable.runtime_instance("deployment-1").unwrap().unwrap();
        assert_eq!(projection.instance.release_version, "1.0.0");
        assert_eq!(projection.endpoint, "127.0.0.2:20000:service-1");
    }

    #[test]
    fn operation_api_to_agent_completion_closes_the_operation() {
        let directory = tempdir().unwrap();
        let mut storage =
            SqliteOrchestratorStore::open(directory.path().join("orchestrator.db")).unwrap();
        storage
            .upsert_node(NodeRecord {
                node_id: "node-e2e".to_string(),
                host_ip: "127.0.0.3".to_string(),
                parent_node_id: String::new(),
                role: "standalone".to_string(),
                labels: json!({}),
                status: "READY".to_string(),
                created_at: "t0".to_string(),
                updated_at: "t0".to_string(),
            })
            .unwrap();

        let plan_request = ApiRequest {
            method: "POST".to_string(),
            path: "/api/v1/operations:plan".to_string(),
            headers: Default::default(),
            body: serde_json::to_string(&PlanOperation {
                operation_id: "op-e2e".to_string(),
                action: "deployment.start".to_string(),
                target_type: "deployment".to_string(),
                target_id: "deployment-e2e".to_string(),
                request: json!({}),
                jobs: vec![PlannedJob {
                    step_id: "start".to_string(),
                    node_id: "node-e2e".to_string(),
                    kind: JobKind::Start,
                    depends_on: vec![],
                    condition: Default::default(),
                    payload: json!({"deployment_id": "deployment-e2e"}),
                    max_attempts: 3,
                }],
            })
            .unwrap(),
        };
        let durable = DurableStore::Sqlite(storage.clone());
        durable
            .put_runtime_instance(
                &serde_json::from_value(json!({
                    "node_id": "node-e2e",
                    "instance": runtime_instance(
                        "deployment-e2e",
                        "service-e2e",
                        "1.0.0",
                        "container-old"
                    ),
                    "endpoint": "127.0.0.3:20000:service-e2e",
                    "updated_at": "t0"
                }))
                .unwrap(),
            )
            .unwrap();
        let planned = crate::operation_api::route(Some(&durable), &plan_request, "req-plan")
            .expect("operation route");
        assert_eq!(planned.status, 201);

        let confirmed = crate::operation_api::route(
            Some(&durable),
            &ApiRequest {
                method: "POST".to_string(),
                path: "/api/v1/operations/op-e2e:confirm".to_string(),
                headers: Default::default(),
                body: String::new(),
            },
            "req-confirm",
        )
        .expect("operation route");
        assert_eq!(confirmed.status, 200);
        let applied = crate::operation_api::route(
            Some(&durable),
            &ApiRequest {
                method: "POST".to_string(),
                path: "/api/v1/operations/op-e2e:apply".to_string(),
                headers: Default::default(),
                body: String::new(),
            },
            "req-apply",
        )
        .expect("operation route");
        assert_eq!(applied.status, 202);

        let jobs = Mutex::new(durable.job_store());
        let claim = route(
            Some(&durable),
            Some(&jobs),
            ApiRequest {
                method: "POST".to_string(),
                path: "/api/v1/agent/nodes/node-e2e/jobs:claim".to_string(),
                headers: Default::default(),
                body: json!({
                    "instance_id": "agent-e2e",
                    "protocol_version": "v1",
                    "capabilities": REQUIRED_V1_AGENT_CAPABILITIES,
                    "max_jobs": 1
                })
                .to_string(),
            },
        );
        assert_eq!(claim.status, 200);
        let job_id = claim.body["jobs"][0]["job_id"].as_str().unwrap();
        let lease_token = claim.body["jobs"][0]["lease_token"].as_str().unwrap();
        let completed = route(
            Some(&durable),
            Some(&jobs),
            ApiRequest {
                method: "POST".to_string(),
                path: format!("/api/v1/agent/nodes/node-e2e/jobs/{job_id}:complete"),
                headers: Default::default(),
                body: json!({
                    "lease_token": lease_token,
                    "status": "SUCCEEDED",
                    "result": {"instance": runtime_instance("deployment-e2e", "service-e2e", "1.0.0", "container-e2e")},
                    "events": []
                })
                .to_string(),
            },
        );
        assert_eq!(completed.status, 204);

        let fetched = crate::operation_api::route(
            Some(&durable),
            &ApiRequest {
                method: "GET".to_string(),
                path: "/api/v1/operations/op-e2e".to_string(),
                headers: Default::default(),
                body: String::new(),
            },
            "req-get",
        )
        .expect("operation route");
        assert_eq!(fetched.status, 200);
        assert_eq!(
            fetched.body["data"]["operation"]["status"],
            json!("SUCCEEDED")
        );
        assert_eq!(
            fetched.body["data"]["operation"]["result"]["start"]["result"]["instance"]["container_id"],
            json!("container-e2e")
        );
    }
}
