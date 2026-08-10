use crate::artifact_store::{ArtifactStore, ArtifactStoreError, DEFAULT_CHUNK_BYTES};
use crate::durable::{DurableError, DurableJobStore, DurableStore};
use crate::http::{ApiRequest, ApiResponse};
use crate::node_identity::{NodeIdentityService, NodePeerIdentity};
use crate::topology_provider::TopologyProviderSaga;
use crate::topology_worker::{
    reconcile_runtime_binding_projections, runtime_preserves_active_binding_route,
};
use crate::workload_credentials::{
    WORKLOAD_TOKEN_TTL_SECONDS, WorkloadTokenIssuer, WorkloadTokenRequest,
};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use getrandom::fill as random_fill;
use orchestrator_agent::NodeRuntimeFactsV1;
use orchestrator_control_plane::{
    ClaimRequest, CompleteRequest, CompletionStatus, DEFAULT_LEASE_MS, DEFAULT_LONG_POLL_MS,
    HeartbeatRequest, JobError, JobStatus, JobStore, NewJobEvent, OperationCoordinator,
    OperationError,
};
use orchestrator_runtime::{
    ArtifactReference, BindingContextApplyPayload, ManagedServiceContextProjection,
    ManagedServiceContextSpec, OciImageReference, RuntimeDesiredState, RuntimeInstance,
    RuntimeObservedState, RuntimeProfile,
};
use orchestrator_storage::{
    ApiBindingState, RuntimeManagementMode, StoredNodeRuntimeFacts, StoredRuntimeInstance,
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::BTreeSet;
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const AGENT_LONG_POLL_PREFERENCE: &str = "wait=25";
const CLAIM_POLL_INTERVAL: Duration = Duration::from_millis(500);
const RUNTIME_FACTS_MAX_CLOCK_SKEW_MS: i64 = 5 * 60 * 1_000;
const RUNTIME_REPORT_CAS_RETRIES: usize = 64;
const MANAGED_CONTEXT_STATE_NAMESPACE: &str = "managed-service-context-v1";

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
    "binding_context_apply",
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

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct WorkloadCredentialExchangeBody {
    deployment_id: String,
    #[serde(default)]
    job_id: String,
    #[serde(default)]
    lease_token: String,
}

#[derive(Debug, Clone, Copy)]
pub(crate) enum AgentCaller<'a> {
    LocalBootstrap { node_id: &'a str },
    Mtls(&'a NodePeerIdentity),
    AnonymousTls,
}

#[derive(Clone, Copy)]
pub(crate) struct AgentRouteContext<'a> {
    pub(crate) storage: Option<&'a DurableStore>,
    pub(crate) jobs: Option<&'a Mutex<DurableJobStore>>,
    pub(crate) artifact_store: Option<&'a ArtifactStore>,
    pub(crate) identity_service: Option<&'a NodeIdentityService>,
    pub(crate) workload_token_issuer: Option<&'a dyn WorkloadTokenIssuer>,
    pub(crate) topology_provider: Option<&'a TopologyProviderSaga>,
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
        AgentRouteContext {
            storage,
            jobs,
            artifact_store: None,
            identity_service: None,
            workload_token_issuer: None,
            topology_provider: None,
        },
        AgentCaller::LocalBootstrap {
            node_id: &local_node_id,
        },
        request,
    )
}

pub(crate) fn route_authenticated(
    context: AgentRouteContext<'_>,
    caller: AgentCaller<'_>,
    request: ApiRequest,
) -> ApiResponse {
    let AgentRouteContext {
        storage,
        jobs,
        artifact_store,
        identity_service,
        workload_token_issuer,
        topology_provider,
    } = context;
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
    if let ["api", "v1", "agent", "nodes", node_id, "runtime-facts"] = segments.as_slice() {
        if request.method != "PUT" {
            return ApiResponse::problem(
                405,
                "AGENT_METHOD_NOT_ALLOWED",
                "Node runtime facts require PUT",
                "req-agent-runtime-facts",
                None,
            );
        }
        return match put_runtime_facts(storage, topology_provider, caller, node_id, &request) {
            Ok(response) => response,
            Err(error) => ApiResponse::problem(
                error.status,
                error.code,
                error.detail,
                "req-agent-runtime-facts",
                None,
            ),
        };
    }
    if let [
        "api",
        "v1",
        "agent",
        "nodes",
        node_id,
        "workload-credentials:exchange",
    ] = segments.as_slice()
    {
        if request.method != "POST" {
            return ApiResponse::problem(
                405,
                "AGENT_METHOD_NOT_ALLOWED",
                "workload credential exchange requires POST",
                "req-agent-workload-credential",
                None,
            );
        }
        return match exchange_workload_credential(
            storage,
            jobs,
            workload_token_issuer,
            caller,
            node_id,
            &request,
        ) {
            Ok(response) => response,
            Err(error) => ApiResponse::problem(
                error.status,
                error.code,
                error.detail,
                "req-agent-workload-credential",
                None,
            ),
        };
    }
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
    match route_with_store(
        storage,
        jobs,
        artifact_store,
        topology_provider,
        &request,
        &segments,
    ) {
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

fn put_runtime_facts(
    storage: &DurableStore,
    topology_provider: Option<&TopologyProviderSaga>,
    caller: AgentCaller<'_>,
    node_id: &str,
    request: &ApiRequest,
) -> Result<ApiResponse, AgentApiError> {
    if matches!(caller, AgentCaller::AnonymousTls) {
        return Err(AgentApiError {
            status: 401,
            code: "AGENT_MTLS_REQUIRED",
            detail: "runtime facts require Node mTLS or the Desktop loopback bootstrap identity"
                .to_string(),
        });
    }
    let facts: NodeRuntimeFactsV1 = serde_json::from_str(&request.body).map_err(invalid_json)?;
    validate_runtime_facts(&facts, now_ms())?;
    let received_at_ms = now_ms();
    let serialized_facts = serde_json::to_value(&facts).map_err(invalid_json)?;
    let stored_facts = StoredNodeRuntimeFacts {
        node_id: node_id.to_string(),
        observed_at_ms: facts.observed_at_ms,
        received_at_ms,
        facts: serialized_facts,
    };
    for attempt in 0..RUNTIME_REPORT_CAS_RETRIES {
        if runtime_report_already_accepted(storage, node_id, &stored_facts)? {
            catch_up_latest_complete_runtime_report(storage, node_id)?;
            reconcile_runtime_report_bindings(storage, topology_provider, node_id)?;
            return Ok(ApiResponse::no_content(Value::Null));
        }
        let projection = runtime_report_projections(storage, node_id, &facts)?;
        match storage.apply_node_runtime_report(
            &stored_facts,
            projection.expected_managed_deployment_ids.as_deref(),
            &projection.updates,
        ) {
            Ok(()) => {
                // The report projection was planned before its storage
                // transaction acquired the Node lock. A completion can insert
                // a new RuntimeInstance in that interval (notably a
                // PostgreSQL phantom when the planned set was empty). Re-read
                // the now-durable report and runtime set until their exact
                // replay reaches a CAS-protected fixed point before returning.
                catch_up_latest_complete_runtime_report(storage, node_id)?;
                reconcile_runtime_report_bindings(storage, topology_provider, node_id)?;
                return Ok(ApiResponse::no_content(Value::Null));
            }
            Err(DurableError::Conflict(_)) if attempt + 1 < RUNTIME_REPORT_CAS_RETRIES => {
                // A lifecycle mutation or another newer report won after the
                // projection snapshot was read. Re-read both facts and
                // RuntimeInstances, then derive a new projection. The storage
                // transaction still performs the final row CAS, so this retry
                // never overwrites a lifecycle update computed in parallel.
            }
            Err(error) => {
                let conflict = matches!(error, DurableError::Conflict(_));
                return Err(AgentApiError {
                    status: if conflict { 409 } else { 500 },
                    code: if conflict {
                        "AGENT_RUNTIME_REPORT_CONFLICT"
                    } else {
                        "AGENT_RUNTIME_FACTS_PERSIST_FAILED"
                    },
                    detail: error.to_string(),
                });
            }
        }
    }
    unreachable!("bounded runtime report CAS loop always returns")
}

fn runtime_report_already_accepted(
    storage: &DurableStore,
    node_id: &str,
    incoming: &StoredNodeRuntimeFacts,
) -> Result<bool, AgentApiError> {
    let Some(previous) = storage
        .node_runtime_facts(node_id)
        .map_err(|error| AgentApiError {
            status: 500,
            code: "AGENT_RUNTIME_FACTS_READ_FAILED",
            detail: error.to_string(),
        })?
    else {
        return Ok(false);
    };
    let previous_report_id = previous
        .facts
        .get("report_id")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let incoming_report_id = incoming
        .facts
        .get("report_id")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if previous_report_id == incoming_report_id {
        if previous.facts == incoming.facts {
            return Ok(true);
        }
        return Err(AgentApiError {
            status: 409,
            code: "AGENT_RUNTIME_REPORT_ID_REUSED",
            detail: "report_id was already accepted with different content".to_string(),
        });
    }
    if incoming.observed_at_ms <= previous.observed_at_ms {
        return Err(AgentApiError {
            status: 409,
            code: "AGENT_RUNTIME_REPORT_STALE",
            detail: "runtime report is older than the latest accepted Node report".to_string(),
        });
    }
    Ok(false)
}

fn validate_runtime_facts(facts: &NodeRuntimeFactsV1, now_ms: i64) -> Result<(), AgentApiError> {
    if facts.schema_version != 1 {
        return Err(invalid("runtime facts schema_version must be 1"));
    }
    if facts.agent_version.trim().is_empty() || facts.agent_version.len() > 128 {
        return Err(invalid("runtime facts require a bounded agent_version"));
    }
    if facts.report_id.trim().is_empty()
        || facts.report_id.len() > 256
        || facts.report_id.chars().any(char::is_control)
    {
        return Err(invalid("runtime facts require a bounded report_id"));
    }
    if !valid_sha256(&facts.runtime_policy_sha256) {
        return Err(invalid(
            "runtime facts runtime_policy_sha256 must be sha256:<64 lowercase hex>",
        ));
    }
    if facts.observed_at_ms < 0
        || facts.observed_at_ms.abs_diff(now_ms) > RUNTIME_FACTS_MAX_CLOCK_SKEW_MS as u64
    {
        return Err(invalid(
            "runtime facts observed_at_ms differs from control-plane time by more than 5 minutes",
        ));
    }
    if facts.allowed_contracts.is_empty() {
        return Err(invalid("runtime facts allowed_contracts must not be empty"));
    }
    let mut profiles = BTreeSet::<orchestrator_runtime::RuntimeProfile>::new();
    for contract in &facts.allowed_contracts {
        contract
            .validate()
            .map_err(|error| invalid(format!("runtime facts contract is invalid: {error}")))?;
        if !profiles.insert(contract.id) {
            return Err(invalid(
                "runtime facts contain a duplicate runtime contract",
            ));
        }
    }
    if facts.judge_sandbox_allowed_images.len() > 128 {
        return Err(invalid(
            "runtime facts judge sandbox artifact allowlist exceeds 128 entries",
        ));
    }
    let mut allowed_images = BTreeSet::new();
    for image in &facts.judge_sandbox_allowed_images {
        let parsed = OciImageReference::parse(image)
            .map_err(|error| invalid(format!("runtime facts allowed image is invalid: {error}")))?;
        if parsed.to_string() != *image || !allowed_images.insert(image) {
            return Err(invalid(
                "runtime facts allowed images must be unique canonical repository@sha256 references",
            ));
        }
    }
    let judge_allowed = profiles.contains(&RuntimeProfile::JudgeSandboxV1);
    if judge_allowed == facts.judge_sandbox_allowed_images.is_empty() {
        return Err(invalid(
            "judge-sandbox-v1 contract and its local artifact allowlist must be reported together",
        ));
    }
    if facts.redis_connection_ids.len() > 64 {
        return Err(invalid(
            "runtime facts Redis connection identifier list exceeds 64 entries",
        ));
    }
    let mut redis_connections = BTreeSet::new();
    for connection_id in &facts.redis_connection_ids {
        if connection_id.is_empty()
            || connection_id.len() > 128
            || !connection_id.chars().enumerate().all(|(index, value)| {
                value.is_ascii_alphanumeric()
                    || (index > 0 && matches!(value, '_' | '-' | '.' | ':'))
            })
            || !redis_connections.insert(connection_id)
        {
            return Err(invalid(
                "runtime facts Redis connection identifiers must be unique bounded tokens",
            ));
        }
    }
    if facts.inventory_complete != facts.inventory_error.is_empty() {
        return Err(invalid(
            "complete inventory must have no error and partial inventory must explain its error",
        ));
    }
    if facts.inventory_error.len() > 512 || facts.inventory_error.chars().any(char::is_control) {
        return Err(invalid(
            "runtime inventory error must be bounded printable text",
        ));
    }
    if facts.deployment_observations.len() > 4_096 || facts.credential_statuses.len() > 4_096 {
        return Err(invalid(
            "runtime report exceeds the bounded deployment inventory limit",
        ));
    }
    let mut deployment_ids = BTreeSet::new();
    let mut container_ids = BTreeSet::new();
    for observation in &facts.deployment_observations {
        for (name, value, max) in [
            ("deployment_id", observation.deployment_id.as_str(), 256),
            ("service_id", observation.service_id.as_str(), 256),
            ("container_id", observation.container_id.as_str(), 256),
            ("health", observation.health.as_str(), 32),
        ] {
            if value.trim().is_empty() || value.len() > max || value.chars().any(char::is_control) {
                return Err(invalid(format!(
                    "runtime observation {name} is empty or exceeds protocol bounds"
                )));
            }
        }
        observation.runtime_contract.validate().map_err(|error| {
            invalid(format!("runtime observation contract is invalid: {error}"))
        })?;
        if observation.drift_reason.len() > 512
            || observation.drift_reason.chars().any(char::is_control)
            || (!observation.runtime_attested && observation.drift_reason.trim().is_empty())
            || (observation.runtime_attested && !observation.drift_reason.is_empty())
        {
            return Err(invalid(
                "runtime observation drift reason must be bounded, printable, and present exactly when attestation failed",
            ));
        }
        if !deployment_ids.insert(observation.deployment_id.as_str())
            || !container_ids.insert(observation.container_id.as_str())
        {
            return Err(invalid(
                "runtime observations must have unique deployment and container IDs",
            ));
        }
        if observation.runtime_attested {
            OciImageReference::parse(&observation.artifact_digest).map_err(|error| {
                invalid(format!("attested runtime artifact is invalid: {error}"))
            })?;
            if !observation.runtime_policy_sha256.is_empty()
                && !valid_sha256(&observation.runtime_policy_sha256)
            {
                return Err(invalid(
                    "attested runtime policy digest is not a valid sha256",
                ));
            }
            if !observation.effective_runtime_sha256.is_empty()
                && !valid_sha256(&observation.effective_runtime_sha256)
            {
                return Err(invalid(
                    "attested effective runtime digest is not a valid sha256",
                ));
            }
        }
    }
    let mut credential_deployments = BTreeSet::new();
    for status in &facts.credential_statuses {
        if status.deployment_id.trim().is_empty()
            || status.deployment_id.len() > 256
            || status.deployment_id.chars().any(char::is_control)
            || status.expires_at_ms < 0
            || status.last_success_at_ms < 0
            || status.last_error.len() > 512
            || status.last_error.chars().any(char::is_control)
            || !credential_deployments.insert(status.deployment_id.as_str())
        {
            return Err(invalid(
                "credential supervisor status is duplicate or exceeds protocol bounds",
            ));
        }
    }
    let docker = &facts.docker;
    if docker.engine != "docker"
        || docker.server_version.trim().is_empty()
        || docker.operating_system.trim().is_empty()
        || docker.os_type.trim().is_empty()
        || docker.architecture.trim().is_empty()
        || docker.cgroup_version.trim().is_empty()
    {
        return Err(invalid("runtime facts Docker identity is incomplete"));
    }
    Ok(())
}

struct RuntimeReportProjection {
    expected_managed_deployment_ids: Option<Vec<String>>,
    updates: Vec<(StoredRuntimeInstance, StoredRuntimeInstance)>,
}

fn runtime_report_projections(
    storage: &DurableStore,
    node_id: &str,
    facts: &NodeRuntimeFactsV1,
) -> Result<RuntimeReportProjection, AgentApiError> {
    // Partial inventories are retained as Node evidence, but never mutate or
    // clear deployment state. Only a complete, monotonic report may prove a
    // container absent or clear a previous drift.
    if !facts.inventory_complete {
        return Ok(RuntimeReportProjection {
            expected_managed_deployment_ids: None,
            updates: Vec::new(),
        });
    }
    let observations = facts
        .deployment_observations
        .iter()
        .map(|observation| (observation.deployment_id.as_str(), observation))
        .collect::<std::collections::BTreeMap<_, _>>();
    let credentials = facts
        .credential_statuses
        .iter()
        .map(|status| (status.deployment_id.as_str(), status))
        .collect::<std::collections::BTreeMap<_, _>>();
    let mut projected = Vec::new();
    let mut expected_managed_deployments = Vec::new();
    for mut stored in storage
        .runtime_instances(Some(node_id))
        .map_err(|error| AgentApiError {
            status: 500,
            code: "AGENT_RUNTIME_PROJECTION_READ_FAILED",
            detail: error.to_string(),
        })?
    {
        if stored.management_mode != RuntimeManagementMode::Managed {
            continue;
        }
        expected_managed_deployments.push(stored.instance.deployment_id.clone());
        // A Job completion and a runtime report use the same Agent clock. If
        // the lifecycle result was observed at or after this inventory began,
        // the report cannot prove that result missing or drifted: it may have
        // enumerated Docker immediately before the Job created/changed the
        // container and only reached the control plane afterwards. The next
        // complete inventory has a newer watermark and will converge the
        // projection normally.
        if stored.last_observed_at_ms >= facts.observed_at_ms {
            continue;
        }
        let expected = stored.clone();
        let deployment_id = stored.instance.deployment_id.clone();
        if let Some(status) = credentials.get(deployment_id.as_str()) {
            stored.credential_expires_at_ms = status.expires_at_ms;
            stored.credential_last_success_at_ms = status.last_success_at_ms;
            stored.credential_last_error = status.last_error.clone();
        }
        match observations.get(deployment_id.as_str()) {
            Some(observation) => apply_runtime_observation(&mut stored, observation),
            None if stored.instance.desired_state == RuntimeDesiredState::Removed => {
                stored.instance.observed_state = RuntimeObservedState::Missing;
                stored.instance.health = "NONE".to_string();
                stored.instance.runtime_attested = true;
                stored.drift_reason.clear();
            }
            None => {
                stored.instance.observed_state = RuntimeObservedState::Missing;
                stored.instance.health = "UNHEALTHY".to_string();
                stored.instance.runtime_attested = false;
                stored.drift_reason =
                    "managed deployment is missing from the complete Agent inventory".to_string();
            }
        }
        stored.last_observed_at_ms = facts.observed_at_ms;
        stored.updated_at = format!("unix-ms:{}", facts.observed_at_ms);
        stored.validate().map_err(|error| AgentApiError {
            status: 422,
            code: "AGENT_RUNTIME_PROJECTION_INVALID",
            detail: error.to_string(),
        })?;
        projected.push((expected, stored));
    }
    Ok(RuntimeReportProjection {
        expected_managed_deployment_ids: Some(expected_managed_deployments),
        updates: projected,
    })
}

#[cfg(test)]
fn runtime_projection_impact(
    projections: &[(StoredRuntimeInstance, StoredRuntimeInstance)],
) -> (BTreeSet<String>, bool) {
    let mut affected = BTreeSet::new();
    let mut force_revoke = false;
    for (previous, current) in projections {
        let was_available = runtime_is_binding_available(previous);
        let is_available = runtime_is_binding_available(current);
        if was_available != is_available {
            affected.insert(current.instance.deployment_id.clone());
            force_revoke |= was_available && !is_available;
        }
    }
    (affected, force_revoke)
}

fn catch_up_latest_complete_runtime_report(
    storage: &DurableStore,
    node_id: &str,
) -> Result<(), AgentApiError> {
    for attempt in 0..RUNTIME_REPORT_CAS_RETRIES {
        let Some(stored_facts) =
            storage
                .node_runtime_facts(node_id)
                .map_err(|error| AgentApiError {
                    status: 500,
                    code: "AGENT_RUNTIME_PROJECTION_READ_FAILED",
                    detail: error.to_string(),
                })?
        else {
            return Ok(());
        };
        let facts: NodeRuntimeFactsV1 = serde_json::from_value(stored_facts.facts.clone())
            .map_err(|error| AgentApiError {
                status: 500,
                code: "AGENT_RUNTIME_PROJECTION_INVALID",
                detail: format!("decode accepted Node runtime report: {error}"),
            })?;
        let projection = runtime_report_projections(storage, node_id, &facts)?;
        // Even a partial report executes the exact no-projection replay below
        // as a Node-lock synchronization barrier. It never mutates runtime and
        // the public evidence view remains fail-closed, but if a concurrent
        // newer complete report committed first this replay conflicts and the
        // loop re-reads that complete report before returning.
        match storage.apply_node_runtime_report(
            &stored_facts,
            projection.expected_managed_deployment_ids.as_deref(),
            &projection.updates,
        ) {
            Ok(()) => return Ok(()),
            Err(DurableError::Conflict(_)) if attempt + 1 < RUNTIME_REPORT_CAS_RETRIES => {
                // Either the report or the exact set/contents of managed
                // RuntimeInstances changed. Re-read both and converge on the
                // new fixed point under the storage transaction's Node lock.
            }
            Err(error) => {
                return Err(AgentApiError {
                    status: if matches!(error, DurableError::Conflict(_)) {
                        409
                    } else {
                        500
                    },
                    code: "AGENT_RUNTIME_CATCHUP_FAILED",
                    detail: error.to_string(),
                });
            }
        }
    }
    unreachable!("bounded runtime catch-up CAS loop always returns")
}

fn runtime_job_deployments(job: &orchestrator_control_plane::Job) -> BTreeSet<String> {
    let mut deployments = BTreeSet::new();
    for pointer in [
        "/spec/deployment_id",
        "/install/spec/deployment_id",
        "/new_spec/deployment_id",
        "/old_deployment_id",
        "/deployment_id",
    ] {
        if let Some(deployment_id) = job.payload.pointer(pointer).and_then(Value::as_str)
            && !deployment_id.trim().is_empty()
        {
            deployments.insert(deployment_id.to_string());
        }
    }
    if let Some(result) = job.result.as_ref() {
        for pointer in ["/instance/deployment_id", "/replaced_deployment_id"] {
            if let Some(deployment_id) = result.pointer(pointer).and_then(Value::as_str)
                && !deployment_id.trim().is_empty()
            {
                deployments.insert(deployment_id.to_string());
            }
        }
    }
    deployments
}

fn runtime_deployments_include_unavailable(
    storage: &DurableStore,
    deployments: &BTreeSet<String>,
) -> Result<bool, AgentApiError> {
    let evidence_at_ms = now_ms();
    for deployment_id in deployments {
        let Some(runtime) =
            storage
                .runtime_instance(deployment_id)
                .map_err(|error| AgentApiError {
                    status: 500,
                    code: "AGENT_RUNTIME_PROJECTION_READ_FAILED",
                    detail: error.to_string(),
                })?
        else {
            return Ok(true);
        };
        let runtime = storage
            .runtime_with_current_evidence(runtime, evidence_at_ms)
            .map_err(|error| AgentApiError {
                status: 500,
                code: "AGENT_RUNTIME_PROJECTION_READ_FAILED",
                detail: error.to_string(),
            })?;
        if !runtime_is_binding_available(&runtime) {
            return Ok(true);
        }
    }
    Ok(false)
}

fn reconcile_runtime_report_bindings(
    storage: &DurableStore,
    topology_provider: Option<&TopologyProviderSaga>,
    node_id: &str,
) -> Result<(), AgentApiError> {
    let runtimes = storage
        .runtime_instances(Some(node_id))
        .map_err(|error| AgentApiError {
            status: 500,
            code: "AGENT_RUNTIME_PROJECTION_READ_FAILED",
            detail: error.to_string(),
        })?;
    let affected = runtimes
        .iter()
        .map(|runtime| runtime.instance.deployment_id.clone())
        .collect::<BTreeSet<_>>();
    if affected.is_empty() {
        return Ok(());
    }
    let force_revoke = runtime_deployments_include_unavailable(storage, &affected)?;
    reconcile_runtime_binding_projections(storage, topology_provider, Some(&affected), force_revoke)
        .map_err(runtime_binding_projection_error)
}

fn runtime_is_binding_available(runtime: &StoredRuntimeInstance) -> bool {
    runtime_preserves_active_binding_route(runtime)
}

fn runtime_binding_projection_error(detail: String) -> AgentApiError {
    AgentApiError {
        status: 503,
        code: "AGENT_RUNTIME_BINDING_PROJECTION_FAILED",
        detail,
    }
}

fn apply_runtime_observation(
    stored: &mut StoredRuntimeInstance,
    observation: &orchestrator_runtime::DeploymentRuntimeObservationV1,
) {
    // Health and process state are live availability evidence, not runtime
    // attestation. A workload that is temporarily unhealthy must remain able
    // to use its already-activated Bindings to recover. Only identity,
    // artifact, profile, policy and effective HostConfig mismatches revoke
    // structural attestation.
    let mut structural_reasons = Vec::new();
    if !observation.runtime_attested {
        structural_reasons.push(observation.drift_reason.clone());
    }
    if observation.container_id != stored.instance.container_id {
        structural_reasons.push("container ID differs from the managed projection".to_string());
    }
    if observation.service_id != stored.instance.service_id {
        structural_reasons
            .push("service identity label differs from the managed projection".to_string());
    }
    if observation.artifact_digest != stored.instance.artifact_digest {
        structural_reasons
            .push("container image digest differs from the signed Release".to_string());
    }
    if observation.runtime_contract != stored.instance.runtime_contract {
        structural_reasons.push("runtime profile or profile digest drifted".to_string());
    }
    if !stored.instance.runtime_policy_sha256.is_empty()
        && observation.runtime_policy_sha256 != stored.instance.runtime_policy_sha256
    {
        structural_reasons.push("Agent runtime policy digest drifted".to_string());
    }
    if !stored.instance.effective_runtime_sha256.is_empty()
        && observation.effective_runtime_sha256 != stored.instance.effective_runtime_sha256
    {
        structural_reasons.push("effective HostConfig or mount digest drifted".to_string());
    }

    stored.instance.observed_state = observation.observed_state.clone();
    stored.instance.health = observation.health.clone();
    if structural_reasons.is_empty() {
        stored.instance.runtime_policy_sha256 = observation.runtime_policy_sha256.clone();
        stored.instance.effective_runtime_sha256 = observation.effective_runtime_sha256.clone();
        stored.instance.runtime_attested = true;
        stored.drift_reason.clear();
    } else {
        stored.instance.runtime_attested = false;
        stored.drift_reason = bounded_agent_text(&structural_reasons.join("; "));
    }
}

fn bounded_agent_text(value: &str) -> String {
    if value.len() <= 512 {
        return value.to_string();
    }
    let mut end = 512;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    value[..end].to_string()
}

fn valid_sha256(value: &str) -> bool {
    let Some(value) = value.strip_prefix("sha256:") else {
        return false;
    };
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn exchange_workload_credential(
    storage: &DurableStore,
    jobs: Option<&Mutex<DurableJobStore>>,
    issuer: Option<&dyn WorkloadTokenIssuer>,
    caller: AgentCaller<'_>,
    node_id: &str,
    request: &ApiRequest,
) -> Result<ApiResponse, AgentApiError> {
    if matches!(caller, AgentCaller::AnonymousTls) {
        return Err(AgentApiError {
            status: 401,
            code: "AGENT_MTLS_REQUIRED",
            detail:
                "workload credentials require Node mTLS or the Desktop loopback bootstrap identity"
                    .to_string(),
        });
    }
    let body: WorkloadCredentialExchangeBody =
        serde_json::from_str(&request.body).map_err(invalid_json)?;
    if body.deployment_id.trim().is_empty() {
        return Err(invalid("deployment_id is required"));
    }
    let has_job = !body.job_id.trim().is_empty();
    let has_lease = !body.lease_token.trim().is_empty();
    if has_job != has_lease {
        return Err(invalid(
            "job_id and lease_token must either both be present or both be omitted",
        ));
    }

    let runtime = storage
        .runtime_instance(&body.deployment_id)
        .map_err(|error| AgentApiError {
            status: 500,
            code: "AGENT_WORKLOAD_ASSIGNMENT_FAILED",
            detail: error.to_string(),
        })?;
    if let Some(runtime) = runtime.as_ref()
        && (runtime.node_id != node_id || runtime.management_mode != RuntimeManagementMode::Managed)
    {
        return Err(AgentApiError {
            status: 403,
            code: "AGENT_WORKLOAD_ASSIGNMENT_REJECTED",
            detail: "deployment is not a managed workload assigned to this Node".to_string(),
        });
    }

    let mut assignment_operation_id = None;
    if has_job {
        let jobs = jobs.ok_or_else(|| AgentApiError {
            status: 503,
            code: "AGENT_JOB_STORAGE_UNAVAILABLE",
            detail: "initial workload credential exchange requires durable Job storage".to_string(),
        })?;
        let store = lock_store(jobs)?;
        let job = store
            .get(&body.job_id)
            .map_err(job_error)?
            .ok_or_else(|| AgentApiError {
                status: 404,
                code: "AGENT_JOB_NOT_FOUND",
                detail: format!("job {} was not found", body.job_id),
            })?;
        if job.node_id != node_id
            || !matches!(job.status, JobStatus::Leased | JobStatus::CancelRequested)
            || job.lease_token.as_deref() != Some(body.lease_token.as_str())
            || job
                .lease_expires_at_ms
                .is_none_or(|expiry| expiry <= now_ms())
            || job_payload_deployment_id(&job.payload) != Some(body.deployment_id.as_str())
        {
            return Err(AgentApiError {
                status: 409,
                code: "AGENT_WORKLOAD_LEASE_REJECTED",
                detail: "job lease is stale or does not assign this deployment to the Node"
                    .to_string(),
            });
        }
        assignment_operation_id = Some(job.operation_id.clone());
    } else {
        let runtime = runtime.as_ref().ok_or_else(|| AgentApiError {
            status: 409,
            code: "AGENT_WORKLOAD_ASSIGNMENT_REQUIRED",
            detail: "lease-free refresh requires an existing managed RuntimeInstance".to_string(),
        })?;
        if runtime.instance.observed_state != orchestrator_runtime::RuntimeObservedState::Running {
            return Err(AgentApiError {
                status: 409,
                code: "AGENT_WORKLOAD_NOT_ACTIVE",
                detail: "lease-free refresh requires a RUNNING RuntimeInstance".to_string(),
            });
        }
    }

    let bindings = storage
        .api_bindings_for_deployment(&body.deployment_id)
        .map_err(|error| AgentApiError {
            status: 500,
            code: "AGENT_WORKLOAD_BINDINGS_FAILED",
            detail: error.to_string(),
        })?;
    let eligible = bindings
        .iter()
        .filter(|binding| {
            let pending_for_apply =
                assignment_operation_id
                    .as_deref()
                    .is_some_and(|operation_id| {
                        binding.observed_state == "PENDING"
                            && binding.state == ApiBindingState::Pending
                            && binding.last_operation_id == operation_id
                            && storage
                                .topology_heads(&binding.topology_id)
                                .ok()
                                .flatten()
                                .is_some_and(|heads| {
                                    heads.applying_revision_id.as_deref()
                                        == Some(binding.topology_revision_id.as_str())
                                        && heads.applying_operation_id.as_deref()
                                            == Some(operation_id)
                                })
                    });
            binding.consumer_node_id == node_id
                && binding.desired_state == "ACTIVE"
                && binding.auth_mode == "workload"
                && if has_job {
                    pending_for_apply
                        || binding.observed_state == "RESOLVED"
                        || binding.observed_state == "ACTIVE"
                } else {
                    binding.observed_state == "ACTIVE"
                }
        })
        .collect::<Vec<_>>();
    if eligible.is_empty() {
        return Err(AgentApiError {
            status: 409,
            code: "AGENT_WORKLOAD_BINDING_NOT_ACTIVE",
            detail: if has_job {
                "initial exchange requires a desired ACTIVE, observed RESOLVED or ACTIVE workload binding"
                    .to_string()
            } else {
                "lease-free refresh requires an observed ACTIVE workload binding".to_string()
            },
        });
    }
    let service_id = eligible[0].consumer_service_id.as_str();
    if eligible
        .iter()
        .any(|binding| binding.consumer_service_id != service_id)
        || runtime
            .as_ref()
            .is_some_and(|runtime| runtime.instance.service_id != service_id)
    {
        return Err(AgentApiError {
            status: 500,
            code: "AGENT_WORKLOAD_BINDING_INCONSISTENT",
            detail: "persisted workload binding identity does not match its RuntimeInstance"
                .to_string(),
        });
    }
    let credential_generation = eligible
        .iter()
        .map(|binding| binding.credential_generation)
        .max()
        .expect("eligible bindings are non-empty");
    if eligible.iter().any(|binding| {
        binding.credential_generation != credential_generation
            || binding.context_generation != credential_generation
    }) {
        return Err(AgentApiError {
            status: 500,
            code: "AGENT_WORKLOAD_BINDING_GENERATION_SPLIT",
            detail: "all active bindings for one deployment must share one credential/context generation"
                .to_string(),
        });
    }
    let issuer = issuer.ok_or_else(|| AgentApiError {
        status: 503,
        code: "AGENT_WORKLOAD_ISSUER_UNAVAILABLE",
        detail: "Auth workload token issuer is not configured".to_string(),
    })?;
    let issued = issuer
        .issue(&WorkloadTokenRequest {
            deployment_id: body.deployment_id,
            service_id: service_id.to_string(),
            node_id: node_id.to_string(),
            credential_generation,
        })
        .map_err(|error| AgentApiError {
            status: 503,
            code: "AGENT_WORKLOAD_ISSUER_FAILED",
            detail: format!("Auth workload token issuance failed: {error}"),
        })?;
    if issued.expires_in != WORKLOAD_TOKEN_TTL_SECONDS || issued.expires_at_ms <= now_ms() {
        return Err(AgentApiError {
            status: 503,
            code: "AGENT_WORKLOAD_ISSUER_INVALID",
            detail: "Auth returned an expired or non-15-minute workload credential".to_string(),
        });
    }
    Ok(ApiResponse::ok(json!({
        "access_token": issued.access_token,
        "token_type": "Bearer",
        "expires_at_ms": issued.expires_at_ms,
        "expires_in": issued.expires_in,
    }))
    .with_header("Cache-Control", "no-store"))
}

fn job_payload_deployment_id(payload: &Value) -> Option<&str> {
    [
        "/spec/deployment_id",
        "/install/spec/deployment_id",
        "/new_spec/deployment_id",
        "/deployment_id",
    ]
    .iter()
    .find_map(|pointer| payload.pointer(pointer).and_then(Value::as_str))
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
    topology_provider: Option<&TopologyProviderSaga>,
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
            let assigned = store
                .get(job_id)
                .map_err(job_error)?
                .ok_or_else(|| job_error(JobError::NotFound(job_id.to_string())))?;
            if body.status == CompletionStatus::Succeeded
                && runtime_job_requires_observation_watermark(&assigned.kind)
            {
                let validation_at_ms = if assigned.status == JobStatus::Succeeded {
                    assigned.updated_at_ms
                } else {
                    completed_at_ms
                };
                validate_runtime_observed_at_ms(&body.result, validation_at_ms).map_err(
                    |detail| AgentApiError {
                        status: 422,
                        code: "AGENT_RUNTIME_COMPLETION_INVALID",
                        detail: format!("job {job_id} {detail}"),
                    },
                )?;
            }
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
            let affected = runtime_job_deployments(&completed);
            if !affected.is_empty() {
                let force_revoke =
                    matches!(
                        completed.kind,
                        orchestrator_control_plane::JobKind::Stop
                            | orchestrator_control_plane::JobKind::Uninstall
                    ) || runtime_deployments_include_unavailable(storage, &affected)?;
                reconcile_runtime_binding_projections(
                    storage,
                    topology_provider,
                    Some(&affected),
                    force_revoke,
                )
                .map_err(runtime_binding_projection_error)?;
            }
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

fn runtime_job_requires_observation_watermark(kind: &orchestrator_control_plane::JobKind) -> bool {
    matches!(
        kind,
        orchestrator_control_plane::JobKind::Install
            | orchestrator_control_plane::JobKind::ReleasePipeline
            | orchestrator_control_plane::JobKind::Upgrade
            | orchestrator_control_plane::JobKind::Start
            | orchestrator_control_plane::JobKind::Stop
            | orchestrator_control_plane::JobKind::Restart
            | orchestrator_control_plane::JobKind::Rollback
            | orchestrator_control_plane::JobKind::Uninstall
            | orchestrator_control_plane::JobKind::Health
    )
}

fn validate_runtime_observed_at_ms(
    result: &Value,
    control_plane_at_ms: i64,
) -> Result<i64, String> {
    let observed_at_ms = result
        .get("runtime_observed_at_ms")
        .and_then(Value::as_i64)
        .filter(|value| *value > 0)
        .ok_or_else(|| {
            "requires a positive integer runtime_observed_at_ms watermark".to_string()
        })?;
    if observed_at_ms.abs_diff(control_plane_at_ms) > RUNTIME_FACTS_MAX_CLOCK_SKEW_MS as u64 {
        return Err(
            "runtime observation differs from control-plane completion time by more than 5 minutes"
                .to_string(),
        );
    }
    Ok(observed_at_ms)
}

fn runtime_completion_observed_at_ms(
    completed: &orchestrator_control_plane::Job,
) -> Result<i64, AgentApiError> {
    validate_runtime_observed_at_ms(
        completed.result.as_ref().unwrap_or(&Value::Null),
        completed.updated_at_ms,
    )
    .map_err(|detail| AgentApiError {
        status: 500,
        code: "AGENT_RUNTIME_PROJECTION_FAILED",
        detail: format!("job {} {detail}", completed.job_id),
    })
}

fn project_runtime_instance(
    storage: &DurableStore,
    completed: &orchestrator_control_plane::Job,
) -> Result<(), AgentApiError> {
    if completed.status != JobStatus::Succeeded {
        return Ok(());
    }
    let runtime_observed_at_ms = if runtime_job_requires_observation_watermark(&completed.kind) {
        runtime_completion_observed_at_ms(completed)?
    } else {
        0
    };
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
                external_probe_protocol: String::new(),
                external_probe_health_path: String::new(),
                last_observed_at_ms: runtime_observed_at_ms,
                drift_reason: String::new(),
                credential_expires_at_ms: 0,
                credential_last_success_at_ms: 0,
                credential_last_error: String::new(),
                updated_at: format!("unix-ms:{}", completed.updated_at_ms),
            };
            storage
                .put_runtime_instance(&stored)
                .map_err(|error| AgentApiError {
                    status: 500,
                    code: "AGENT_RUNTIME_PROJECTION_FAILED",
                    detail: format!("persist runtime instance projection failed: {error}"),
                })?;
            catch_up_latest_complete_runtime_report(storage, &completed.node_id)?;
            if let Some(context) = completed
                .payload
                .pointer(match completed.kind {
                    orchestrator_control_plane::JobKind::Install => "/spec/managed_service_context",
                    orchestrator_control_plane::JobKind::ReleasePipeline => {
                        "/install/spec/managed_service_context"
                    }
                    _ => unreachable!("install projection match guards the Job kind"),
                })
                .filter(|value| !value.is_null())
            {
                let context: ManagedServiceContextSpec = serde_json::from_value(context.clone())
                    .map_err(|error| AgentApiError {
                        status: 500,
                        code: "AGENT_CONTEXT_PROJECTION_FAILED",
                        detail: format!("decode initial managed context: {error}"),
                    })?;
                persist_managed_context_projection(
                    storage,
                    &stored.instance.deployment_id,
                    Some(context),
                    None,
                )?;
            }
            activate_deployment_bindings(
                storage,
                &stored.instance.deployment_id,
                &completed.operation_id,
                &storage
                    .runtime_instance(&stored.instance.deployment_id)
                    .map_err(|error| AgentApiError {
                        status: 500,
                        code: "AGENT_RUNTIME_PROJECTION_FAILED",
                        detail: format!("reload caught-up runtime projection: {error}"),
                    })?
                    .ok_or_else(|| AgentApiError {
                        status: 500,
                        code: "AGENT_RUNTIME_PROJECTION_FAILED",
                        detail: "caught-up runtime projection disappeared".to_string(),
                    })?
                    .instance
                    .health,
            )?;
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
                external_probe_protocol: previous.external_probe_protocol,
                external_probe_health_path: previous.external_probe_health_path,
                last_observed_at_ms: runtime_observed_at_ms,
                drift_reason: previous.drift_reason,
                credential_expires_at_ms: previous.credential_expires_at_ms,
                credential_last_success_at_ms: previous.credential_last_success_at_ms,
                credential_last_error: previous.credential_last_error,
                updated_at: format!("unix-ms:{}", completed.updated_at_ms),
            };
            storage
                .put_runtime_instance(&stored)
                .map_err(|error| AgentApiError {
                    status: 500,
                    code: "AGENT_RUNTIME_PROJECTION_FAILED",
                    detail: format!("persist runtime instance projection failed: {error}"),
                })?;
            catch_up_latest_complete_runtime_report(storage, &completed.node_id)?;
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
                external_probe_protocol: String::new(),
                external_probe_health_path: String::new(),
                last_observed_at_ms: runtime_observed_at_ms,
                drift_reason: String::new(),
                credential_expires_at_ms: 0,
                credential_last_success_at_ms: 0,
                credential_last_error: String::new(),
                updated_at: format!("unix-ms:{}", completed.updated_at_ms),
            };
            if completed
                .payload
                .get("preserve_old_until_topology_cutover")
                .and_then(Value::as_bool)
                .unwrap_or(false)
            {
                storage
                    .put_runtime_instance(&stored)
                    .map_err(|error| AgentApiError {
                        status: 500,
                        code: "AGENT_RUNTIME_PROJECTION_FAILED",
                        detail: format!(
                            "persist topology-gated replacement runtime failed: {error}"
                        ),
                    })?;
            } else {
                storage
                    .replace_runtime_instance(replaced_deployment_id, &stored)
                    .map_err(|error| AgentApiError {
                        status: 500,
                        code: "AGENT_RUNTIME_PROJECTION_FAILED",
                        detail: format!("persist atomic runtime replacement failed: {error}"),
                    })?;
            }
            catch_up_latest_complete_runtime_report(storage, &completed.node_id)?;
            if let Some(context) = completed
                .payload
                .pointer("/new_spec/managed_service_context")
                .filter(|value| !value.is_null())
            {
                let context: ManagedServiceContextSpec = serde_json::from_value(context.clone())
                    .map_err(|error| AgentApiError {
                        status: 500,
                        code: "AGENT_CONTEXT_PROJECTION_FAILED",
                        detail: format!("decode replacement managed context: {error}"),
                    })?;
                persist_managed_context_projection(
                    storage,
                    &stored.instance.deployment_id,
                    Some(context),
                    None,
                )?;
            }
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
            revoke_deployment_bindings(storage, &deployment_id, &completed.operation_id)?;
            storage
                .delete_runtime_instance(&deployment_id)
                .map_err(|error| AgentApiError {
                    status: 500,
                    code: "AGENT_RUNTIME_PROJECTION_FAILED",
                    detail: format!("delete runtime instance projection failed: {error}"),
                })?;
            storage
                .delete_state(MANAGED_CONTEXT_STATE_NAMESPACE, &deployment_id)
                .map_err(|error| AgentApiError {
                    status: 500,
                    code: "AGENT_CONTEXT_PROJECTION_FAILED",
                    detail: format!("delete managed context projection: {error}"),
                })?;
        }
        orchestrator_control_plane::JobKind::BindingContextApply => {
            let payload: BindingContextApplyPayload =
                serde_json::from_value(completed.payload.clone()).map_err(|error| {
                    AgentApiError {
                        status: 500,
                        code: "AGENT_CONTEXT_PROJECTION_FAILED",
                        detail: format!("decode binding context completion payload: {error}"),
                    }
                })?;
            persist_managed_context_projection(
                storage,
                &payload.deployment_id,
                payload.context,
                payload.previous_context,
            )?;
        }
        orchestrator_control_plane::JobKind::Inventory
        | orchestrator_control_plane::JobKind::ExternalHealth
        | orchestrator_control_plane::JobKind::TopologyApply
        | orchestrator_control_plane::JobKind::NodeDrain
        | orchestrator_control_plane::JobKind::NodeRemove => {}
    }
    Ok(())
}

fn persist_managed_context_projection(
    storage: &DurableStore,
    deployment_id: &str,
    current: Option<ManagedServiceContextSpec>,
    previous: Option<ManagedServiceContextSpec>,
) -> Result<(), AgentApiError> {
    let last_nonempty = current.clone().or(previous).ok_or_else(|| AgentApiError {
        status: 500,
        code: "AGENT_CONTEXT_PROJECTION_FAILED",
        detail: "managed context projection has neither current nor previous context".to_string(),
    })?;
    let projection = ManagedServiceContextProjection {
        revoked: current.is_none(),
        current,
        last_nonempty,
    };
    storage
        .put_state(MANAGED_CONTEXT_STATE_NAMESPACE, deployment_id, &projection)
        .map_err(|error| AgentApiError {
            status: 500,
            code: "AGENT_CONTEXT_PROJECTION_FAILED",
            detail: format!("persist managed context projection: {error}"),
        })
}

fn activate_deployment_bindings(
    storage: &DurableStore,
    deployment_id: &str,
    operation_id: &str,
    runtime_health: &str,
) -> Result<(), AgentApiError> {
    let mut bindings = storage
        .api_bindings_for_deployment(deployment_id)
        .map_err(binding_projection_error)?;
    if bindings.is_empty() {
        return Ok(());
    }
    let now = format!("unix-ms:{}", now_ms());
    for binding in &mut bindings {
        if binding.desired_state == "ACTIVE"
            && matches!(
                binding.state,
                ApiBindingState::Resolved | ApiBindingState::Active
            )
        {
            binding.state = ApiBindingState::Active;
            binding.observed_state = "ACTIVE".to_string();
            binding.health = if runtime_health.eq_ignore_ascii_case("HEALTHY") {
                "HEALTHY".to_string()
            } else {
                "DEGRADED".to_string()
            };
            binding.drift.clear();
            binding.last_operation_id = operation_id.to_string();
            binding.updated_at = now.clone();
        }
    }
    storage
        .replace_deployment_api_bindings(deployment_id, &bindings)
        .map_err(binding_projection_error)
}

fn revoke_deployment_bindings(
    storage: &DurableStore,
    deployment_id: &str,
    operation_id: &str,
) -> Result<(), AgentApiError> {
    let mut bindings = storage
        .api_bindings_for_deployment(deployment_id)
        .map_err(binding_projection_error)?;
    if bindings.is_empty() {
        return Ok(());
    }
    let now = format!("unix-ms:{}", now_ms());
    for binding in &mut bindings {
        binding.state = ApiBindingState::Revoked;
        binding.desired_state = "REVOKED".to_string();
        binding.observed_state = "REVOKED".to_string();
        binding.health = "UNHEALTHY".to_string();
        binding.credential_generation = binding.credential_generation.saturating_add(1);
        binding.context_generation = binding.context_generation.saturating_add(1);
        binding.drift.clear();
        binding.last_operation_id = operation_id.to_string();
        binding.reason = "consumer deployment was uninstalled".to_string();
        binding.updated_at = now.clone();
    }
    storage
        .replace_deployment_api_bindings(deployment_id, &bindings)
        .map_err(binding_projection_error)
}

fn binding_projection_error(error: DurableError) -> AgentApiError {
    AgentApiError {
        status: 500,
        code: "AGENT_BINDING_PROJECTION_FAILED",
        detail: error.to_string(),
    }
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

    fn runtime_report(
        report_id: &str,
        observed_at_ms: i64,
        inventory_complete: bool,
        deployment_observations: Value,
        credential_statuses: Value,
    ) -> Value {
        json!({
            "schema_version": 1,
            "report_id": report_id,
            "observed_at_ms": observed_at_ms,
            "agent_version": "1.0.0-test",
            "runtime_policy_sha256": format!("sha256:{}", "c".repeat(64)),
            "allowed_contracts": [{
                "id": "standard-container-v1",
                "profile_sha256": orchestrator_runtime::STANDARD_RUNTIME_PROFILE_SHA256,
            }],
            "judge_sandbox_allowed_images": [],
            "docker": {
                "engine": "docker",
                "server_version": "28.0.0",
                "operating_system": "Linux",
                "os_type": "linux",
                "architecture": "x86_64",
                "cgroup_version": "2",
                "memory_limit": true,
                "pids_limit": true,
                "rootless": false,
                "apparmor": true,
                "seccomp": true,
                "security_options": ["name=apparmor", "name=seccomp,profile=builtin"]
            },
            "inventory_complete": inventory_complete,
            "inventory_error": if inventory_complete { "" } else { "bounded Docker inventory unavailable" },
            "deployment_observations": deployment_observations,
            "credential_statuses": credential_statuses,
        })
    }

    fn healthy_observation(container_id: &str) -> Value {
        json!({
            "deployment_id": "deployment-report-1",
            "service_id": "service-report-1",
            "container_id": container_id,
            "artifact_digest": format!("registry.example/ojos/service@sha256:{}", "a".repeat(64)),
            "runtime_contract": {
                "id": "standard-container-v1",
                "profile_sha256": orchestrator_runtime::STANDARD_RUNTIME_PROFILE_SHA256,
            },
            "runtime_policy_sha256": format!("sha256:{}", "c".repeat(64)),
            "effective_runtime_sha256": format!("sha256:{}", "d".repeat(64)),
            "observed_state": "RUNNING",
            "health": "HEALTHY",
            "runtime_attested": true,
            "drift_reason": "",
        })
    }

    fn runtime_report_fixture() -> (tempfile::TempDir, DurableStore) {
        let directory = tempdir().unwrap();
        let sqlite =
            SqliteOrchestratorStore::open(directory.path().join("runtime-report.db")).unwrap();
        let durable = DurableStore::Sqlite(sqlite);
        let stored: StoredRuntimeInstance = serde_json::from_value(json!({
            "node_id": "node-report-1",
            "instance": {
                "deployment_id": "deployment-report-1",
                "service_id": "service-report-1",
                "release_version": "1.0.0",
                "container_id": "container-report-1",
                "artifact_digest": format!("registry.example/ojos/service@sha256:{}", "a".repeat(64)),
                "runtime_contract": {
                    "id": "standard-container-v1",
                    "profile_sha256": orchestrator_runtime::STANDARD_RUNTIME_PROFILE_SHA256,
                },
                "runtime_policy_sha256": format!("sha256:{}", "c".repeat(64)),
                "effective_runtime_sha256": format!("sha256:{}", "d".repeat(64)),
                "runtime_attested": false,
                "desired_state": "RUNNING",
                "observed_state": "UNKNOWN",
                "health": "UNHEALTHY"
            },
            "management_mode": "MANAGED",
            "endpoint": "",
            "updated_at": "unix-ms:1"
        }))
        .unwrap();
        durable.put_runtime_instance(&stored).unwrap();
        (directory, durable)
    }

    #[test]
    fn unhealthy_runtime_report_keeps_attestation_while_structural_drift_revokes() {
        let (_directory, storage) = runtime_report_fixture();
        let mut healthy = storage
            .runtime_instance("deployment-report-1")
            .unwrap()
            .unwrap();
        healthy.instance.observed_state = RuntimeObservedState::Running;
        healthy.instance.health = "HEALTHY".to_string();
        healthy.instance.runtime_attested = true;
        healthy.drift_reason.clear();

        let observed_at_ms = now_ms();
        let mut unhealthy_observation = healthy_observation("container-report-1");
        unhealthy_observation["health"] = json!("UNHEALTHY");
        assert_eq!(
            publish_runtime_report(
                &storage,
                runtime_report(
                    "report-unhealthy-continuity",
                    observed_at_ms,
                    true,
                    json!([unhealthy_observation]),
                    json!([]),
                ),
            )
            .status,
            204
        );
        let unhealthy = storage
            .runtime_instance("deployment-report-1")
            .unwrap()
            .unwrap();
        assert_eq!(
            unhealthy.instance.observed_state,
            RuntimeObservedState::Running
        );
        assert_eq!(unhealthy.instance.health, "UNHEALTHY");
        assert!(unhealthy.instance.runtime_attested);
        assert!(unhealthy.drift_reason.is_empty());

        let (affected, force_revoke) =
            runtime_projection_impact(&[(healthy.clone(), unhealthy.clone())]);
        assert!(affected.is_empty());
        assert!(!force_revoke);

        let mut drift_observation = healthy_observation("container-report-1");
        drift_observation["effective_runtime_sha256"] = json!(format!("sha256:{}", "e".repeat(64)));
        drift_observation["artifact_digest"] = json!(format!(
            "registry.example/ojos/service@sha256:{}",
            "f".repeat(64)
        ));
        assert_eq!(
            publish_runtime_report(
                &storage,
                runtime_report(
                    "report-structural-drift",
                    observed_at_ms + 1,
                    true,
                    json!([drift_observation]),
                    json!([]),
                ),
            )
            .status,
            204
        );
        let drifted = storage
            .runtime_instance("deployment-report-1")
            .unwrap()
            .unwrap();
        assert!(!drifted.instance.runtime_attested);
        assert!(drifted.drift_reason.contains("image digest"));
        assert!(drifted.drift_reason.contains("HostConfig"));

        let (affected, force_revoke) =
            runtime_projection_impact(&[(healthy.clone(), drifted.clone())]);
        assert_eq!(
            affected,
            BTreeSet::from(["deployment-report-1".to_string()])
        );
        assert!(force_revoke);

        let (affected, force_revoke) = runtime_projection_impact(&[(drifted, healthy)]);
        assert_eq!(
            affected,
            BTreeSet::from(["deployment-report-1".to_string()])
        );
        assert!(!force_revoke, "recovery must restore Auth before Gateway");
    }

    #[test]
    fn lifecycle_projection_tracks_stop_uninstall_and_replacement_assignments() {
        let job = Job {
            job_id: "job-runtime-impact".to_string(),
            operation_id: "op-runtime-impact".to_string(),
            node_id: "node-b".to_string(),
            kind: JobKind::Uninstall,
            payload: json!({
                "deployment_id": "deployment-uninstalled",
                "old_deployment_id": "deployment-old",
                "new_spec": {"deployment_id": "deployment-new"}
            }),
            payload_sha256: "hash".to_string(),
            idempotency_key: "runtime-impact".to_string(),
            status: JobStatus::Succeeded,
            attempt: 1,
            max_attempts: 3,
            available_at_ms: 0,
            lease_owner: None,
            lease_token: Some("lease".to_string()),
            lease_expires_at_ms: None,
            result: Some(json!({
                "instance": {"deployment_id": "deployment-result"},
                "replaced_deployment_id": "deployment-replaced"
            })),
            error_message: None,
            completion_fingerprint: Some("fingerprint".to_string()),
            created_at_ms: 1,
            started_at_ms: Some(2),
            completed_at_ms: Some(3),
            updated_at_ms: 3,
        };
        assert_eq!(
            runtime_job_deployments(&job),
            BTreeSet::from([
                "deployment-new".to_string(),
                "deployment-old".to_string(),
                "deployment-replaced".to_string(),
                "deployment-result".to_string(),
                "deployment-uninstalled".to_string(),
            ])
        );
    }

    fn publish_runtime_report(storage: &DurableStore, report: Value) -> ApiResponse {
        route(
            Some(storage),
            None,
            ApiRequest {
                method: "PUT".to_string(),
                path: "/api/v1/agent/nodes/node-report-1/runtime-facts".to_string(),
                headers: Default::default(),
                body: report.to_string(),
            },
        )
    }

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
    fn runtime_completion_watermark_is_required_bounded_and_covers_every_lifecycle_kind() {
        let at_ms = 1_000_000;
        for invalid_result in [
            json!({}),
            json!({"runtime_observed_at_ms": "1000000"}),
            json!({"runtime_observed_at_ms": 0}),
            json!({
                "runtime_observed_at_ms": at_ms + RUNTIME_FACTS_MAX_CLOCK_SKEW_MS + 1
            }),
        ] {
            assert!(
                validate_runtime_observed_at_ms(&invalid_result, at_ms).is_err(),
                "invalid lifecycle evidence was accepted: {invalid_result}"
            );
        }
        assert_eq!(
            validate_runtime_observed_at_ms(&json!({"runtime_observed_at_ms": at_ms}), at_ms,)
                .unwrap(),
            at_ms
        );

        for kind in [
            JobKind::Install,
            JobKind::ReleasePipeline,
            JobKind::Upgrade,
            JobKind::Start,
            JobKind::Stop,
            JobKind::Restart,
            JobKind::Rollback,
            JobKind::Uninstall,
            JobKind::Health,
        ] {
            assert!(
                runtime_job_requires_observation_watermark(&kind),
                "{kind:?} omitted the causal watermark"
            );
        }
        assert!(!runtime_job_requires_observation_watermark(
            &JobKind::BindingContextApply
        ));
    }

    #[test]
    fn complete_runtime_report_projects_attestation_health_and_credential_expiry() {
        let (_directory, storage) = runtime_report_fixture();
        let observed_at_ms = now_ms();
        let report = runtime_report(
            "report-complete-1",
            observed_at_ms,
            true,
            json!([healthy_observation("container-report-1")]),
            json!([{
                "deployment_id": "deployment-report-1",
                "expires_at_ms": observed_at_ms + 900_000,
                "last_success_at_ms": observed_at_ms,
                "last_error": ""
            }]),
        );
        let response = publish_runtime_report(&storage, report.clone());
        assert_eq!(response.status, 204, "{:?}", response.body);
        let stored = storage
            .runtime_instance("deployment-report-1")
            .unwrap()
            .unwrap();
        assert!(stored.instance.runtime_attested);
        assert_eq!(stored.instance.health, "HEALTHY");
        assert!(stored.drift_reason.is_empty());
        assert_eq!(stored.last_observed_at_ms, observed_at_ms);
        assert_eq!(stored.credential_expires_at_ms, observed_at_ms + 900_000);
        assert_eq!(stored.credential_last_success_at_ms, observed_at_ms);
        assert!(stored.credential_last_error.is_empty());
        assert_eq!(publish_runtime_report(&storage, report).status, 204);
    }

    #[test]
    fn only_latest_complete_inventory_can_mark_missing_or_clear_drift() {
        let (_directory, storage) = runtime_report_fixture();
        let base = now_ms().saturating_sub(10_000);
        assert_eq!(
            publish_runtime_report(
                &storage,
                runtime_report("complete-missing", base, true, json!([]), json!([])),
            )
            .status,
            204
        );
        let missing = storage
            .runtime_instance("deployment-report-1")
            .unwrap()
            .unwrap();
        assert_eq!(
            missing.instance.observed_state,
            RuntimeObservedState::Missing
        );
        assert!(!missing.instance.runtime_attested);
        assert!(missing.drift_reason.contains("missing"));

        let partial = runtime_report(
            "partial-newer",
            base + 1,
            false,
            json!([healthy_observation("container-report-1")]),
            json!([]),
        );
        assert_eq!(publish_runtime_report(&storage, partial).status, 204);
        let unchanged = storage
            .runtime_instance("deployment-report-1")
            .unwrap()
            .unwrap();
        assert_eq!(
            unchanged.instance.observed_state,
            RuntimeObservedState::Missing
        );
        assert!(!unchanged.instance.runtime_attested);

        let stale = runtime_report(
            "stale-complete",
            base,
            true,
            json!([healthy_observation("container-report-1")]),
            json!([]),
        );
        assert_eq!(publish_runtime_report(&storage, stale).status, 409);

        let complete = runtime_report(
            "complete-newest",
            base + 2,
            true,
            json!([healthy_observation("container-report-1")]),
            json!([]),
        );
        assert_eq!(publish_runtime_report(&storage, complete).status, 204);
        let recovered = storage
            .runtime_instance("deployment-report-1")
            .unwrap()
            .unwrap();
        assert!(recovered.instance.runtime_attested);
        assert!(recovered.drift_reason.is_empty());
    }

    #[test]
    fn pre_lifecycle_inventory_arriving_late_cannot_overwrite_completion_evidence() {
        let (_directory, storage) = runtime_report_fixture();
        let snapshot_started_at_ms = now_ms().saturating_sub(10_000);
        let lifecycle_observed_at_ms = snapshot_started_at_ms + 1_000;

        // The Agent captured an empty inventory, then completed a real
        // install before that report reached the control plane.
        let mut completed = storage
            .runtime_instance("deployment-report-1")
            .unwrap()
            .unwrap();
        completed.instance.observed_state = RuntimeObservedState::Running;
        completed.instance.health = "HEALTHY".to_string();
        completed.instance.runtime_attested = true;
        completed.drift_reason.clear();
        completed.last_observed_at_ms = lifecycle_observed_at_ms;
        completed.updated_at = format!("unix-ms:{lifecycle_observed_at_ms}");
        storage.put_runtime_instance(&completed).unwrap();

        assert_eq!(
            publish_runtime_report(
                &storage,
                runtime_report(
                    "captured-before-install",
                    snapshot_started_at_ms,
                    true,
                    json!([]),
                    json!([]),
                ),
            )
            .status,
            204
        );
        let persisted = storage
            .runtime_instance("deployment-report-1")
            .unwrap()
            .unwrap();
        assert_eq!(persisted.last_observed_at_ms, lifecycle_observed_at_ms);
        assert_eq!(
            persisted.instance.observed_state,
            RuntimeObservedState::Running
        );
        assert_eq!(persisted.instance.health, "HEALTHY");
        assert!(persisted.instance.runtime_attested);

        // Reads can immediately use the authenticated Job result while the
        // latest fresh report is causally older; no harness polling shortcut
        // or fabricated health is involved.
        let public = storage
            .runtime_with_current_evidence(persisted, now_ms())
            .unwrap();
        assert_eq!(
            public.instance.observed_state,
            RuntimeObservedState::Running
        );
        assert_eq!(public.instance.health, "HEALTHY");
        assert!(public.instance.runtime_attested);

        // A genuinely newer complete inventory still fails closed and proves
        // the managed container missing.
        assert_eq!(
            publish_runtime_report(
                &storage,
                runtime_report(
                    "captured-after-install",
                    lifecycle_observed_at_ms + 1,
                    true,
                    json!([]),
                    json!([]),
                ),
            )
            .status,
            204
        );
        let missing = storage
            .runtime_instance("deployment-report-1")
            .unwrap()
            .unwrap();
        assert_eq!(
            missing.instance.observed_state,
            RuntimeObservedState::Missing
        );
        assert_eq!(missing.instance.health, "UNHEALTHY");
        assert!(!missing.instance.runtime_attested);
        assert!(missing.drift_reason.contains("missing"));
    }

    #[test]
    fn newer_report_committed_before_install_projection_is_caught_up_immediately() {
        let (_directory, storage) = runtime_report_fixture();
        let template = storage
            .runtime_instance("deployment-report-1")
            .unwrap()
            .unwrap();
        storage
            .delete_runtime_instance("deployment-report-1")
            .unwrap();
        let completion_observed_at_ms = now_ms().saturating_sub(10_000);
        let report_observed_at_ms = completion_observed_at_ms + 1_000;

        // The Docker inventory can see the newly healthy container while the
        // completion request is still in flight and before its durable
        // RuntimeInstance row exists.
        let report = runtime_report(
            "newer-report-before-completion",
            report_observed_at_ms,
            true,
            json!([healthy_observation("container-report-1")]),
            json!([]),
        );
        assert_eq!(publish_runtime_report(&storage, report).status, 204);
        assert!(
            storage
                .runtime_instance("deployment-report-1")
                .unwrap()
                .is_none()
        );

        let completed = Job {
            job_id: "job-report-before-install".to_string(),
            operation_id: "operation-report-before-install".to_string(),
            node_id: "node-report-1".to_string(),
            kind: JobKind::Install,
            payload: json!({
                "spec": {
                    "labels": {"ojos.release_version": "1.0.0"},
                    "published_endpoint": null
                }
            }),
            payload_sha256: "hash".to_string(),
            idempotency_key: "report-before-install".to_string(),
            status: JobStatus::Succeeded,
            attempt: 1,
            max_attempts: 3,
            available_at_ms: 0,
            lease_owner: None,
            lease_token: Some("lease".to_string()),
            lease_expires_at_ms: None,
            result: Some(json!({
                "runtime_observed_at_ms": completion_observed_at_ms,
                "instance": template.instance,
            })),
            error_message: None,
            completion_fingerprint: Some("fingerprint".to_string()),
            created_at_ms: completion_observed_at_ms - 2,
            started_at_ms: Some(completion_observed_at_ms - 1),
            completed_at_ms: Some(completion_observed_at_ms),
            updated_at_ms: completion_observed_at_ms,
        };
        project_runtime_instance(&storage, &completed).unwrap();

        let caught_up = storage
            .runtime_instance("deployment-report-1")
            .unwrap()
            .unwrap();
        assert_eq!(caught_up.last_observed_at_ms, report_observed_at_ms);
        assert_eq!(
            caught_up.instance.observed_state,
            RuntimeObservedState::Running
        );
        assert_eq!(caught_up.instance.health, "HEALTHY");
        assert!(caught_up.instance.runtime_attested);
        assert!(caught_up.drift_reason.is_empty());
        let public = storage
            .runtime_with_current_evidence(caught_up, now_ms())
            .unwrap();
        assert_eq!(
            public.instance.observed_state,
            RuntimeObservedState::Running
        );
        assert_eq!(public.instance.health, "HEALTHY");
    }

    #[test]
    fn lifecycle_completion_cannot_regress_a_newer_report_projection() {
        let (_directory, storage) = runtime_report_fixture();
        let completion_observed_at_ms = now_ms().saturating_sub(10_000);
        let report_observed_at_ms = completion_observed_at_ms + 1_000;
        assert_eq!(
            publish_runtime_report(
                &storage,
                runtime_report(
                    "newer-report-before-lifecycle-completion",
                    report_observed_at_ms,
                    true,
                    json!([healthy_observation("container-report-1")]),
                    json!([]),
                ),
            )
            .status,
            204
        );
        let newer = storage
            .runtime_instance("deployment-report-1")
            .unwrap()
            .unwrap();
        assert_eq!(newer.last_observed_at_ms, report_observed_at_ms);

        let completed = Job {
            job_id: "job-start-after-newer-report".to_string(),
            operation_id: "operation-start-after-newer-report".to_string(),
            node_id: "node-report-1".to_string(),
            kind: JobKind::Start,
            payload: json!({"container_id": "container-report-1"}),
            payload_sha256: "hash".to_string(),
            idempotency_key: "start-after-newer-report".to_string(),
            status: JobStatus::Succeeded,
            attempt: 1,
            max_attempts: 3,
            available_at_ms: 0,
            lease_owner: None,
            lease_token: Some("lease".to_string()),
            lease_expires_at_ms: None,
            result: Some(json!({
                "runtime_observed_at_ms": completion_observed_at_ms,
                "instance": newer.instance,
            })),
            error_message: None,
            completion_fingerprint: Some("fingerprint".to_string()),
            created_at_ms: completion_observed_at_ms - 2,
            started_at_ms: Some(completion_observed_at_ms - 1),
            completed_at_ms: Some(completion_observed_at_ms),
            updated_at_ms: completion_observed_at_ms,
        };
        project_runtime_instance(&storage, &completed).unwrap();

        let final_runtime = storage
            .runtime_instance("deployment-report-1")
            .unwrap()
            .unwrap();
        assert_eq!(final_runtime.last_observed_at_ms, report_observed_at_ms);
        assert_eq!(
            final_runtime.instance.observed_state,
            RuntimeObservedState::Running
        );
        assert_eq!(final_runtime.instance.health, "HEALTHY");
        assert!(final_runtime.instance.runtime_attested);
    }

    #[test]
    fn reused_report_id_and_container_identity_drift_fail_closed() {
        let (_directory, storage) = runtime_report_fixture();
        let base = now_ms().saturating_sub(1_000);
        let first = runtime_report(
            "same-id",
            base,
            true,
            json!([healthy_observation("replacement-container")]),
            json!([]),
        );
        assert_eq!(publish_runtime_report(&storage, first.clone()).status, 204);
        let drifted = storage
            .runtime_instance("deployment-report-1")
            .unwrap()
            .unwrap();
        assert!(!drifted.instance.runtime_attested);
        assert_eq!(
            drifted.instance.observed_state,
            RuntimeObservedState::Running
        );
        assert_eq!(drifted.instance.health, "HEALTHY");
        assert!(drifted.drift_reason.contains("container ID"));

        let mut changed = first;
        changed["inventory_error"] = json!("tampered duplicate");
        changed["inventory_complete"] = json!(false);
        assert_eq!(publish_runtime_report(&storage, changed).status, 409);
    }

    #[test]
    fn thirty_two_concurrent_reports_cannot_roll_back_the_newest_inventory() {
        let (_directory, storage) = runtime_report_fixture();
        let base = now_ms().saturating_sub(10_000);
        std::thread::scope(|scope| {
            let handles = (0..32_i64)
                .map(|index| {
                    let storage = &storage;
                    scope.spawn(move || {
                        let observations = if index % 2 == 0 {
                            json!([healthy_observation("container-report-1")])
                        } else {
                            json!([])
                        };
                        publish_runtime_report(
                            storage,
                            runtime_report(
                                &format!("concurrent-report-{index}"),
                                base + index,
                                true,
                                observations,
                                json!([]),
                            ),
                        )
                    })
                })
                .collect::<Vec<_>>();
            for handle in handles {
                let response = handle.join().unwrap();
                assert!(matches!(response.status, 204 | 409), "{:?}", response.body);
            }
        });
        let facts = storage
            .node_runtime_facts("node-report-1")
            .unwrap()
            .unwrap();
        assert_eq!(facts.observed_at_ms, base + 31);
        assert_eq!(facts.facts["report_id"], "concurrent-report-31");
        let deployment = storage
            .runtime_instance("deployment-report-1")
            .unwrap()
            .unwrap();
        assert_eq!(deployment.last_observed_at_ms, base + 31);
        assert_eq!(
            deployment.instance.observed_state,
            RuntimeObservedState::Missing
        );
        assert!(!deployment.instance.runtime_attested);
    }

    #[test]
    fn concurrent_runtime_report_never_rolls_back_a_lifecycle_stop() {
        for index in 0..32 {
            let (_directory, storage) = runtime_report_fixture();
            let barrier = std::sync::Barrier::new(2);
            std::thread::scope(|scope| {
                let report_storage = &storage;
                let report_barrier = &barrier;
                let report = scope.spawn(move || {
                    report_barrier.wait();
                    publish_runtime_report(
                        report_storage,
                        runtime_report(
                            &format!("lifecycle-race-{index}"),
                            now_ms(),
                            true,
                            json!([healthy_observation("container-report-1")]),
                            json!([]),
                        ),
                    )
                });
                let lifecycle_storage = &storage;
                let lifecycle_barrier = &barrier;
                let lifecycle = scope.spawn(move || {
                    lifecycle_barrier.wait();
                    let mut runtime = lifecycle_storage
                        .runtime_instance("deployment-report-1")
                        .unwrap()
                        .unwrap();
                    runtime.instance.desired_state = RuntimeDesiredState::Stopped;
                    runtime.instance.observed_state = RuntimeObservedState::Stopped;
                    runtime.instance.health = "NONE".to_string();
                    runtime.updated_at = format!("lifecycle-stop-{index}");
                    lifecycle_storage.put_runtime_instance(&runtime).unwrap();
                });
                assert_eq!(report.join().unwrap().status, 204);
                lifecycle.join().unwrap();
            });
            let runtime = storage
                .runtime_instance("deployment-report-1")
                .unwrap()
                .unwrap();
            assert_eq!(runtime.instance.desired_state, RuntimeDesiredState::Stopped);
        }
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
            AgentRouteContext {
                storage: None,
                jobs: None,
                artifact_store: None,
                identity_service: None,
                workload_token_issuer: None,
                topology_provider: None,
            },
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
                AgentRouteContext {
                    storage: Some(&storage),
                    jobs: None,
                    artifact_store: None,
                    identity_service: None,
                    workload_token_issuer: None,
                    topology_provider: None,
                },
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
            AgentRouteContext {
                storage: Some(&storage),
                jobs: None,
                artifact_store: None,
                identity_service: None,
                workload_token_issuer: None,
                topology_provider: None,
            },
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
                "runtime_observed_at_ms": 2,
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
        assert_eq!(replacement.last_observed_at_ms, 2);
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
            AgentRouteContext {
                storage: Some(&durable),
                jobs: Some(&jobs),
                artifact_store: Some(&artifact_store),
                identity_service: None,
                workload_token_issuer: None,
                topology_provider: None,
            },
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
            AgentRouteContext {
                storage: Some(&durable),
                jobs: Some(&jobs),
                artifact_store: Some(&artifact_store),
                identity_service: None,
                workload_token_issuer: None,
                topology_provider: None,
            },
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
            AgentRouteContext {
                storage: Some(&durable),
                jobs: Some(&jobs),
                artifact_store: Some(&artifact_store),
                identity_service: None,
                workload_token_issuer: None,
                topology_provider: None,
            },
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
                    "result": {
                        "runtime_observed_at_ms": now_ms(),
                        "instance": runtime_instance("deployment-1", "service-1", "1.0.0", "container-1")
                    },
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
                    "result": {
                        "runtime_observed_at_ms": now_ms(),
                        "instance": runtime_instance("deployment-e2e", "service-e2e", "1.0.0", "container-e2e")
                    },
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
