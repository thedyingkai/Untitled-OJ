use crate::contribution_controller;
use crate::durable::DurableStore;
use crate::topology_provider::{
    RuntimeProjectionOrder, TopologyProviderApplyState, TopologyProviderObservation,
    TopologyProviderObservedState, TopologyProviderSaga, TopologyProvidersObservation,
    provider_projection_sha256,
};
use getrandom::fill as random_fill;
use orchestrator_control_plane::{
    ClaimRequest, CompleteRequest, CompletionStatus, DEFAULT_LEASE_MS, DurableOperationStatus,
    HeartbeatRequest, Job, JobError, JobKind, JobStore, OperationCoordinator, OperationRepository,
    ResolveExpiredSuccessRequest,
};
use orchestrator_legacy::{
    ApiBinding, ApiBindingHealth, ApiBindingObservedState, ApiBindingState, Endpoint,
    EndpointProbe, TcpEndpointProbe, TopologyDeploymentStatus, TopologyDesiredDeploymentState,
    TopologyDrift, TopologyDriftKind, TopologyEndpointStatus, TopologyHealth, TopologyLinkStatus,
    TopologyObservedDeploymentState, TopologyReconciliationState, TopologyResourceKind,
    TopologySpec, TopologyStatus, parse_endpoint_id, validate_endpoint_id,
};
use orchestrator_runtime::{RuntimeDesiredState, RuntimeInstance, RuntimeObservedState};
use orchestrator_storage::{RuntimeManagementMode, StoredRuntimeInstance, TopologyApplyOutcome};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::io::Read;
use std::net::{TcpStream, ToSocketAddrs};
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread;
use std::thread::JoinHandle;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const CONTROL_PLANE_NODE_ID: &str = "control-plane";
const CONTROL_PLANE_HEARTBEAT_INTERVAL: Duration = Duration::from_secs(10);
const CONTROL_PLANE_MAX_STALL_MS: i64 = 25_000;
const RECONCILE_INTERVAL: Duration = Duration::from_secs(5);
const NETWORK_PROBE_TIMEOUT: Duration = Duration::from_millis(750);
const NETWORK_PROBE_CONCURRENCY: usize = 16;
const ENDPOINT_PROBE_BATCH: usize = 512;
const LINK_PROBE_BATCH: usize = 1_024;
const NETWORK_OBSERVATION_MAX_AGE_MS: i64 = 120_000;
const NETWORK_RESPONSE_LIMIT: usize = 4_096;
const EXTERNAL_REPROBE_INTERVAL_MS: i64 = 30_000;
const ENDPOINT_EVIDENCE_PREFIX: &str = "network probe:";
const LINK_EVIDENCE_PREFIX: &str = "source probe:";
const RUNTIME_BINDING_PROJECTION_STATE_NAMESPACE: &str = "topology-runtime-binding-projection-v1";

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct TopologyApplyPayload {
    #[serde(default)]
    topology_id: String,
    #[serde(default)]
    revision_id: String,
    #[serde(default)]
    phase: TopologyApplyPhase,
    #[serde(default)]
    bindings: Vec<ApiBinding>,
    #[serde(default)]
    previous_bindings: Vec<ApiBinding>,
    #[serde(default)]
    group: Vec<TopologyApplyGroupPayloadMember>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct TopologyApplyGroupPayloadMember {
    topology_id: String,
    revision_id: String,
}

#[derive(Debug, Clone, Copy, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
enum TopologyApplyPhase {
    #[default]
    Full,
    Stage,
    Prepare,
    Finalize,
    FinalizeGroup,
    Abort,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct NodeLifecyclePayload {
    node_id: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ExternalHealthPayload {
    deployment_id: String,
    service_id: String,
    version: String,
    endpoint: String,
    protocol: String,
    #[serde(default)]
    health_path: String,
    artifact_digest: String,
}

struct ControlPlaneLeaseHeartbeat {
    stop: Option<mpsc::Sender<()>>,
    handle: Option<JoinHandle<()>>,
    state: Arc<ControlPlaneLeaseState>,
    job_id: String,
    lease_token: String,
}

struct ControlPlaneLeaseState {
    lost: AtomicBool,
    last_progress_ms: AtomicI64,
    lease_expires_at_ms: AtomicI64,
}

impl ControlPlaneLeaseHeartbeat {
    fn start(
        storage: DurableStore,
        job_id: String,
        lease_token: String,
        lease_expires_at_ms: i64,
    ) -> Result<Self, String> {
        Self::start_with_timing(
            storage,
            job_id,
            lease_token,
            lease_expires_at_ms,
            CONTROL_PLANE_HEARTBEAT_INTERVAL,
            CONTROL_PLANE_MAX_STALL_MS,
        )
    }

    fn start_with_timing(
        storage: DurableStore,
        job_id: String,
        lease_token: String,
        lease_expires_at_ms: i64,
        heartbeat_interval: Duration,
        max_stall_ms: i64,
    ) -> Result<Self, String> {
        if heartbeat_interval.is_zero() || max_stall_ms <= 0 {
            return Err("control-plane heartbeat timing must be positive".to_string());
        }
        let (stop, stopped) = mpsc::channel();
        let state = Arc::new(ControlPlaneLeaseState {
            lost: AtomicBool::new(false),
            last_progress_ms: AtomicI64::new(now_ms()),
            lease_expires_at_ms: AtomicI64::new(lease_expires_at_ms),
        });
        let heartbeat_state = Arc::clone(&state);
        let heartbeat_job_id = job_id.clone();
        let heartbeat_lease_token = lease_token.clone();
        let handle = thread::Builder::new()
            .name("orchestrator-control-plane-heartbeat".to_string())
            .spawn(move || {
                let mut delay = heartbeat_interval;
                loop {
                    match stopped.recv_timeout(delay) {
                        Ok(()) | Err(mpsc::RecvTimeoutError::Disconnected) => break,
                        Err(mpsc::RecvTimeoutError::Timeout) => {
                            let heartbeat_at = now_ms();
                            if heartbeat_at.saturating_sub(
                                heartbeat_state.last_progress_ms.load(Ordering::Acquire),
                            ) >= max_stall_ms
                            {
                                heartbeat_state.lost.store(true, Ordering::Release);
                                eprintln!(
                                    "control-plane Job {heartbeat_job_id} stopped heartbeating after {max_stall_ms}ms without progress"
                                );
                                break;
                            }
                            let mut jobs = storage.job_store();
                            match jobs.heartbeat(HeartbeatRequest {
                                job_id: heartbeat_job_id.clone(),
                                lease_token: heartbeat_lease_token.clone(),
                                now_ms: heartbeat_at,
                                lease_ms: DEFAULT_LEASE_MS,
                                events: Vec::new(),
                            }) {
                                Ok(job) => {
                                    heartbeat_state.lease_expires_at_ms.store(
                                        job.lease_expires_at_ms.unwrap_or(heartbeat_at),
                                        Ordering::Release,
                                    );
                                    delay = heartbeat_interval;
                                }
                                Err(JobError::StaleLease) => {
                                    heartbeat_state.lost.store(true, Ordering::Release);
                                    eprintln!(
                                        "control-plane Job {heartbeat_job_id} heartbeat lost its lease"
                                    );
                                    break;
                                }
                                Err(error) => {
                                    eprintln!(
                                        "control-plane Job {heartbeat_job_id} heartbeat error: {error}"
                                    );
                                    if heartbeat_at
                                        >= heartbeat_state
                                            .lease_expires_at_ms
                                            .load(Ordering::Acquire)
                                    {
                                        heartbeat_state.lost.store(true, Ordering::Release);
                                        break;
                                    }
                                    delay = Duration::from_secs(1);
                                }
                            }
                        }
                    }
                }
            })
            .map_err(|error| format!("spawn control-plane Job heartbeat: {error}"))?;
        Ok(Self {
            stop: Some(stop),
            handle: Some(handle),
            state,
            job_id,
            lease_token,
        })
    }

    fn checkpoint(&self, jobs: &mut crate::durable::DurableJobStore) -> Result<(), String> {
        if self.state.lost.load(Ordering::Acquire) {
            return Err(format!(
                "control-plane Job {} lost its execution lease",
                self.job_id
            ));
        }
        let heartbeat_at = now_ms();
        match jobs.heartbeat(HeartbeatRequest {
            job_id: self.job_id.clone(),
            lease_token: self.lease_token.clone(),
            now_ms: heartbeat_at,
            lease_ms: DEFAULT_LEASE_MS,
            events: Vec::new(),
        }) {
            Ok(job) => {
                self.state.lease_expires_at_ms.store(
                    job.lease_expires_at_ms.unwrap_or(heartbeat_at),
                    Ordering::Release,
                );
                self.state
                    .last_progress_ms
                    .store(heartbeat_at, Ordering::Release);
                Ok(())
            }
            Err(error) => {
                if matches!(error, JobError::StaleLease)
                    || heartbeat_at >= self.state.lease_expires_at_ms.load(Ordering::Acquire)
                {
                    self.state.lost.store(true, Ordering::Release);
                }
                Err(format!(
                    "control-plane Job {} lease checkpoint failed: {error}",
                    self.job_id
                ))
            }
        }
    }
}

impl Drop for ControlPlaneLeaseHeartbeat {
    fn drop(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

pub(crate) fn run_loop(
    storage: DurableStore,
    provider: Option<TopologyProviderSaga>,
    shutdown: Arc<AtomicBool>,
) {
    let reconcile_provider = provider.clone();
    let reconcile_storage = storage.clone();
    let reconcile_shutdown = Arc::clone(&shutdown);
    let reconciler = thread::Builder::new()
        .name("orchestrator-topology-reconciler".to_string())
        .spawn(move || {
            run_reconciler_loop(
                &reconcile_storage,
                reconcile_provider.as_ref(),
                &reconcile_shutdown,
            )
        })
        .ok();
    let mut last_terminal_recovery_ms = 0_i64;
    while !shutdown.load(Ordering::Acquire) {
        let now = now_ms();
        if now.saturating_sub(last_terminal_recovery_ms) >= 1_000 {
            if let Err(error) = recover_terminal_topology_applies(&storage) {
                eprintln!("topology terminal-operation recovery error: {error}");
            }
            last_terminal_recovery_ms = now;
        }
        match process_one(&storage, provider.as_ref()) {
            Ok(true) => {}
            Ok(false) => thread::sleep(Duration::from_millis(100)),
            Err(error) => {
                eprintln!("topology control-plane worker error: {error}");
                thread::sleep(Duration::from_secs(1));
            }
        }
    }
    if let Some(reconciler) = reconciler {
        let _ = reconciler.join();
    }
}

/// The sole periodic owner of expired-lease recovery. Claims never perform
/// recovery, so 100 long-polling Agents cannot multiply full recovery scans or
/// serialize the queue mutex hundreds of times per second.
pub(crate) fn run_lease_recovery_loop(storage: DurableStore, shutdown: Arc<AtomicBool>) {
    while !shutdown.load(Ordering::Acquire) {
        if let Err(error) = recover_expired(&storage, now_ms()) {
            eprintln!("control-plane lease recovery error: {error}");
        }
        if let Err(error) = repair_recoverable_operation_projections(&storage, now_ms()) {
            eprintln!("control-plane Operation projection repair error: {error}");
        }
        let deadline = std::time::Instant::now() + Duration::from_secs(1);
        while !shutdown.load(Ordering::Acquire) && std::time::Instant::now() < deadline {
            thread::sleep(Duration::from_millis(100));
        }
    }
}

fn run_reconciler_loop(
    storage: &DurableStore,
    provider: Option<&TopologyProviderSaga>,
    shutdown: &AtomicBool,
) {
    let network_probes = NetworkProbePool::new();
    while !shutdown.load(Ordering::Acquire) {
        let result = match provider {
            Some(provider) => reconcile_all(storage, provider, &network_probes),
            None => refresh_external_runtime_health(storage),
        };
        if let Err(error) = result {
            eprintln!("topology reconciler error: {error}");
        }
        let deadline = std::time::Instant::now() + RECONCILE_INTERVAL;
        while !shutdown.load(Ordering::Acquire) && std::time::Instant::now() < deadline {
            thread::sleep(Duration::from_millis(100));
        }
    }
}

fn reconcile_all(
    storage: &DurableStore,
    provider: &TopologyProviderSaga,
    network_probes: &NetworkProbePool,
) -> Result<(), String> {
    // External deployments have no authenticated Agent inventory. Their
    // persisted probe contract is therefore refreshed by the same bounded
    // reconciler before topology, Binding and provider status is projected.
    refresh_external_runtime_health(storage)?;
    // Runtime identity and structural attestation are inputs to the live
    // route/grant projection, not to the immutable TopologySpec.  Once a
    // Binding is active, transient health/report gaps retain its recovery
    // route; desired stop/removal or structural drift still revokes it.
    // Provider projection is retried independently from Status observation.
    // A temporary Gateway/Auth management outage must remain visible to the
    // caller, but it must not starve every applied topology of fresh runtime
    // and provider observations for the whole reconciliation pass.
    let runtime_projection_error =
        reconcile_runtime_binding_projections(storage, Some(provider), None, false).err();
    for heads in storage
        .list_topology_heads()
        .map_err(|error| error.to_string())?
    {
        let Some(applied_revision_id) = heads.applied_revision_id.as_deref() else {
            continue;
        };
        if heads.applying_revision_id.is_some() {
            continue;
        }
        if let Err(error) = reconcile_one(
            storage,
            provider,
            &heads.topology_id,
            applied_revision_id,
            heads.last_operation_id,
            network_probes,
        ) {
            eprintln!(
                "topology {} observation could not be persisted: {error}",
                heads.topology_id
            );
        }
    }
    match runtime_projection_error {
        Some(error) => Err(error),
        None => Ok(()),
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct RuntimeBindingProjectionState {
    schema_version: u8,
    revision_id: String,
    content_sha256: String,
    #[serde(default)]
    projection_sha256: String,
    bindings: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RuntimeProjectionTransition {
    Unchanged,
    Revoke,
    Grant,
    Mixed,
}

/// Synchronizes the runtime-effective subset of an applied revision through
/// the formal Gateway/Auth topology projection contract.  `affected` limits
/// synchronous Agent callbacks to topologies that reference the changed
/// deployment; the periodic reconciler passes `None` to catch stale reports
/// and crash windows.
pub(crate) fn reconcile_runtime_binding_projections(
    storage: &DurableStore,
    provider: Option<&TopologyProviderSaga>,
    affected: Option<&BTreeSet<String>>,
    force_revoke: bool,
) -> Result<(), String> {
    for heads in storage
        .list_topology_heads()
        .map_err(|error| error.to_string())?
    {
        let Some(applied_revision_id) = heads.applied_revision_id.as_deref() else {
            continue;
        };
        let all_bindings = storage
            .api_bindings_for_topology(&heads.topology_id)
            .map_err(|error| error.to_string())?;
        if let Some(affected) = affected
            && !all_bindings.iter().any(|binding| {
                affected.contains(&binding.consumer_deployment_id)
                    || affected.contains(&binding.provider_deployment_id)
            })
        {
            continue;
        }

        let nominal = all_bindings
            .iter()
            .filter(|binding| {
                binding.topology_revision_id == applied_revision_id
                    && binding.desired_state == "ACTIVE"
                    && binding.state == ApiBindingState::Active
            })
            .cloned()
            .collect::<Vec<_>>();
        // A revoked consumer row is intentionally retained for audit, so an
        // explicit uninstall still reaches this topology even when `nominal`
        // is now empty. Topologies that have never owned an ApiBinding need no
        // runtime projection state at all.
        if nominal.is_empty()
            && all_bindings.iter().all(|binding| {
                binding.topology_revision_id != applied_revision_id
                    || (binding.desired_state != "REVOKED"
                        && binding.state != ApiBindingState::Revoked)
            })
        {
            continue;
        }

        let revision = storage
            .topology_revision(&heads.topology_id, applied_revision_id)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| {
                format!(
                    "applied topology revision {applied_revision_id} disappeared during runtime projection"
                )
            })?;
        let content_sha256 = revision
            .spec()
            .content_sha256()
            .map_err(|error| error.to_string())?;
        let effective = nominal
            .iter()
            .filter_map(
                |binding| match runtime_binding_route_is_admissible(storage, binding) {
                    Ok(true) => Some(Ok(binding.clone())),
                    Ok(false) => None,
                    Err(error) => Some(Err(error)),
                },
            )
            .collect::<Result<Vec<_>, _>>()?;
        let desired = runtime_projection_state(applied_revision_id, &content_sha256, &effective)?;
        let persisted = storage
            .get_state::<RuntimeBindingProjectionState>(
                RUNTIME_BINDING_PROJECTION_STATE_NAMESPACE,
                &heads.topology_id,
            )
            .map_err(|error| error.to_string())?
            .filter(|state| {
                state.schema_version == 1
                    && state.revision_id == applied_revision_id
                    && state.content_sha256 == content_sha256
            });
        let previous = match persisted.as_ref() {
            Some(state) => state.clone(),
            None => runtime_projection_state(applied_revision_id, &content_sha256, &nominal)?,
        };
        let mut transition = runtime_projection_transition(&previous.bindings, &desired.bindings);
        let mut repair_observed_mismatch = false;

        if transition == RuntimeProjectionTransition::Unchanged && !force_revoke {
            let provider = provider.ok_or_else(|| {
                "Topology provider is unavailable while verifying runtime projection state"
                    .to_string()
            })?;
            let observed = provider.observe(&heads.topology_id);
            if observed.gateway.matches(
                applied_revision_id,
                &content_sha256,
                &desired.projection_sha256,
            ) && observed.auth.matches(
                applied_revision_id,
                &content_sha256,
                &desired.projection_sha256,
            ) {
                if persisted.as_ref() != Some(&desired) {
                    storage
                        .put_state(
                            RUNTIME_BINDING_PROJECTION_STATE_NAMESPACE,
                            &heads.topology_id,
                            &desired,
                        )
                        .map_err(|error| error.to_string())?;
                }
                continue;
            }
            let present_mismatch =
                [&observed.gateway, &observed.auth]
                    .into_iter()
                    .any(|observation| {
                        observation.state == TopologyProviderObservedState::Present
                            && !observation.matches(
                                applied_revision_id,
                                &content_sha256,
                                &desired.projection_sha256,
                            )
                    });
            let direct_grant = !present_mismatch
                && [&observed.gateway, &observed.auth]
                    .into_iter()
                    .all(|observation| {
                        observation.state == TopologyProviderObservedState::Absent
                            || observation.matches(
                                applied_revision_id,
                                &content_sha256,
                                &desired.projection_sha256,
                            )
                    });
            if direct_grant {
                // A genuinely absent projection has no stale authority to
                // revoke. Granting Auth before Gateway is sufficient and
                // preserves the normal first-install ordering.
                transition = RuntimeProjectionTransition::Grant;
            } else {
                // A present-but-different projection may contain an unknown
                // route or grant even when revision/spec hashes still match.
                // Converge both providers to an empty intersection first;
                // only then repopulate the exact desired projection.
                repair_observed_mismatch = true;
            }
        } else if transition == RuntimeProjectionTransition::Unchanged {
            transition = RuntimeProjectionTransition::Revoke;
        }

        let provider = provider.ok_or_else(|| {
            "Topology provider is unavailable while runtime binding revocation is required"
                .to_string()
        })?;
        if repair_observed_mismatch {
            let safe = Vec::new();
            let safe_state = runtime_projection_state(applied_revision_id, &content_sha256, &safe)?;
            provider.apply_runtime_projection(
                &heads.topology_id,
                applied_revision_id,
                revision.spec(),
                &safe,
                &runtime_projection_operation_id(&heads.topology_id, &safe_state, "repair-revoke"),
                RuntimeProjectionOrder::RevokeFirst,
            )?;
            provider.apply_runtime_projection(
                &heads.topology_id,
                applied_revision_id,
                revision.spec(),
                &effective,
                &runtime_projection_operation_id(&heads.topology_id, &desired, "repair-grant"),
                RuntimeProjectionOrder::GrantFirst,
            )?;
        } else if transition == RuntimeProjectionTransition::Mixed {
            let safe = effective
                .iter()
                .filter(|binding| {
                    previous.bindings.get(&binding.binding_id)
                        == desired.bindings.get(&binding.binding_id)
                })
                .cloned()
                .collect::<Vec<_>>();
            let safe_state = runtime_projection_state(applied_revision_id, &content_sha256, &safe)?;
            provider.apply_runtime_projection(
                &heads.topology_id,
                applied_revision_id,
                revision.spec(),
                &safe,
                &runtime_projection_operation_id(&heads.topology_id, &safe_state, "revoke"),
                RuntimeProjectionOrder::RevokeFirst,
            )?;
            provider.apply_runtime_projection(
                &heads.topology_id,
                applied_revision_id,
                revision.spec(),
                &effective,
                &runtime_projection_operation_id(&heads.topology_id, &desired, "grant"),
                RuntimeProjectionOrder::GrantFirst,
            )?;
        } else {
            let order = match transition {
                RuntimeProjectionTransition::Revoke => RuntimeProjectionOrder::RevokeFirst,
                RuntimeProjectionTransition::Grant => RuntimeProjectionOrder::GrantFirst,
                RuntimeProjectionTransition::Unchanged | RuntimeProjectionTransition::Mixed => {
                    unreachable!("runtime projection transition was normalized above")
                }
            };
            let phase = if order == RuntimeProjectionOrder::RevokeFirst {
                "revoke"
            } else {
                "grant"
            };
            provider.apply_runtime_projection(
                &heads.topology_id,
                applied_revision_id,
                revision.spec(),
                &effective,
                &runtime_projection_operation_id(&heads.topology_id, &desired, phase),
                order,
            )?;
        }
        storage
            .put_state(
                RUNTIME_BINDING_PROJECTION_STATE_NAMESPACE,
                &heads.topology_id,
                &desired,
            )
            .map_err(|error| error.to_string())?;
    }
    Ok(())
}

/// Decides whether an already-activated Binding remains authorized for route
/// and grant projection. This deliberately differs from Binding health: a
/// workload can become unhealthy precisely because its provider is briefly
/// unavailable. Revoking the route in that state creates a circular recovery
/// dependency (the workload needs the route in order to become healthy again).
///
/// Initial apply still uses the strict consumer/provider health gates. After
/// activation we retain authorization across transient health, observation,
/// and heartbeat failures, while failing closed for desired stop/removal,
/// assignment changes, failed runtime attestation, and structural drift.
fn runtime_binding_route_is_admissible(
    storage: &DurableStore,
    binding: &ApiBinding,
) -> Result<bool, String> {
    if binding.desired_state != "ACTIVE"
        || binding.state != ApiBindingState::Active
        || binding.observed_state != "ACTIVE"
        || !binding.drift.is_empty()
        || !binding.reason.trim().is_empty()
    {
        return Ok(false);
    }
    for (deployment_id, service_id, node_id) in [
        (
            binding.consumer_deployment_id.as_str(),
            binding.consumer_service_id.as_str(),
            binding.consumer_node_id.as_str(),
        ),
        (
            binding.provider_deployment_id.as_str(),
            binding.provider_service_id.as_str(),
            binding.provider_node_id.as_str(),
        ),
    ] {
        let Some(runtime) = storage
            .runtime_instance(deployment_id)
            .map_err(|error| error.to_string())?
        else {
            return Ok(false);
        };
        if runtime.instance.service_id != service_id
            || runtime.node_id != node_id
            || !runtime_preserves_active_binding_route(&runtime)
        {
            return Ok(false);
        }
    }
    Ok(true)
}

pub(crate) fn runtime_preserves_active_binding_route(runtime: &StoredRuntimeInstance) -> bool {
    runtime.instance.desired_state == RuntimeDesiredState::Running
        && runtime.drift_reason.trim().is_empty()
        && (runtime.management_mode != RuntimeManagementMode::Managed
            || runtime.instance.runtime_attested)
}

fn runtime_projection_state(
    revision_id: &str,
    content_sha256: &str,
    bindings: &[ApiBinding],
) -> Result<RuntimeBindingProjectionState, String> {
    let mut projected = BTreeMap::new();
    for binding in bindings {
        let encoded = serde_json::to_vec(binding).map_err(|error| error.to_string())?;
        let digest = Sha256::digest(encoded);
        if projected
            .insert(binding.binding_id.clone(), format!("{digest:x}"))
            .is_some()
        {
            return Err(format!(
                "runtime projection repeats binding {}",
                binding.binding_id
            ));
        }
    }
    Ok(RuntimeBindingProjectionState {
        schema_version: 1,
        revision_id: revision_id.to_string(),
        content_sha256: content_sha256.to_string(),
        projection_sha256: provider_projection_sha256(bindings)?,
        bindings: projected,
    })
}

fn runtime_projection_transition(
    previous: &BTreeMap<String, String>,
    desired: &BTreeMap<String, String>,
) -> RuntimeProjectionTransition {
    if previous == desired {
        return RuntimeProjectionTransition::Unchanged;
    }
    let desired_is_subset = desired
        .iter()
        .all(|(id, digest)| previous.get(id) == Some(digest));
    let previous_is_subset = previous
        .iter()
        .all(|(id, digest)| desired.get(id) == Some(digest));
    if desired_is_subset {
        RuntimeProjectionTransition::Revoke
    } else if previous_is_subset {
        RuntimeProjectionTransition::Grant
    } else {
        RuntimeProjectionTransition::Mixed
    }
}

fn runtime_projection_operation_id(
    topology_id: &str,
    state: &RuntimeBindingProjectionState,
    phase: &str,
) -> String {
    let mut hasher = Sha256::new();
    hasher.update(topology_id.as_bytes());
    hasher.update([0]);
    hasher.update(state.revision_id.as_bytes());
    hasher.update([0]);
    hasher.update(state.content_sha256.as_bytes());
    hasher.update([0]);
    hasher.update(state.projection_sha256.as_bytes());
    hasher.update([0]);
    hasher.update(serde_json::to_vec(&state.bindings).unwrap_or_default());
    let digest = format!("{:x}", hasher.finalize());
    format!("runtime-projection-{}-{phase}", &digest[..32])
}

fn reconcile_one(
    storage: &DurableStore,
    provider: &TopologyProviderSaga,
    topology_id: &str,
    applied_revision_id: &str,
    last_operation_id: Option<String>,
    network_probes: &NetworkProbePool,
) -> Result<(), String> {
    let revision = storage
        .topology_revision(topology_id, applied_revision_id)
        .map_err(|error| error.to_string())?
        .ok_or_else(|| format!("applied revision {applied_revision_id} disappeared"))?;
    let content_sha256 = revision
        .spec()
        .content_sha256()
        .map_err(|error| error.to_string())?;

    // Provider I/O is deliberately complete before the final status CAS.
    let providers = provider.observe(topology_id);
    let evidence_at_ms = now_ms();
    let runtime_instances = storage
        .runtime_instances(None)
        .map_err(|error| error.to_string())?
        .into_iter()
        .map(|runtime| storage.runtime_with_current_evidence(runtime, evidence_at_ms))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| error.to_string())?;
    let stored_api_bindings = storage
        .api_bindings_for_topology(topology_id)
        .map_err(|error| error.to_string())?;
    let effective_bindings = stored_api_bindings
        .iter()
        .filter_map(
            |binding| match runtime_binding_route_is_admissible(storage, binding) {
                Ok(true) => Some(Ok(binding.clone())),
                Ok(false) => None,
                Err(error) => Some(Err(error)),
            },
        )
        .collect::<Result<Vec<_>, _>>()?;
    let projection_sha256 = provider_projection_sha256(&effective_bindings)?;
    let api_bindings = stored_api_bindings
        .into_iter()
        .map(|binding| storage.binding_with_current_runtime_evidence(binding, evidence_at_ms))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| error.to_string())?;
    let link_probe_source_endpoints = storage
        .link_probe_source_endpoints(revision.spec())
        .unwrap_or_else(|error| {
            eprintln!("topology {topology_id} Link probe release binding is unavailable: {error}");
            BTreeSet::new()
        });
    let previous_status = storage
        .topology_status(topology_id)
        .map_err(|error| error.to_string())?;
    let observed_at = now_marker();
    let mut drift = Vec::new();
    add_provider_drift(
        &mut drift,
        topology_id,
        &providers.gateway,
        applied_revision_id,
        &content_sha256,
        &projection_sha256,
    );
    add_provider_drift(
        &mut drift,
        topology_id,
        &providers.auth,
        applied_revision_id,
        &content_sha256,
        &projection_sha256,
    );
    let (deployments, endpoints, links) = runtime_topology_status(
        revision.spec(),
        &runtime_instances,
        &api_bindings,
        &link_probe_source_endpoints,
        &providers,
        previous_status.as_ref(),
        network_probes,
        &observed_at,
        &mut drift,
    );
    drift.sort_by(|left, right| {
        (&left.resource_kind, &left.resource_id).cmp(&(&right.resource_kind, &right.resource_id))
    });
    let providers_match =
        providers
            .gateway
            .matches(applied_revision_id, &content_sha256, &projection_sha256)
            && providers
                .auth
                .matches(applied_revision_id, &content_sha256, &projection_sha256);
    let status = TopologyStatus {
        topology_id: topology_id.to_string(),
        desired_revision_id: Some(applied_revision_id.to_string()),
        observed_revision_id: providers_match.then(|| applied_revision_id.to_string()),
        state: if drift.is_empty() && providers_match {
            TopologyReconciliationState::InSync
        } else {
            TopologyReconciliationState::Degraded
        },
        deployments,
        endpoints,
        links,
        drift,
        last_operation_id,
        updated_at: observed_at,
    };
    storage
        .put_reconciled_topology_status(&status, applied_revision_id)
        .map_err(|error| error.to_string())
}

fn add_provider_drift(
    drift: &mut Vec<TopologyDrift>,
    topology_id: &str,
    observation: &TopologyProviderObservation,
    desired_revision_id: &str,
    desired_content_sha256: &str,
    desired_projection_sha256: &str,
) {
    if observation.matches(
        desired_revision_id,
        desired_content_sha256,
        desired_projection_sha256,
    ) {
        return;
    }
    let (kind, detail) = match observation.state {
        TopologyProviderObservedState::Absent => (
            TopologyDriftKind::Missing,
            format!(
                "{} provider has no topology projection",
                observation.provider
            ),
        ),
        TopologyProviderObservedState::Unreachable => (
            TopologyDriftKind::Unreachable,
            format!(
                "{} provider could not be observed: {}",
                observation.provider, observation.detail
            ),
        ),
        TopologyProviderObservedState::Present => (
            TopologyDriftKind::Changed,
            format!(
                "{} provider reports revision {:?}, content hash {:?}, and effective projection hash {:?}; expected {desired_revision_id}, {desired_content_sha256}, and {desired_projection_sha256}",
                observation.provider,
                observation.observed_revision_id,
                observation.observed_content_sha256,
                observation.observed_projection_sha256
            ),
        ),
    };
    drift.push(TopologyDrift {
        resource_kind: TopologyResourceKind::Authority,
        resource_id: format!("{topology_id}/{}", observation.provider),
        kind,
        detail: bounded_detail(&detail),
    });
}

// Reconciliation joins the immutable Spec, runtime projection, provider
// observations and bounded network evidence in one pure projection step.
#[allow(clippy::too_many_arguments)]
fn runtime_topology_status(
    spec: &TopologySpec,
    runtime_instances: &[orchestrator_storage::StoredRuntimeInstance],
    api_bindings: &[ApiBinding],
    link_probe_source_endpoints: &BTreeSet<String>,
    providers: &TopologyProvidersObservation,
    previous_status: Option<&TopologyStatus>,
    network_probes: &NetworkProbePool,
    observed_at: &str,
    drift: &mut Vec<TopologyDrift>,
) -> (
    Vec<TopologyDeploymentStatus>,
    Vec<TopologyEndpointStatus>,
    Vec<TopologyLinkStatus>,
) {
    let service_ids = spec
        .endpoints
        .iter()
        .map(|endpoint| endpoint.service_id.as_str())
        .collect::<std::collections::BTreeSet<_>>();
    let endpoint_ids = spec
        .endpoints
        .iter()
        .map(|endpoint| endpoint.endpoint.as_str())
        .collect::<std::collections::BTreeSet<_>>();
    let link_ids = spec
        .links
        .iter()
        .map(|link| (link.source_endpoint.as_str(), link.target_endpoint.as_str()))
        .collect::<std::collections::BTreeSet<_>>();
    for provider in [&providers.gateway, &providers.auth] {
        for endpoint in &provider.endpoints {
            if !endpoint_ids.contains(endpoint.endpoint.as_str()) {
                drift.push(TopologyDrift {
                    resource_kind: TopologyResourceKind::Endpoint,
                    resource_id: format!("{}/{}", provider.provider, endpoint.endpoint),
                    kind: TopologyDriftKind::Unexpected,
                    detail: format!(
                        "{} provider reports an endpoint outside the applied spec",
                        provider.provider
                    ),
                });
            }
        }
        for link in &provider.links {
            if !link_ids.contains(&(link.source_endpoint.as_str(), link.target_endpoint.as_str())) {
                drift.push(TopologyDrift {
                    resource_kind: TopologyResourceKind::Link,
                    resource_id: format!(
                        "{}/{}->{}",
                        provider.provider, link.source_endpoint, link.target_endpoint
                    ),
                    kind: TopologyDriftKind::Unexpected,
                    detail: format!(
                        "{} provider reports a link outside the applied spec",
                        provider.provider
                    ),
                });
            }
        }
    }
    let mut relevant = runtime_instances
        .iter()
        .filter(|stored| service_ids.contains(stored.instance.service_id.as_str()))
        .collect::<Vec<_>>();
    relevant.sort_by_key(|stored| stored.instance.deployment_id.as_str());
    let deployments = relevant
        .iter()
        .map(|stored| {
            let desired_state = desired_deployment_state(&stored.instance.desired_state);
            let observed_state = observed_deployment_state(&stored.instance.observed_state);
            let health = runtime_health(&stored.instance.health);
            let mut deployment_drift = Vec::new();
            if !runtime_states_match(
                &stored.instance.desired_state,
                &stored.instance.observed_state,
            ) {
                deployment_drift
                    .push("runtime observed state does not match desired state".to_string());
            }
            if stored.management_mode == RuntimeManagementMode::Managed
                && !stored.instance.runtime_attested
            {
                deployment_drift
                    .push("managed runtime has no current Agent attestation".to_string());
            }
            if !stored.drift_reason.trim().is_empty() {
                deployment_drift.push(stored.drift_reason.clone());
            }
            if health != TopologyHealth::Healthy {
                deployment_drift.push("runtime health is not HEALTHY".to_string());
            }
            if !deployment_drift.is_empty() {
                drift.push(TopologyDrift {
                    resource_kind: TopologyResourceKind::Deployment,
                    resource_id: stored.instance.deployment_id.clone(),
                    kind: if stored.instance.observed_state == RuntimeObservedState::Missing {
                        TopologyDriftKind::Missing
                    } else {
                        TopologyDriftKind::Changed
                    },
                    detail: bounded_detail(&deployment_drift.join("; ")),
                });
            }
            TopologyDeploymentStatus {
                deployment_id: stored.instance.deployment_id.clone(),
                service_id: stored.instance.service_id.clone(),
                node_id: stored.node_id.clone(),
                desired_state,
                observed_state,
                health,
                // RuntimeInstance v1 does not expose a generation counter. A
                // zero pair explicitly means unreported rather than invented.
                desired_generation: 0,
                observed_generation: 0,
                message: if stored.instance.health.eq_ignore_ascii_case("healthy") {
                    String::new()
                } else {
                    "runtime health is not healthy".to_string()
                },
            }
        })
        .collect::<Vec<_>>();

    let (endpoints, links) = observed_network_status(
        spec,
        &relevant,
        NetworkObservationContext {
            api_bindings,
            link_probe_source_endpoints,
            previous_status,
            network_probes,
            observed_at,
        },
        drift,
    );
    (deployments, endpoints, links)
}

#[derive(Debug, Clone)]
struct EndpointProbeTask {
    endpoint: String,
    service_id: String,
    protocol: String,
    health_path: String,
}

#[derive(Debug, Clone)]
struct LinkProbeTask {
    source_endpoint: String,
    source_service_id: String,
    source_protocol: String,
    target_endpoint: String,
    target_service_id: String,
}

struct NetworkObservationContext<'a> {
    api_bindings: &'a [ApiBinding],
    link_probe_source_endpoints: &'a BTreeSet<String>,
    previous_status: Option<&'a TopologyStatus>,
    network_probes: &'a NetworkProbePool,
    observed_at: &'a str,
}

fn observed_network_status(
    spec: &TopologySpec,
    relevant: &[&StoredRuntimeInstance],
    context: NetworkObservationContext<'_>,
    drift: &mut Vec<TopologyDrift>,
) -> (Vec<TopologyEndpointStatus>, Vec<TopologyLinkStatus>) {
    let NetworkObservationContext {
        api_bindings,
        link_probe_source_endpoints,
        previous_status,
        network_probes,
        observed_at,
    } = context;
    let now = now_ms();
    let previous_endpoints = previous_status
        .map(|status| {
            status
                .endpoints
                .iter()
                .map(|endpoint| (endpoint.endpoint.as_str(), endpoint))
                .collect::<BTreeMap<_, _>>()
        })
        .unwrap_or_default();
    let mut endpoint_tasks = Vec::new();
    let mut endpoint_statuses = BTreeMap::new();
    let binding_consumers = spec
        .links
        .iter()
        .filter(|link| link.enabled && !link.api_bindings.is_empty())
        .map(|link| link.source_endpoint.as_str())
        .collect::<BTreeSet<_>>();
    for endpoint in &spec.endpoints {
        let configured_deployment = endpoint
            .config
            .as_object()
            .and_then(|config| config.get("deployment_id"))
            .and_then(Value::as_str)
            .filter(|deployment_id| !deployment_id.trim().is_empty());
        let matching = relevant
            .iter()
            .copied()
            .filter(|stored| {
                stored.instance.service_id == endpoint.service_id
                    && configured_deployment.map_or_else(
                        || stored.endpoint == endpoint.endpoint,
                        |deployment_id| stored.instance.deployment_id == deployment_id,
                    )
            })
            .collect::<Vec<_>>();
        if matching.len() != 1 {
            endpoint_statuses.insert(
                endpoint.endpoint.clone(),
                TopologyEndpointStatus {
                    endpoint: endpoint.endpoint.clone(),
                    health: TopologyHealth::Unknown,
                    reachable: false,
                    latency_ms: None,
                    message: if matching.is_empty() {
                        "no runtime projection owns this exact endpoint".to_string()
                    } else {
                        "multiple runtime projections claim this exact endpoint".to_string()
                    },
                    observed_at: observed_at.to_string(),
                },
            );
            continue;
        }
        let stored = matching[0];
        if stored.instance.desired_state != RuntimeDesiredState::Running
            || stored.instance.observed_state != RuntimeObservedState::Running
            || runtime_health(&stored.instance.health) != TopologyHealth::Healthy
        {
            endpoint_statuses.insert(
                endpoint.endpoint.clone(),
                TopologyEndpointStatus {
                    endpoint: endpoint.endpoint.clone(),
                    health: if runtime_health(&stored.instance.health) == TopologyHealth::Unhealthy
                    {
                        TopologyHealth::Unhealthy
                    } else {
                        TopologyHealth::Unknown
                    },
                    reachable: false,
                    latency_ms: None,
                    message: "exact runtime projection is not healthy and Running".to_string(),
                    observed_at: observed_at.to_string(),
                },
            );
            continue;
        }
        if binding_consumers.contains(endpoint.endpoint.as_str()) {
            endpoint_statuses.insert(
                endpoint.endpoint.clone(),
                TopologyEndpointStatus {
                    endpoint: endpoint.endpoint.clone(),
                    health: TopologyHealth::Healthy,
                    reachable: true,
                    latency_ms: None,
                    message: "outbound ApiBinding consumer health is derived from its exact RuntimeInstance"
                        .to_string(),
                    observed_at: observed_at.to_string(),
                },
            );
            continue;
        }
        endpoint_tasks.push(EndpointProbeTask {
            endpoint: endpoint.endpoint.clone(),
            service_id: endpoint.service_id.clone(),
            protocol: endpoint.protocol.clone(),
            health_path: if endpoint.health_path.is_empty() {
                "/health".to_string()
            } else {
                endpoint.health_path.clone()
            },
        });
    }
    endpoint_tasks.sort_by_key(|task| {
        previous_endpoints
            .get(task.endpoint.as_str())
            .and_then(|status| {
                trusted_observation_ms(
                    status.observed_at.as_str(),
                    &status.message,
                    ENDPOINT_EVIDENCE_PREFIX,
                    now,
                )
            })
            .unwrap_or(i64::MIN)
    });
    let selected_endpoint_ids = endpoint_tasks
        .iter()
        .take(ENDPOINT_PROBE_BATCH)
        .map(|task| task.endpoint.as_str())
        .collect::<BTreeSet<_>>();
    let endpoint_probe_tasks = endpoint_tasks
        .iter()
        .filter(|task| selected_endpoint_ids.contains(task.endpoint.as_str()))
        .cloned()
        .collect::<Vec<_>>();
    let endpoint_probe_results = network_probes
        .probe_endpoints(&endpoint_probe_tasks, observed_at)
        .into_iter()
        .map(|status| (status.endpoint.clone(), status))
        .collect::<BTreeMap<_, _>>();
    for task in endpoint_tasks {
        let status = endpoint_probe_results
            .get(&task.endpoint)
            .cloned()
            .or_else(|| {
                previous_endpoints
                    .get(task.endpoint.as_str())
                    .and_then(|status| {
                        trusted_observation_ms(
                            &status.observed_at,
                            &status.message,
                            ENDPOINT_EVIDENCE_PREFIX,
                            now,
                        )
                        .map(|_| (*status).clone())
                    })
            })
            .unwrap_or_else(|| TopologyEndpointStatus {
                endpoint: task.endpoint.clone(),
                health: TopologyHealth::Unknown,
                reachable: false,
                latency_ms: None,
                message: "network probe: pending bounded observation batch".to_string(),
                observed_at: String::new(),
            });
        endpoint_statuses.insert(task.endpoint, status);
    }
    let endpoints = spec
        .endpoints
        .iter()
        .map(|endpoint| {
            let status = endpoint_statuses
                .remove(&endpoint.endpoint)
                .expect("every endpoint receives an observed status");
            if status.health != TopologyHealth::Healthy || !status.reachable {
                drift.push(TopologyDrift {
                    resource_kind: TopologyResourceKind::Endpoint,
                    resource_id: endpoint.endpoint.clone(),
                    kind: if status.message.starts_with("no runtime projection") {
                        TopologyDriftKind::Missing
                    } else {
                        TopologyDriftKind::Unreachable
                    },
                    detail: bounded_detail(&status.message),
                });
            }
            status
        })
        .collect::<Vec<_>>();

    let endpoint_status_by_id = endpoints
        .iter()
        .map(|status| (status.endpoint.as_str(), status))
        .collect::<BTreeMap<_, _>>();
    let endpoint_spec_by_id = spec
        .endpoints
        .iter()
        .map(|endpoint| (endpoint.endpoint.as_str(), endpoint))
        .collect::<BTreeMap<_, _>>();
    let previous_links = previous_status
        .map(|status| {
            status
                .links
                .iter()
                .map(|link| {
                    (
                        (link.source_endpoint.as_str(), link.target_endpoint.as_str()),
                        link,
                    )
                })
                .collect::<BTreeMap<_, _>>()
        })
        .unwrap_or_default();
    let mut link_tasks = Vec::new();
    let mut link_statuses = BTreeMap::new();
    for link in &spec.links {
        let key = (link.source_endpoint.clone(), link.target_endpoint.clone());
        if !link.enabled {
            link_statuses.insert(
                key,
                TopologyLinkStatus {
                    source_endpoint: link.source_endpoint.clone(),
                    target_endpoint: link.target_endpoint.clone(),
                    health: TopologyHealth::Unknown,
                    latency_ms: None,
                    message: "link is disabled and was not probed".to_string(),
                    observed_at: observed_at.to_string(),
                },
            );
            continue;
        }
        if !link.api_bindings.is_empty() {
            let observed = link
                .api_bindings
                .iter()
                .map(|declared| {
                    api_bindings.iter().find(|binding| {
                        binding.requirement_name == declared.requirement_name
                            && binding.api_id == declared.api_id
                            && binding.link_source_endpoint == link.source_endpoint
                            && binding.link_target_endpoint == link.target_endpoint
                    })
                })
                .collect::<Vec<_>>();
            let healthy = observed.iter().all(|binding| {
                binding.is_some_and(|binding| {
                    binding.state == ApiBindingState::Active
                        && binding.desired_state == "ACTIVE"
                        && binding.observed_state == "ACTIVE"
                        && binding.health == "HEALTHY"
                })
            });
            link_statuses.insert(
                key,
                TopologyLinkStatus {
                    source_endpoint: link.source_endpoint.clone(),
                    target_endpoint: link.target_endpoint.clone(),
                    health: if healthy {
                        TopologyHealth::Healthy
                    } else {
                        TopologyHealth::Unhealthy
                    },
                    latency_ms: None,
                    message: if healthy {
                        "all ApiBindings are ACTIVE and healthy".to_string()
                    } else {
                        "one or more ApiBindings are missing, inactive, or unhealthy".to_string()
                    },
                    observed_at: observed_at.to_string(),
                },
            );
            continue;
        }
        let source = endpoint_spec_by_id
            .get(link.source_endpoint.as_str())
            .expect("validated link source exists");
        if !link_probe_source_endpoints.contains(&link.source_endpoint) {
            link_statuses.insert(
                key,
                TopologyLinkStatus {
                    source_endpoint: link.source_endpoint.clone(),
                    target_endpoint: link.target_endpoint.clone(),
                    health: TopologyHealth::Unknown,
                    latency_ms: None,
                    message: format!(
                        "source endpoint {} has no exact release-bound orchestrator.link-probe.v1 capability",
                        link.source_endpoint
                    ),
                    observed_at: observed_at.to_string(),
                },
            );
            continue;
        }
        let source_status = endpoint_status_by_id.get(link.source_endpoint.as_str());
        let target_status = endpoint_status_by_id.get(link.target_endpoint.as_str());
        if ![source_status, target_status]
            .into_iter()
            .flatten()
            .all(|status| status.health == TopologyHealth::Healthy && status.reachable)
            || source_status.is_none()
            || target_status.is_none()
        {
            link_statuses.insert(
                key,
                TopologyLinkStatus {
                    source_endpoint: link.source_endpoint.clone(),
                    target_endpoint: link.target_endpoint.clone(),
                    health: TopologyHealth::Unknown,
                    latency_ms: None,
                    message: "source or target endpoint lacks fresh healthy network evidence"
                        .to_string(),
                    observed_at: observed_at.to_string(),
                },
            );
            continue;
        }
        let target = endpoint_spec_by_id
            .get(link.target_endpoint.as_str())
            .expect("validated link target exists");
        link_tasks.push(LinkProbeTask {
            source_endpoint: link.source_endpoint.clone(),
            source_service_id: source.service_id.clone(),
            source_protocol: source.protocol.clone(),
            target_endpoint: link.target_endpoint.clone(),
            target_service_id: target.service_id.clone(),
        });
    }
    link_tasks.sort_by_key(|task| {
        previous_links
            .get(&(task.source_endpoint.as_str(), task.target_endpoint.as_str()))
            .and_then(|status| {
                trusted_observation_ms(
                    &status.observed_at,
                    &status.message,
                    LINK_EVIDENCE_PREFIX,
                    now,
                )
            })
            .unwrap_or(i64::MIN)
    });
    let selected_link_ids = link_tasks
        .iter()
        .take(LINK_PROBE_BATCH)
        .map(|task| (task.source_endpoint.as_str(), task.target_endpoint.as_str()))
        .collect::<BTreeSet<_>>();
    let link_probe_tasks = link_tasks
        .iter()
        .filter(|task| {
            selected_link_ids
                .contains(&(task.source_endpoint.as_str(), task.target_endpoint.as_str()))
        })
        .cloned()
        .collect::<Vec<_>>();
    let link_probe_results = network_probes
        .probe_links(&link_probe_tasks, observed_at)
        .into_iter()
        .map(|status| {
            (
                (
                    status.source_endpoint.clone(),
                    status.target_endpoint.clone(),
                ),
                status,
            )
        })
        .collect::<BTreeMap<_, _>>();
    for task in link_tasks {
        let key = (task.source_endpoint.clone(), task.target_endpoint.clone());
        let status = link_probe_results.get(&key).cloned().or_else(|| {
            previous_links
                .get(&(task.source_endpoint.as_str(), task.target_endpoint.as_str()))
                .and_then(|status| {
                    trusted_observation_ms(
                        &status.observed_at,
                        &status.message,
                        LINK_EVIDENCE_PREFIX,
                        now,
                    )
                    .map(|_| (*status).clone())
                })
        });
        link_statuses.insert(
            key,
            status.unwrap_or_else(|| TopologyLinkStatus {
                source_endpoint: task.source_endpoint,
                target_endpoint: task.target_endpoint,
                health: TopologyHealth::Unknown,
                latency_ms: None,
                message: "source probe: pending bounded observation batch".to_string(),
                observed_at: String::new(),
            }),
        );
    }
    let links = spec
        .links
        .iter()
        .map(|link| {
            let status = link_statuses
                .remove(&(link.source_endpoint.clone(), link.target_endpoint.clone()))
                .expect("every link receives an observed status");
            if link.enabled && status.health != TopologyHealth::Healthy {
                drift.push(TopologyDrift {
                    resource_kind: TopologyResourceKind::Link,
                    resource_id: format!("{}->{}", link.source_endpoint, link.target_endpoint),
                    kind: TopologyDriftKind::Unreachable,
                    detail: bounded_detail(&status.message),
                });
            }
            status
        })
        .collect::<Vec<_>>();
    (endpoints, links)
}

fn trusted_observation_ms(
    marker: &str,
    message: &str,
    evidence_prefix: &str,
    now: i64,
) -> Option<i64> {
    if !message.starts_with(evidence_prefix) {
        return None;
    }
    let observed = marker.strip_prefix("unix-ms:")?.parse::<i64>().ok()?;
    (observed <= now && now.saturating_sub(observed) <= NETWORK_OBSERVATION_MAX_AGE_MS)
        .then_some(observed)
}

fn network_probe_agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_global(Some(NETWORK_PROBE_TIMEOUT))
        .http_status_as_error(false)
        .max_redirects(0)
        .proxy(None)
        .build()
        .into()
}

enum NetworkProbeWork {
    Endpoint {
        index: usize,
        task: EndpointProbeTask,
        observed_at: String,
        results: mpsc::Sender<NetworkProbeResult>,
    },
    Link {
        index: usize,
        task: LinkProbeTask,
        observed_at: String,
        results: mpsc::Sender<NetworkProbeResult>,
    },
    Shutdown,
}

enum NetworkProbeResult {
    Endpoint(usize, TopologyEndpointStatus),
    Link(usize, TopologyLinkStatus),
}

struct NetworkProbePool {
    work: mpsc::SyncSender<NetworkProbeWork>,
    workers: Vec<JoinHandle<()>>,
}

impl NetworkProbePool {
    fn new() -> Self {
        let (work, receiver) = mpsc::sync_channel(LINK_PROBE_BATCH + ENDPOINT_PROBE_BATCH);
        let receiver = Arc::new(Mutex::new(receiver));
        let mut workers = Vec::with_capacity(NETWORK_PROBE_CONCURRENCY);
        for ordinal in 0..NETWORK_PROBE_CONCURRENCY {
            let receiver = Arc::clone(&receiver);
            workers.push(
                thread::Builder::new()
                    .name(format!("orchestrator-topology-probe-{ordinal:02}"))
                    .spawn(move || {
                        let agent = network_probe_agent();
                        loop {
                            let work = receiver
                                .lock()
                                .expect("network probe queue lock poisoned")
                                .recv();
                            match work {
                                Ok(NetworkProbeWork::Endpoint {
                                    index,
                                    task,
                                    observed_at,
                                    results,
                                }) => {
                                    let _ = results.send(NetworkProbeResult::Endpoint(
                                        index,
                                        probe_endpoint(&agent, &task, &observed_at),
                                    ));
                                }
                                Ok(NetworkProbeWork::Link {
                                    index,
                                    task,
                                    observed_at,
                                    results,
                                }) => {
                                    let _ = results.send(NetworkProbeResult::Link(
                                        index,
                                        probe_link(&agent, &task, &observed_at),
                                    ));
                                }
                                Ok(NetworkProbeWork::Shutdown) | Err(_) => break,
                            }
                        }
                    })
                    .expect("spawn fixed topology network probe worker"),
            );
        }
        Self { work, workers }
    }

    fn probe_endpoints(
        &self,
        tasks: &[EndpointProbeTask],
        observed_at: &str,
    ) -> Vec<TopologyEndpointStatus> {
        let (results, receiver) = mpsc::channel();
        for (index, task) in tasks.iter().cloned().enumerate() {
            self.work
                .send(NetworkProbeWork::Endpoint {
                    index,
                    task,
                    observed_at: observed_at.to_string(),
                    results: results.clone(),
                })
                .expect("fixed topology network probe pool stopped unexpectedly");
        }
        drop(results);
        let mut observed = receiver
            .into_iter()
            .map(|result| match result {
                NetworkProbeResult::Endpoint(index, status) => (index, status),
                NetworkProbeResult::Link(_, _) => {
                    unreachable!("endpoint batch received a link probe result")
                }
            })
            .collect::<Vec<_>>();
        observed.sort_by_key(|(index, _)| *index);
        observed.into_iter().map(|(_, status)| status).collect()
    }

    fn probe_links(&self, tasks: &[LinkProbeTask], observed_at: &str) -> Vec<TopologyLinkStatus> {
        let (results, receiver) = mpsc::channel();
        for (index, task) in tasks.iter().cloned().enumerate() {
            self.work
                .send(NetworkProbeWork::Link {
                    index,
                    task,
                    observed_at: observed_at.to_string(),
                    results: results.clone(),
                })
                .expect("fixed topology network probe pool stopped unexpectedly");
        }
        drop(results);
        let mut observed = receiver
            .into_iter()
            .map(|result| match result {
                NetworkProbeResult::Link(index, status) => (index, status),
                NetworkProbeResult::Endpoint(_, _) => {
                    unreachable!("link batch received an endpoint probe result")
                }
            })
            .collect::<Vec<_>>();
        observed.sort_by_key(|(index, _)| *index);
        observed.into_iter().map(|(_, status)| status).collect()
    }
}

impl Drop for NetworkProbePool {
    fn drop(&mut self) {
        for _ in 0..self.workers.len() {
            let _ = self.work.send(NetworkProbeWork::Shutdown);
        }
        for worker in self.workers.drain(..) {
            let _ = worker.join();
        }
    }
}

fn probe_endpoint(
    agent: &ureq::Agent,
    task: &EndpointProbeTask,
    observed_at: &str,
) -> TopologyEndpointStatus {
    let started = std::time::Instant::now();
    match endpoint_health_url(&task.endpoint, &task.protocol, &task.health_path)
        .and_then(|url| bounded_http_get(agent, &url).map(|_| url))
    {
        Ok(url) => TopologyEndpointStatus {
            endpoint: task.endpoint.clone(),
            health: TopologyHealth::Healthy,
            reachable: true,
            latency_ms: Some(elapsed_ms(started)),
            message: bounded_detail(&format!(
                "{ENDPOINT_EVIDENCE_PREFIX} {} {} returned HTTP 2xx for service {}",
                task.protocol, url, task.service_id
            )),
            observed_at: observed_at.to_string(),
        },
        Err(error) => TopologyEndpointStatus {
            endpoint: task.endpoint.clone(),
            health: TopologyHealth::Unhealthy,
            reachable: false,
            latency_ms: Some(elapsed_ms(started)),
            message: bounded_detail(&format!("{ENDPOINT_EVIDENCE_PREFIX} {error}")),
            observed_at: observed_at.to_string(),
        },
    }
}

fn probe_link(agent: &ureq::Agent, task: &LinkProbeTask, observed_at: &str) -> TopologyLinkStatus {
    let started = std::time::Instant::now();
    let result = link_probe_url(
        &task.source_endpoint,
        &task.source_protocol,
        &task.target_endpoint,
    )
    .and_then(|url| bounded_http_get(agent, &url))
    .and_then(|body| validate_link_probe_body(task, &body));
    match result {
        Ok(()) => TopologyLinkStatus {
            source_endpoint: task.source_endpoint.clone(),
            target_endpoint: task.target_endpoint.clone(),
            health: TopologyHealth::Healthy,
            latency_ms: Some(elapsed_ms(started)),
            message: format!(
                "{LINK_EVIDENCE_PREFIX} source {} reached exact target {}",
                task.source_service_id, task.target_endpoint
            ),
            observed_at: observed_at.to_string(),
        },
        Err(error) => TopologyLinkStatus {
            source_endpoint: task.source_endpoint.clone(),
            target_endpoint: task.target_endpoint.clone(),
            health: TopologyHealth::Unhealthy,
            latency_ms: Some(elapsed_ms(started)),
            message: bounded_detail(&format!("{LINK_EVIDENCE_PREFIX} {error}")),
            observed_at: observed_at.to_string(),
        },
    }
}

fn endpoint_health_url(endpoint: &str, protocol: &str, path: &str) -> Result<String, String> {
    endpoint_url(endpoint, protocol, path, None)
}

fn link_probe_url(source: &str, protocol: &str, target: &str) -> Result<String, String> {
    validate_endpoint_id(target).map_err(|error| format!("invalid target endpoint: {error}"))?;
    let query = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("target", target)
        .finish();
    endpoint_url(source, protocol, "/probe", Some(&query))
}

fn endpoint_url(
    endpoint: &str,
    protocol: &str,
    path: &str,
    query: Option<&str>,
) -> Result<String, String> {
    validate_endpoint_id(endpoint).map_err(|error| error.to_string())?;
    if !matches!(protocol, "http" | "https") {
        return Err(format!(
            "protocol {protocol} does not expose the v1 HTTP network probe contract"
        ));
    }
    if !path.starts_with('/') || path.contains('#') {
        return Err("health/probe path must be an absolute path without a fragment".to_string());
    }
    let identity = parse_endpoint_id(endpoint).map_err(|error| error.to_string())?;
    let host = if identity.host.contains(':') {
        format!("[{}]", identity.host)
    } else {
        identity.host.to_string()
    };
    let mut url = url::Url::parse(&format!("{protocol}://{host}:{}", identity.port))
        .map_err(|error| format!("construct endpoint URL: {error}"))?;
    url.set_path(path);
    url.set_query(query);
    Ok(url.to_string())
}

fn bounded_http_get(agent: &ureq::Agent, url: &str) -> Result<Vec<u8>, String> {
    let response = agent
        .get(url)
        .header("accept", "application/json")
        .call()
        .map_err(|error| format!("GET {url} failed: {error}"))?;
    let status = response.status().as_u16();
    let mut body = Vec::new();
    response
        .into_body()
        .into_reader()
        .take(NETWORK_RESPONSE_LIMIT as u64 + 1)
        .read_to_end(&mut body)
        .map_err(|error| format!("GET {url} response read failed: {error}"))?;
    if body.len() > NETWORK_RESPONSE_LIMIT {
        return Err(format!(
            "GET {url} response exceeded {NETWORK_RESPONSE_LIMIT} bytes"
        ));
    }
    if !(200..=299).contains(&status) {
        return Err(format!("GET {url} returned HTTP {status}"));
    }
    Ok(body)
}

fn validate_link_probe_body(task: &LinkProbeTask, body: &[u8]) -> Result<(), String> {
    let value: Value =
        serde_json::from_slice(body).map_err(|error| format!("decode /probe JSON: {error}"))?;
    let expected = [
        ("status", "healthy"),
        ("source_service_id", task.source_service_id.as_str()),
        ("target_endpoint", task.target_endpoint.as_str()),
        ("target_service_id", task.target_service_id.as_str()),
    ];
    if expected
        .iter()
        .any(|(key, expected)| value.get(key).and_then(Value::as_str) != Some(*expected))
    {
        return Err("/probe response does not prove the exact source-to-target path".to_string());
    }
    Ok(())
}

fn elapsed_ms(started: std::time::Instant) -> u64 {
    u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)
}

fn desired_deployment_state(state: &RuntimeDesiredState) -> TopologyDesiredDeploymentState {
    match state {
        RuntimeDesiredState::Running => TopologyDesiredDeploymentState::Running,
        RuntimeDesiredState::Stopped => TopologyDesiredDeploymentState::Stopped,
        RuntimeDesiredState::Removed => TopologyDesiredDeploymentState::Absent,
    }
}

fn observed_deployment_state(state: &RuntimeObservedState) -> TopologyObservedDeploymentState {
    match state {
        RuntimeObservedState::Created => TopologyObservedDeploymentState::Pending,
        RuntimeObservedState::Running => TopologyObservedDeploymentState::Running,
        RuntimeObservedState::Stopped => TopologyObservedDeploymentState::Stopped,
        RuntimeObservedState::Exited => TopologyObservedDeploymentState::Failed,
        RuntimeObservedState::Missing | RuntimeObservedState::Unknown => {
            TopologyObservedDeploymentState::Unknown
        }
    }
}

fn runtime_health(value: &str) -> TopologyHealth {
    if value.eq_ignore_ascii_case("healthy") {
        TopologyHealth::Healthy
    } else if value.eq_ignore_ascii_case("unhealthy") {
        TopologyHealth::Unhealthy
    } else {
        TopologyHealth::Unknown
    }
}

fn runtime_states_match(desired: &RuntimeDesiredState, observed: &RuntimeObservedState) -> bool {
    matches!(
        (desired, observed),
        (RuntimeDesiredState::Running, RuntimeObservedState::Running)
            | (RuntimeDesiredState::Stopped, RuntimeObservedState::Stopped)
            | (RuntimeDesiredState::Removed, RuntimeObservedState::Missing)
    )
}

fn topology_binding_consumers_healthy(
    storage: &DurableStore,
    bindings: &[ApiBinding],
) -> Result<(), String> {
    let deployments = bindings
        .iter()
        .filter(|binding| binding.desired_state == "ACTIVE")
        .map(|binding| binding.consumer_deployment_id.as_str())
        .collect::<BTreeSet<_>>();
    topology_binding_role_healthy(storage, deployments, "consumer")
}

fn topology_binding_providers_healthy(
    storage: &DurableStore,
    bindings: &[ApiBinding],
) -> Result<(), String> {
    let deployments = bindings
        .iter()
        .filter(|binding| binding.desired_state == "ACTIVE")
        .map(|binding| binding.provider_deployment_id.as_str())
        .collect::<BTreeSet<_>>();
    topology_binding_role_healthy(storage, deployments, "provider")
}

fn topology_binding_role_healthy(
    storage: &DurableStore,
    deployments: BTreeSet<&str>,
    role: &str,
) -> Result<(), String> {
    if deployments.is_empty() {
        return Ok(());
    }
    let runtimes = storage
        .runtime_instances(None)
        .map_err(|error| error.to_string())?;
    let evidence_at_ms = now_ms();
    for deployment_id in deployments {
        let matching = runtimes
            .iter()
            .filter(|runtime| runtime.instance.deployment_id == deployment_id)
            .collect::<Vec<_>>();
        let [runtime] = matching.as_slice() else {
            return Err(format!(
                "{role} deployment {deployment_id} no longer has one exact runtime projection"
            ));
        };
        let runtime = storage
            .runtime_with_current_evidence((*runtime).clone(), evidence_at_ms)
            .map_err(|error| error.to_string())?;
        if runtime.instance.desired_state != RuntimeDesiredState::Running
            || runtime.instance.observed_state != RuntimeObservedState::Running
            || !runtime.instance.health.eq_ignore_ascii_case("HEALTHY")
            || !runtime.drift_reason.trim().is_empty()
            || (runtime.management_mode == RuntimeManagementMode::Managed
                && !runtime.instance.runtime_attested)
        {
            let evidence = if runtime.drift_reason.trim().is_empty() {
                "runtime is not desired Running, observed Running/Healthy".to_string()
            } else {
                runtime.drift_reason
            };
            return Err(format!(
                "{role} deployment {deployment_id} is unavailable: {evidence}"
            ));
        }
    }
    Ok(())
}

fn validate_prepared_bindings(
    bindings: &[ApiBinding],
    topology_id: &str,
    revision_id: &str,
    operation_id: &str,
) -> Result<(), String> {
    let mut generations = BTreeMap::<&str, u64>::new();
    let mut requirements = BTreeSet::new();
    for binding in bindings {
        binding.validate().map_err(|error| error.to_string())?;
        if binding.topology_id != topology_id
            || binding.topology_revision_id != revision_id
            || binding.last_operation_id != operation_id
            || binding.state != ApiBindingState::Pending
            || !matches!(binding.desired_state.as_str(), "ACTIVE" | "REVOKED")
        {
            return Err(format!(
                "prepared binding {} does not belong to the applying revision/operation or is not PENDING",
                binding.binding_id
            ));
        }
        if binding.credential_generation != binding.context_generation {
            return Err(format!(
                "prepared binding {} has split credential/context generations",
                binding.binding_id
            ));
        }
        let generation = generations
            .entry(binding.consumer_deployment_id.as_str())
            .or_insert(binding.credential_generation);
        if *generation != binding.credential_generation {
            return Err(format!(
                "consumer {} bindings do not share one deployment-wide generation",
                binding.consumer_deployment_id
            ));
        }
        if !requirements.insert((
            binding.consumer_deployment_id.as_str(),
            binding.requirement_name.as_str(),
        )) {
            return Err(format!(
                "consumer {} requirement {} is repeated",
                binding.consumer_deployment_id, binding.requirement_name
            ));
        }
    }
    Ok(())
}

fn activate_staged_bindings(mut bindings: Vec<ApiBinding>, observed_at: &str) -> Vec<ApiBinding> {
    for binding in &mut bindings {
        if binding.desired_state == "ACTIVE" {
            binding.state = ApiBindingState::Active;
            binding.observed_state = ApiBindingObservedState::Active;
            binding.health = ApiBindingHealth::Healthy;
            binding.reason.clear();
        } else {
            binding.state = ApiBindingState::Revoked;
            binding.observed_state = ApiBindingObservedState::Revoked;
            binding.health = ApiBindingHealth::Unknown;
            binding.reason = "removed or disabled by applied Topology revision".to_string();
        }
        binding.updated_at = observed_at.to_string();
    }
    bindings
}

fn normalize_group_binding_moves(members: &mut [orchestrator_storage::TopologyApplyGroupMember]) {
    let mut owners = BTreeMap::<(String, String), Vec<(String, ApiBindingState)>>::new();
    for member in members.iter() {
        for binding in &member.active_bindings {
            owners
                .entry((
                    binding.consumer_deployment_id.clone(),
                    binding.requirement_name.clone(),
                ))
                .or_default()
                .push((member.topology_id.clone(), binding.state));
        }
    }
    let moved_requirements = owners
        .into_iter()
        .filter_map(|(requirement, owners)| {
            let is_one_owner_move = owners.len() == 2
                && owners[0].0 != owners[1].0
                && owners
                    .iter()
                    .filter(|(_, state)| *state == ApiBindingState::Active)
                    .count()
                    == 1
                && owners
                    .iter()
                    .filter(|(_, state)| *state == ApiBindingState::Revoked)
                    .count()
                    == 1;
            is_one_owner_move.then_some(requirement)
        })
        .collect::<BTreeSet<_>>();
    for member in members {
        member.active_bindings.retain(|binding| {
            binding.state != ApiBindingState::Revoked
                || !moved_requirements.contains(&(
                    binding.consumer_deployment_id.clone(),
                    binding.requirement_name.clone(),
                ))
        });
    }
}

fn bounded_detail(detail: &str) -> String {
    const MAX_DETAIL_BYTES: usize = 512;
    let mut bounded = String::with_capacity(detail.len().min(MAX_DETAIL_BYTES));
    for character in detail.chars().filter(|character| !character.is_control()) {
        if bounded.len() + character.len_utf8() > MAX_DETAIL_BYTES {
            break;
        }
        bounded.push(character);
    }
    bounded
}

fn topology_expired_success_plan(
    storage: &DurableStore,
    job: &Job,
    payload: &TopologyApplyPayload,
) -> Result<Option<(Vec<orchestrator_storage::TopologyApplyGroupMember>, Value)>, String> {
    let mut identities = match payload.phase {
        TopologyApplyPhase::FinalizeGroup => payload
            .group
            .iter()
            .map(|member| (member.topology_id.clone(), member.revision_id.clone()))
            .collect::<Vec<_>>(),
        TopologyApplyPhase::Finalize | TopologyApplyPhase::Full => {
            vec![(payload.topology_id.clone(), payload.revision_id.clone())]
        }
        TopologyApplyPhase::Stage | TopologyApplyPhase::Prepare | TopologyApplyPhase::Abort => {
            return Ok(None);
        }
    };
    identities.sort();
    if identities.is_empty()
        || identities.iter().any(|(topology_id, revision_id)| {
            topology_id.trim().is_empty() || revision_id.trim().is_empty()
        })
        || identities.windows(2).any(|pair| pair[0].0 == pair[1].0)
    {
        return Err(format!(
            "expired topology Job {} has invalid success-evidence identities",
            job.job_id
        ));
    }

    let mut binding_counts = Vec::with_capacity(identities.len());
    let members = identities
        .iter()
        .map(|(topology_id, revision_id)| {
            let binding_count = storage
                .api_bindings_for_topology(topology_id)
                .map_err(|error| error.to_string())?
                .len();
            binding_counts.push((topology_id.clone(), revision_id.clone(), binding_count));
            Ok(orchestrator_storage::TopologyApplyGroupMember {
                topology_id: topology_id.clone(),
                revision_id: revision_id.clone(),
                active_bindings: Vec::new(),
            })
        })
        .collect::<Result<Vec<_>, String>>()?;
    let result = match payload.phase {
        TopologyApplyPhase::FinalizeGroup => serde_json::json!({
            "phase": "FINALIZE_GROUP",
            "topologies": binding_counts
                .into_iter()
                .map(|(topology_id, revision_id, bindings)| serde_json::json!({
                    "topology_id": topology_id,
                    "revision_id": revision_id,
                    "bindings": bindings,
                }))
                .collect::<Vec<_>>(),
        }),
        TopologyApplyPhase::Finalize => serde_json::json!({
            "phase": "FINALIZE",
            "topology_id": payload.topology_id,
            "revision_id": payload.revision_id,
            "bindings": binding_counts.first().map(|member| member.2).unwrap_or(0),
        }),
        TopologyApplyPhase::Full => serde_json::json!({
            "phase": "FULL",
            "topology_id": payload.topology_id,
            "revision_id": payload.revision_id,
            "recovered_from_durable_head": true,
        }),
        TopologyApplyPhase::Stage | TopologyApplyPhase::Prepare | TopologyApplyPhase::Abort => {
            unreachable!("non-final phases returned before building recovery evidence")
        }
    };
    Ok(Some((members, result)))
}

fn recover_unknown_topology_payload(
    storage: &DurableStore,
    payload: TopologyApplyPayload,
    operation_id: &str,
) -> Result<(), String> {
    if payload.phase == TopologyApplyPhase::FinalizeGroup {
        if payload.group.is_empty() {
            return Err("expired FINALIZE_GROUP payload has no members".to_string());
        }
        let detail = "control-plane worker lease expired with an unproven grouped provider outcome";
        let mut failures = Vec::new();
        for member in payload.group {
            if let Err(error) = recover_unknown_topology_apply(
                storage,
                &member.topology_id,
                &member.revision_id,
                operation_id,
                detail,
            ) {
                failures.push(format!("{}: {error}", member.topology_id));
                continue;
            }
            // A mixed group is never a successful atomic generation. Members
            // that were already visible must therefore remain visible but be
            // marked Degraded alongside members whose applying head was
            // released above.
            if let Err(error) = mark_degraded(storage, &member.topology_id, operation_id, detail) {
                failures.push(format!("{}: {error}", member.topology_id));
            }
        }
        if !failures.is_empty() {
            return Err(format!(
                "{} grouped topology recovery member(s) failed: {}",
                failures.len(),
                failures.join("; ")
            ));
        }
        return Ok(());
    }
    recover_unknown_topology_apply(
        storage,
        &payload.topology_id,
        &payload.revision_id,
        operation_id,
        "control-plane worker lease expired with an unproven provider outcome",
    )
}

fn recover_expired(storage: &DurableStore, now_ms: i64) -> Result<(), String> {
    let jobs = storage.job_store();
    let expired = jobs
        .expired_leases(now_ms)
        .map_err(|error| error.to_string())?;
    drop(jobs);

    for job in &expired {
        if !matches!(
            job.kind,
            JobKind::TopologyApply | JobKind::ContributionProjection
        ) {
            continue;
        }
        if contribution_controller::is_contribution_job(job) {
            match contribution_controller::recover_expired_contribution_job(storage, job) {
                Ok(Some(result)) => {
                    let mut jobs = storage.job_store();
                    jobs.resolve_expired_success(ResolveExpiredSuccessRequest {
                        job_id: job.job_id.clone(),
                        now_ms,
                        result,
                    })
                    .map_err(|error| {
                        format!(
                            "resolve expired contribution Job {} from durable evidence: {error}",
                            job.job_id
                        )
                    })?;
                }
                Ok(None) => {
                    // No side-effect outcome can be proved. JobStore recovery
                    // will move this non-retry-safe lease to NEEDS_ATTENTION;
                    // the durable activation/receipts remain the repair source.
                }
                Err(error) => {
                    eprintln!(
                        "expired contribution Job {} could not be reconciled: {error}",
                        job.job_id
                    );
                }
            }
            continue;
        }
        let payload = serde_json::from_value::<TopologyApplyPayload>(job.payload.clone()).map_err(
            |error| {
                format!(
                    "expired topology Job {} has an invalid recovery payload: {error}",
                    job.job_id
                )
            },
        )?;
        let resolved = match topology_expired_success_plan(storage, job, &payload)? {
            Some((members, result)) => storage
                .resolve_expired_topology_apply_group_success(
                    &members,
                    &job.operation_id,
                    &job.job_id,
                    now_ms,
                    result,
                )
                .map_err(|error| error.to_string())?
                .is_some(),
            None => false,
        };
        if !resolved {
            recover_unknown_topology_payload(storage, payload, &job.operation_id)?;
        }
    }

    let mut jobs = storage.job_store();
    let mut operations = storage.operation_store();
    OperationCoordinator::new(&mut operations, &mut jobs)
        .recover(now_ms)
        .map_err(|error| error.to_string())?;
    Ok(())
}

/// Repairs the intentional transaction boundary between durable Job completion
/// and its Operation projection. A process crash or a transient persistence
/// failure after `JobStore::complete` must not leave a terminal Job represented
/// forever by a stale RUNNING/LEASED Operation snapshot.
fn repair_recoverable_operation_projections(
    storage: &DurableStore,
    now_ms: i64,
) -> Result<(), String> {
    let recoverable = storage
        .operation_store()
        .recoverable()
        .map_err(|error| error.to_string())?;
    let mut failures = Vec::new();
    for operation in recoverable {
        let should_auto_enqueue = operation.status == DurableOperationStatus::Confirmed
            && operation
                .request
                .get("auto_enqueue")
                .and_then(Value::as_bool)
                == Some(true);
        if operation.status == DurableOperationStatus::Confirmed && !should_auto_enqueue {
            continue;
        }
        let mut operations = storage.operation_store();
        let mut jobs = storage.job_store();
        let mut coordinator = OperationCoordinator::new(&mut operations, &mut jobs);
        let repaired = match operation.status {
            DurableOperationStatus::Confirmed | DurableOperationStatus::Enqueuing => {
                coordinator.enqueue(&operation.operation_id, now_ms)
            }
            DurableOperationStatus::Running => coordinator.project(&operation.operation_id, now_ms),
            DurableOperationStatus::Cancelling => {
                coordinator.cancel(&operation.operation_id, now_ms)
            }
            _ => continue,
        };
        if let Err(error) = repaired {
            failures.push(format!("{}: {error}", operation.operation_id));
        }
    }
    if failures.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "{} recoverable Operation projection(s) failed: {}",
            failures.len(),
            failures.join("; ")
        ))
    }
}

fn recover_terminal_topology_applies(storage: &DurableStore) -> Result<(), String> {
    for heads in storage
        .list_topology_heads()
        .map_err(|error| error.to_string())?
    {
        let (Some(revision_id), Some(operation_id)) = (
            heads.applying_revision_id.as_deref(),
            heads.applying_operation_id.as_deref(),
        ) else {
            continue;
        };
        let operation = storage
            .operation_store()
            .get(operation_id)
            .map_err(|error| error.to_string())?;
        let (outcome, degraded_detail) = match operation.map(|operation| operation.status) {
            Some(DurableOperationStatus::Cancelled | DurableOperationStatus::Failed) => {
                (TopologyApplyOutcome::Failed, None)
            }
            Some(DurableOperationStatus::NeedsAttention) => (
                TopologyApplyOutcome::Degraded,
                Some("topology apply operation requires explicit reconciliation"),
            ),
            Some(DurableOperationStatus::Succeeded) => (TopologyApplyOutcome::Succeeded, None),
            Some(
                DurableOperationStatus::Planned
                | DurableOperationStatus::Confirmed
                | DurableOperationStatus::Enqueuing
                | DurableOperationStatus::Running
                | DurableOperationStatus::Cancelling
                | DurableOperationStatus::RolledBack,
            ) => continue,
            None => (
                TopologyApplyOutcome::Degraded,
                Some("topology apply ownership references a missing Operation"),
            ),
        };
        storage
            .finish_topology_apply(
                &heads.topology_id,
                revision_id,
                operation_id,
                outcome,
                &now_marker(),
            )
            .map_err(|error| error.to_string())?;
        if let Some(detail) = degraded_detail {
            mark_degraded(storage, &heads.topology_id, operation_id, detail)?;
        }
    }
    Ok(())
}

/// Releases durable apply ownership after an outcome becomes unknowable.
///
/// A crashed control-plane must never blindly replay provider mutations, but
/// leaving `applying_revision_id` set would also permanently prevent drafts
/// and make the reconciler skip the topology.  Completing the apply as
/// `Degraded` keeps the last proven applied head, records the attempted
/// revision as desired state, and lets fresh provider observations drive the
/// explicit operator reconciliation that follows `NEEDS_ATTENTION`.
fn finish_unknown_topology_apply(
    storage: &DurableStore,
    topology_id: &str,
    revision_id: &str,
    operation_id: &str,
    detail: &str,
) -> Result<(), String> {
    storage
        .finish_topology_apply(
            topology_id,
            revision_id,
            operation_id,
            TopologyApplyOutcome::Degraded,
            &now_marker(),
        )
        .map_err(|error| {
            format!("unknown topology apply outcome could not release durable ownership: {error}")
        })?;
    mark_degraded(storage, topology_id, operation_id, detail)
}

fn recover_unknown_topology_apply(
    storage: &DurableStore,
    topology_id: &str,
    revision_id: &str,
    operation_id: &str,
    detail: &str,
) -> Result<(), String> {
    let heads = storage
        .topology_heads(topology_id)
        .map_err(|error| error.to_string())?
        .ok_or_else(|| format!("topology {topology_id} disappeared during recovery"))?;
    if heads.applying_revision_id.as_deref() == Some(revision_id)
        && heads.applying_operation_id.as_deref() == Some(operation_id)
    {
        return finish_unknown_topology_apply(
            storage,
            topology_id,
            revision_id,
            operation_id,
            detail,
        );
    }
    if heads.applied_revision_id.as_deref() == Some(revision_id)
        && heads.last_operation_id.as_deref() == Some(operation_id)
    {
        // The provider acknowledgement and applied-head commit completed
        // before the worker crashed.  That durable commit is proof of the
        // topology result, so do not downgrade or replay the provider state.
        return Ok(());
    }
    mark_degraded(storage, topology_id, operation_id, detail)
}

fn mark_degraded(
    storage: &DurableStore,
    topology_id: &str,
    operation_id: &str,
    detail: &str,
) -> Result<(), String> {
    let mut status = storage
        .topology_status(topology_id)
        .map_err(|error| error.to_string())?
        .ok_or_else(|| format!("topology {topology_id} has no status"))?;
    status.state = TopologyReconciliationState::Degraded;
    status.last_operation_id = Some(operation_id.to_string());
    status.updated_at = now_marker();
    status.drift = vec![TopologyDrift {
        resource_kind: TopologyResourceKind::Authority,
        resource_id: topology_id.to_string(),
        kind: TopologyDriftKind::Unreachable,
        detail: detail.to_string(),
    }];
    storage
        .put_topology_status(&status)
        .map_err(|error| error.to_string())
}

pub(crate) fn process_one(
    storage: &DurableStore,
    provider: Option<&TopologyProviderSaga>,
) -> Result<bool, String> {
    let now = now_ms();
    let mut jobs = storage.job_store();
    let Some(job) = jobs
        .claim(ClaimRequest {
            node_id: CONTROL_PLANE_NODE_ID.to_string(),
            instance_id: "single-active-control-plane".to_string(),
            lease_token: lease_token()?,
            now_ms: now,
            lease_ms: DEFAULT_LEASE_MS,
        })
        .map_err(|error| error.to_string())?
    else {
        return Ok(false);
    };
    let lease_token = job
        .lease_token
        .clone()
        .ok_or_else(|| "claimed topology job has no lease token".to_string())?;
    let lease_heartbeat = ControlPlaneLeaseHeartbeat::start(
        storage.clone(),
        job.job_id.clone(),
        lease_token.clone(),
        job.lease_expires_at_ms
            .ok_or_else(|| "claimed topology job has no lease expiry".to_string())?,
    )?;
    lease_heartbeat.checkpoint(&mut jobs)?;
    if contribution_controller::is_contribution_job(&job) {
        let outcome = contribution_controller::execute_contribution_job(
            storage,
            &job.payload,
            &job.operation_id,
            || lease_heartbeat.checkpoint(&mut jobs),
        );
        match outcome {
            Ok(outcome) => complete_and_project(
                storage,
                &mut jobs,
                &job.job_id,
                &job.operation_id,
                lease_token,
                CompletionStatus::Succeeded,
                outcome.result,
                String::new(),
            )?,
            Err(error) => {
                let status = if error.retryable() {
                    if error.retry_exhaustion_needs_attention() && job.attempt >= job.max_attempts {
                        CompletionStatus::NeedsAttention
                    } else {
                        CompletionStatus::RetryableFailure
                    }
                } else if error.needs_attention() {
                    CompletionStatus::NeedsAttention
                } else {
                    CompletionStatus::Failed
                };
                complete_and_project(
                    storage,
                    &mut jobs,
                    &job.job_id,
                    &job.operation_id,
                    lease_token,
                    status,
                    serde_json::json!({"code": error.code()}),
                    error.to_string(),
                )?;
            }
        }
        return Ok(true);
    }
    if matches!(job.kind, JobKind::NodeDrain | JobKind::NodeRemove) {
        let outcome = process_node_lifecycle(storage, &job.kind, &job.payload);
        match outcome {
            Ok(result) => complete_and_project(
                storage,
                &mut jobs,
                &job.job_id,
                &job.operation_id,
                lease_token,
                CompletionStatus::Succeeded,
                result,
                String::new(),
            )?,
            Err(failure) => complete_and_project(
                storage,
                &mut jobs,
                &job.job_id,
                &job.operation_id,
                lease_token,
                CompletionStatus::Failed,
                serde_json::json!({"code": failure.code}),
                failure.detail,
            )?,
        }
        return Ok(true);
    }
    if job.kind == JobKind::ExternalHealth {
        match process_external_health(storage, &job.payload) {
            Ok(result) => complete_and_project(
                storage,
                &mut jobs,
                &job.job_id,
                &job.operation_id,
                lease_token,
                CompletionStatus::Succeeded,
                result,
                String::new(),
            )?,
            Err(failure) => complete_and_project(
                storage,
                &mut jobs,
                &job.job_id,
                &job.operation_id,
                lease_token,
                failure.status,
                serde_json::json!({"code": failure.code}),
                failure.detail,
            )?,
        }
        return Ok(true);
    }
    if job.kind != JobKind::TopologyApply {
        complete_and_project(
            storage,
            &mut jobs,
            &job.job_id,
            &job.operation_id,
            lease_token,
            CompletionStatus::NeedsAttention,
            Value::Null,
            format!(
                "control-plane queue received unsupported job kind {:?}",
                job.kind
            ),
        )?;
        return Ok(true);
    }
    let Some(provider) = provider else {
        complete_and_project(
            storage,
            &mut jobs,
            &job.job_id,
            &job.operation_id,
            lease_token,
            CompletionStatus::NeedsAttention,
            Value::Null,
            "Topology providers are unavailable after the apply job was durably accepted"
                .to_string(),
        )?;
        if let Ok(payload) = serde_json::from_value::<TopologyApplyPayload>(job.payload.clone())
            && payload.phase != TopologyApplyPhase::FinalizeGroup
        {
            finish_unknown_topology_apply(
                storage,
                &payload.topology_id,
                &payload.revision_id,
                &job.operation_id,
                "Topology providers are unavailable after the apply job was durably accepted",
            )?;
        }
        return Ok(true);
    };

    let payload: TopologyApplyPayload = serde_json::from_value(job.payload.clone())
        .map_err(|error| format!("invalid topology apply payload: {error}"))?;
    if payload.phase == TopologyApplyPhase::FinalizeGroup {
        return finalize_topology_group(
            storage,
            provider,
            &lease_heartbeat,
            &mut jobs,
            &job.job_id,
            &job.operation_id,
            lease_token,
            &payload.group,
        );
    }
    let mut heads = storage
        .topology_heads(&payload.topology_id)
        .map_err(|error| error.to_string())?
        .ok_or_else(|| format!("topology {} disappeared", payload.topology_id))?;
    if matches!(
        payload.phase,
        TopologyApplyPhase::Full | TopologyApplyPhase::Prepare
    ) && heads.applying_revision_id.is_none()
        && heads.draft_revision_id == payload.revision_id
        && heads.applied_revision_id.as_deref() != Some(payload.revision_id.as_str())
    {
        // A compensated FAILED apply clears ownership. A generic Operation
        // retry creates a fresh durable job for the same revision, so it must
        // reacquire the topology CAS before any provider I/O.
        storage
            .begin_topology_apply(
                &payload.topology_id,
                &payload.revision_id,
                &job.operation_id,
                &now_marker(),
            )
            .map_err(|error| format!("retry could not reacquire topology apply: {error}"))?;
        heads = storage
            .topology_heads(&payload.topology_id)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| format!("topology {} disappeared", payload.topology_id))?;
    }
    let aborting_completed_group_member = payload.phase == TopologyApplyPhase::Abort
        && heads.applying_revision_id.is_none()
        && heads.applied_revision_id.as_deref() == Some(payload.revision_id.as_str())
        && heads.last_operation_id.as_deref() == Some(job.operation_id.as_str());
    if (heads.applying_revision_id.as_deref() != Some(payload.revision_id.as_str())
        || heads.applying_operation_id.as_deref() != Some(job.operation_id.as_str()))
        && !aborting_completed_group_member
    {
        let compensated_failure_kept_retryable_draft = heads.draft_revision_id
            == payload.revision_id
            && heads.applied_revision_id.as_deref() != Some(payload.revision_id.as_str());
        let compensated_abort_restored_previous_draft = heads
            .applied_revision_id
            .as_ref()
            .is_some_and(|applied| heads.draft_revision_id == *applied)
            && heads.applied_revision_id.as_deref() != Some(payload.revision_id.as_str());
        if payload.phase == TopologyApplyPhase::Abort
            && heads.applying_revision_id.is_none()
            && heads.last_operation_id.as_deref() == Some(job.operation_id.as_str())
            && (compensated_failure_kept_retryable_draft
                || compensated_abort_restored_previous_draft)
        {
            // A FAILED forward phase releases topology ownership only after
            // it has proved provider and binding compensation. It deliberately
            // leaves the candidate as the draft so an explicit retry can
            // reacquire the same immutable revision. A completed ABORT instead
            // restores draft and applied to the previous revision. Both are
            // durable, writer-fenced terminal facts, so replaying the planned
            // ABORT must be a no-op rather than manufacturing NEEDS_ATTENTION.
            complete_and_project(
                storage,
                &mut jobs,
                &job.job_id,
                &job.operation_id,
                lease_token,
                CompletionStatus::Succeeded,
                serde_json::json!({
                    "phase": "ABORT",
                    "restored": true,
                    "replayed": true,
                    "retryable_draft": compensated_failure_kept_retryable_draft,
                }),
                String::new(),
            )?;
            return Ok(true);
        }
        complete_and_project(
            storage,
            &mut jobs,
            &job.job_id,
            &job.operation_id,
            lease_token,
            CompletionStatus::NeedsAttention,
            Value::Null,
            "topology apply ownership no longer matches the durable head".to_string(),
        )?;
        return Ok(true);
    }
    let revision = storage
        .topology_revision(&payload.topology_id, &payload.revision_id)
        .map_err(|error| error.to_string())?
        .ok_or_else(|| format!("topology revision {} disappeared", payload.revision_id))?;
    let previous_revision_id = if aborting_completed_group_member {
        revision.parent_revision_id()
    } else {
        heads.applied_revision_id.as_deref()
    };
    let previous = previous_revision_id
        .map(|revision_id| {
            storage
                .topology_revision(&payload.topology_id, revision_id)
                .map_err(|error| error.to_string())?
                .ok_or_else(|| format!("applied topology revision {revision_id} disappeared"))
        })
        .transpose()?;
    if payload.phase == TopologyApplyPhase::Abort {
        lease_heartbeat.checkpoint(&mut jobs)?;
        let provider_compensation = provider.compensate_applied_revision(
            &payload.topology_id,
            &payload.revision_id,
            previous.as_ref().map(|revision| revision.revision_id()),
            previous.as_ref().map(|revision| revision.spec()),
            &payload.previous_bindings,
            &job.operation_id,
        );
        lease_heartbeat.checkpoint(&mut jobs)?;
        let binding_compensation =
            storage.replace_topology_api_bindings(&payload.topology_id, &payload.previous_bindings);
        let degraded = provider_compensation.is_err() || binding_compensation.is_err();
        let finish = if degraded {
            Err(
                "provider or binding restoration failed; durable candidate ownership was retained"
                    .to_string(),
            )
        } else if aborting_completed_group_member {
            previous_revision_id
                .ok_or_else(|| {
                    "group compensation cannot rewind an initial topology revision".to_string()
                })
                .and_then(|previous_revision_id| {
                    storage
                        .compensate_completed_topology_apply(
                            &payload.topology_id,
                            &payload.revision_id,
                            previous_revision_id,
                            &job.operation_id,
                            &now_marker(),
                        )
                        .map_err(|error| error.to_string())
                })
        } else {
            previous_revision_id
                .ok_or_else(|| {
                    "an initial topology revision has no safe previous draft to restore".to_string()
                })
                .and_then(|previous_revision_id| {
                    storage
                        .complete_compensated_topology_abort(
                            &payload.topology_id,
                            &payload.revision_id,
                            previous_revision_id,
                            &job.operation_id,
                            &now_marker(),
                        )
                        .map_err(|error| error.to_string())
                })
        };
        let finish_error = finish.err();
        let needs_attention = degraded || finish_error.is_some();
        let detail = format!(
            "topology abort provider restore: {}; binding restore: {}; head release: {}",
            provider_compensation
                .err()
                .unwrap_or_else(|| "succeeded".to_string()),
            binding_compensation
                .err()
                .map(|error| error.to_string())
                .unwrap_or_else(|| "succeeded".to_string()),
            finish_error
                .map(|error| error.to_string())
                .unwrap_or_else(|| "succeeded".to_string()),
        );
        complete_and_project(
            storage,
            &mut jobs,
            &job.job_id,
            &job.operation_id,
            lease_token,
            if needs_attention {
                CompletionStatus::NeedsAttention
            } else {
                CompletionStatus::Succeeded
            },
            serde_json::json!({
                "phase": "ABORT",
                "restored": !needs_attention,
            }),
            if needs_attention {
                detail
            } else {
                String::new()
            },
        )?;
        return Ok(true);
    }
    if payload.phase == TopologyApplyPhase::Finalize {
        let staged_bindings = storage
            .api_bindings_for_topology(&payload.topology_id)
            .map_err(|error| error.to_string())?;
        let health = topology_binding_providers_healthy(storage, &staged_bindings)
            .and_then(|()| topology_binding_consumers_healthy(storage, &staged_bindings));
        if health.is_ok() {
            let active_bindings = activate_staged_bindings(staged_bindings, &now_marker());
            if let Err(activation_error) =
                storage.replace_topology_api_bindings(&payload.topology_id, &active_bindings)
            {
                let provider_compensation = provider.compensate_applied_revision(
                    &payload.topology_id,
                    &payload.revision_id,
                    previous.as_ref().map(|revision| revision.revision_id()),
                    previous.as_ref().map(|revision| revision.spec()),
                    &payload.previous_bindings,
                    &job.operation_id,
                );
                let binding_compensation = storage.replace_topology_api_bindings(
                    &payload.topology_id,
                    &payload.previous_bindings,
                );
                let degraded = provider_compensation.is_err() || binding_compensation.is_err();
                storage
                    .finish_topology_apply(
                        &payload.topology_id,
                        &payload.revision_id,
                        &job.operation_id,
                        if degraded {
                            TopologyApplyOutcome::Degraded
                        } else {
                            TopologyApplyOutcome::Failed
                        },
                        &now_marker(),
                    )
                    .map_err(|error| {
                        format!("finalize activation cleanup could not release ownership: {error}")
                    })?;
                complete_and_project(
                    storage,
                    &mut jobs,
                    &job.job_id,
                    &job.operation_id,
                    lease_token,
                    if degraded {
                        CompletionStatus::NeedsAttention
                    } else {
                        CompletionStatus::Failed
                    },
                    serde_json::json!({"code": "TOPOLOGY_BINDING_ACTIVATION_FAILED", "phase": "FINALIZE"}),
                    format!(
                        "binding activation failed ({activation_error}); provider compensation: {}; binding compensation: {}",
                        provider_compensation
                            .err()
                            .unwrap_or_else(|| "succeeded".to_string()),
                        binding_compensation
                            .err()
                            .map(|error| error.to_string())
                            .unwrap_or_else(|| "succeeded".to_string()),
                    ),
                )?;
                return Ok(true);
            }
            lease_heartbeat.checkpoint(&mut jobs)?;
            if let Err(head_error) = storage.finish_topology_apply_fenced(
                &payload.topology_id,
                &payload.revision_id,
                &job.operation_id,
                TopologyApplyOutcome::Succeeded,
                &now_marker(),
                &job.job_id,
                &lease_token,
                now_ms(),
            ) {
                let provider_compensation = provider.compensate_applied_revision(
                    &payload.topology_id,
                    &payload.revision_id,
                    previous.as_ref().map(|revision| revision.revision_id()),
                    previous.as_ref().map(|revision| revision.spec()),
                    &payload.previous_bindings,
                    &job.operation_id,
                );
                let binding_compensation = storage.replace_topology_api_bindings(
                    &payload.topology_id,
                    &payload.previous_bindings,
                );
                let _ = storage.finish_topology_apply(
                    &payload.topology_id,
                    &payload.revision_id,
                    &job.operation_id,
                    TopologyApplyOutcome::Degraded,
                    &now_marker(),
                );
                complete_and_project(
                    storage,
                    &mut jobs,
                    &job.job_id,
                    &job.operation_id,
                    lease_token,
                    CompletionStatus::NeedsAttention,
                    serde_json::json!({"code": "TOPOLOGY_HEAD_ADVANCE_FAILED", "phase": "FINALIZE"}),
                    format!(
                        "binding projection activated but applied head did not advance ({head_error}); provider compensation: {}; binding compensation: {}",
                        provider_compensation
                            .err()
                            .unwrap_or_else(|| "succeeded".to_string()),
                        binding_compensation
                            .err()
                            .map(|error| error.to_string())
                            .unwrap_or_else(|| "succeeded".to_string()),
                    ),
                )?;
                return Ok(true);
            }
            complete_and_project(
                storage,
                &mut jobs,
                &job.job_id,
                &job.operation_id,
                lease_token,
                CompletionStatus::Succeeded,
                serde_json::json!({
                    "topology_id": payload.topology_id,
                    "revision_id": payload.revision_id,
                    "phase": "FINALIZE",
                    "bindings": active_bindings.len(),
                }),
                String::new(),
            )?;
            return Ok(true);
        }
        let health = health.unwrap_err();
        let provider_compensation = provider.compensate_applied_revision(
            &payload.topology_id,
            &payload.revision_id,
            previous.as_ref().map(|revision| revision.revision_id()),
            previous.as_ref().map(|revision| revision.spec()),
            &payload.previous_bindings,
            &job.operation_id,
        );
        let binding_compensation =
            storage.replace_topology_api_bindings(&payload.topology_id, &payload.previous_bindings);
        let degraded = provider_compensation.is_err() || binding_compensation.is_err();
        storage
            .finish_topology_apply(
                &payload.topology_id,
                &payload.revision_id,
                &job.operation_id,
                if degraded {
                    TopologyApplyOutcome::Degraded
                } else {
                    TopologyApplyOutcome::Failed
                },
                &now_marker(),
            )
            .map_err(|error| {
                format!("finalize failure could not release apply ownership: {error}")
            })?;
        let detail = format!(
            "consumer health gate failed ({health}); provider compensation: {}; binding compensation: {}",
            provider_compensation
                .err()
                .unwrap_or_else(|| "succeeded".to_string()),
            binding_compensation
                .err()
                .map(|error| error.to_string())
                .unwrap_or_else(|| "succeeded".to_string()),
        );
        complete_and_project(
            storage,
            &mut jobs,
            &job.job_id,
            &job.operation_id,
            lease_token,
            if degraded {
                CompletionStatus::NeedsAttention
            } else {
                CompletionStatus::Failed
            },
            serde_json::json!({"code": "TOPOLOGY_CONSUMER_UNHEALTHY", "phase": "FINALIZE"}),
            detail,
        )?;
        return Ok(true);
    }
    let previous_bindings = storage
        .api_bindings_for_topology(&payload.topology_id)
        .map_err(|error| error.to_string())?;
    if payload.phase == TopologyApplyPhase::Stage {
        let validation = validate_prepared_bindings(
            &payload.bindings,
            &payload.topology_id,
            &payload.revision_id,
            &job.operation_id,
        );
        if let Err(detail) = validation {
            storage
                .finish_topology_apply(
                    &payload.topology_id,
                    &payload.revision_id,
                    &job.operation_id,
                    TopologyApplyOutcome::Failed,
                    &now_marker(),
                )
                .map_err(|error| format!("{detail}; stage validation cleanup failed: {error}"))?;
            complete_and_project(
                storage,
                &mut jobs,
                &job.job_id,
                &job.operation_id,
                lease_token,
                CompletionStatus::Failed,
                serde_json::json!({"code": "TOPOLOGY_BINDING_STAGE_REJECTED", "phase": "STAGE"}),
                detail,
            )?;
            return Ok(true);
        }
        match storage.replace_topology_api_bindings(&payload.topology_id, &payload.bindings) {
            Ok(()) => complete_and_project(
                storage,
                &mut jobs,
                &job.job_id,
                &job.operation_id,
                lease_token,
                CompletionStatus::Succeeded,
                serde_json::json!({"phase": "STAGE", "bindings": payload.bindings.len()}),
                String::new(),
            )?,
            Err(error) => {
                storage
                    .finish_topology_apply(
                        &payload.topology_id,
                        &payload.revision_id,
                        &job.operation_id,
                        TopologyApplyOutcome::Failed,
                        &now_marker(),
                    )
                    .map_err(|finish| {
                        format!("binding stage failed ({error}); cleanup failed: {finish}")
                    })?;
                complete_and_project(
                    storage,
                    &mut jobs,
                    &job.job_id,
                    &job.operation_id,
                    lease_token,
                    CompletionStatus::Failed,
                    serde_json::json!({"code": "TOPOLOGY_BINDING_STAGE_FAILED", "phase": "STAGE"}),
                    error.to_string(),
                )?;
            }
        }
        return Ok(true);
    }
    if payload.phase == TopologyApplyPhase::Prepare {
        let validation = validate_prepared_bindings(
            &payload.bindings,
            &payload.topology_id,
            &payload.revision_id,
            &job.operation_id,
        )
        .and_then(|()| topology_binding_providers_healthy(storage, &payload.bindings));
        if let Err(detail) = validation {
            storage
                .finish_topology_apply(
                    &payload.topology_id,
                    &payload.revision_id,
                    &job.operation_id,
                    TopologyApplyOutcome::Failed,
                    &now_marker(),
                )
                .map_err(|error| format!("{detail}; prepare validation cleanup failed: {error}"))?;
            complete_and_project(
                storage,
                &mut jobs,
                &job.job_id,
                &job.operation_id,
                lease_token,
                CompletionStatus::Failed,
                serde_json::json!({
                    "code": "TOPOLOGY_BINDING_PREPARE_REJECTED",
                    "phase": "PREPARE",
                }),
                detail,
            )?;
            return Ok(true);
        }
        storage
            .replace_topology_api_bindings(&payload.topology_id, &payload.bindings)
            .map_err(|error| format!("prepare could not stage bindings: {error}"))?;
        lease_heartbeat.checkpoint(&mut jobs)?;
        let provider_result = provider.apply_with_bindings(
            &payload.topology_id,
            &payload.revision_id,
            revision.spec(),
            &payload.bindings,
            previous.as_ref().map(|revision| revision.revision_id()),
            previous.as_ref().map(|revision| revision.spec()),
            &payload.previous_bindings,
            &job.operation_id,
        );
        lease_heartbeat.checkpoint(&mut jobs)?;
        match provider_result {
            Ok(receipt) => {
                complete_and_project(
                    storage,
                    &mut jobs,
                    &job.job_id,
                    &job.operation_id,
                    lease_token,
                    CompletionStatus::Succeeded,
                    serde_json::json!({"phase": "PREPARE", "receipt": receipt}),
                    String::new(),
                )?;
            }
            Err(failure) => {
                let binding_compensation = storage.replace_topology_api_bindings(
                    &payload.topology_id,
                    &payload.previous_bindings,
                );
                let degraded = failure.state == TopologyProviderApplyState::Degraded
                    || binding_compensation.is_err();
                storage
                    .finish_topology_apply(
                        &payload.topology_id,
                        &payload.revision_id,
                        &job.operation_id,
                        if degraded {
                            TopologyApplyOutcome::Degraded
                        } else {
                            TopologyApplyOutcome::Failed
                        },
                        &now_marker(),
                    )
                    .map_err(|error| {
                        format!("prepare failure could not release apply ownership: {error}")
                    })?;
                complete_and_project(
                    storage,
                    &mut jobs,
                    &job.job_id,
                    &job.operation_id,
                    lease_token,
                    if degraded {
                        CompletionStatus::NeedsAttention
                    } else {
                        CompletionStatus::Failed
                    },
                    serde_json::to_value(&failure).map_err(|error| error.to_string())?,
                    format!(
                        "{failure}; binding compensation: {}",
                        binding_compensation
                            .err()
                            .map(|error| error.to_string())
                            .unwrap_or_else(|| "succeeded".to_string())
                    ),
                )?;
            }
        }
        return Ok(true);
    }
    let staged_bindings = match storage.resolve_topology_api_bindings(
        revision.spec(),
        &payload.revision_id,
        &job.operation_id,
    ) {
        Ok(bindings) => bindings,
        Err(error) => {
            let detail = format!("topology binding resolution failed: {error}");
            storage
                .finish_topology_apply(
                    &payload.topology_id,
                    &payload.revision_id,
                    &job.operation_id,
                    TopologyApplyOutcome::Failed,
                    &now_marker(),
                )
                .map_err(|finish| format!("{detail}; apply ownership cleanup failed: {finish}"))?;
            complete_and_project(
                storage,
                &mut jobs,
                &job.job_id,
                &job.operation_id,
                lease_token,
                CompletionStatus::Failed,
                serde_json::json!({"code": "TOPOLOGY_API_BINDING_INVALID"}),
                detail,
            )?;
            return Ok(true);
        }
    };
    if let Err(error) =
        storage.replace_topology_api_bindings(&payload.topology_id, &staged_bindings)
    {
        let detail = format!("topology bindings could not be staged atomically: {error}");
        storage
            .finish_topology_apply(
                &payload.topology_id,
                &payload.revision_id,
                &job.operation_id,
                TopologyApplyOutcome::Failed,
                &now_marker(),
            )
            .map_err(|finish| format!("{detail}; apply ownership cleanup failed: {finish}"))?;
        complete_and_project(
            storage,
            &mut jobs,
            &job.job_id,
            &job.operation_id,
            lease_token,
            CompletionStatus::Failed,
            serde_json::json!({"code": "TOPOLOGY_BINDING_STAGE_FAILED"}),
            detail,
        )?;
        return Ok(true);
    }

    // All provider I/O happens after the topology transaction that established
    // apply ownership has committed and before the completion transaction.
    lease_heartbeat.checkpoint(&mut jobs)?;
    let provider_result = provider.apply_with_bindings(
        &payload.topology_id,
        &payload.revision_id,
        revision.spec(),
        &staged_bindings,
        previous.as_ref().map(|revision| revision.revision_id()),
        previous.as_ref().map(|revision| revision.spec()),
        &previous_bindings,
        &job.operation_id,
    );
    lease_heartbeat.checkpoint(&mut jobs)?;
    match provider_result {
        Ok(receipt) => {
            let health_failure = topology_binding_providers_healthy(storage, &staged_bindings)
                .and_then(|()| topology_binding_consumers_healthy(storage, &staged_bindings))
                .err();
            if let Some(health_failure) = health_failure {
                let provider_compensation = provider.compensate_applied_revision(
                    &payload.topology_id,
                    &payload.revision_id,
                    previous.as_ref().map(|revision| revision.revision_id()),
                    previous.as_ref().map(|revision| revision.spec()),
                    &previous_bindings,
                    &job.operation_id,
                );
                let storage_compensation =
                    storage.replace_topology_api_bindings(&payload.topology_id, &previous_bindings);
                let degraded = provider_compensation.is_err() || storage_compensation.is_err();
                storage
                    .finish_topology_apply(
                        &payload.topology_id,
                        &payload.revision_id,
                        &job.operation_id,
                        if degraded {
                            TopologyApplyOutcome::Degraded
                        } else {
                            TopologyApplyOutcome::Failed
                        },
                        &now_marker(),
                    )
                    .map_err(|error| {
                        format!("health-gate failure could not be persisted: {error}")
                    })?;
                let detail = format!(
                    "consumer health gate failed ({health_failure}); provider compensation: {}; binding compensation: {}",
                    provider_compensation
                        .err()
                        .unwrap_or_else(|| "succeeded".to_string()),
                    storage_compensation
                        .err()
                        .map(|error| error.to_string())
                        .unwrap_or_else(|| "succeeded".to_string())
                );
                complete_and_project(
                    storage,
                    &mut jobs,
                    &job.job_id,
                    &job.operation_id,
                    lease_token,
                    if degraded {
                        CompletionStatus::NeedsAttention
                    } else {
                        CompletionStatus::Failed
                    },
                    serde_json::json!({"code": "TOPOLOGY_CONSUMER_UNHEALTHY"}),
                    detail,
                )?;
                return Ok(true);
            }
            let active_bindings = activate_staged_bindings(staged_bindings, &now_marker());
            if let Err(activation_error) =
                storage.replace_topology_api_bindings(&payload.topology_id, &active_bindings)
            {
                let provider_compensation = provider.compensate_applied_revision(
                    &payload.topology_id,
                    &payload.revision_id,
                    previous.as_ref().map(|revision| revision.revision_id()),
                    previous.as_ref().map(|revision| revision.spec()),
                    &previous_bindings,
                    &job.operation_id,
                );
                let storage_compensation =
                    storage.replace_topology_api_bindings(&payload.topology_id, &previous_bindings);
                let degraded = provider_compensation.is_err() || storage_compensation.is_err();
                storage
                    .finish_topology_apply(
                        &payload.topology_id,
                        &payload.revision_id,
                        &job.operation_id,
                        if degraded {
                            TopologyApplyOutcome::Degraded
                        } else {
                            TopologyApplyOutcome::Failed
                        },
                        &now_marker(),
                    )
                    .map_err(|error| {
                        format!("binding activation failure could not be persisted: {error}")
                    })?;
                complete_and_project(
                    storage,
                    &mut jobs,
                    &job.job_id,
                    &job.operation_id,
                    lease_token,
                    if degraded {
                        CompletionStatus::NeedsAttention
                    } else {
                        CompletionStatus::Failed
                    },
                    serde_json::json!({"code": "TOPOLOGY_BINDING_ACTIVATION_FAILED"}),
                    format!(
                        "binding activation failed ({activation_error}); provider compensation: {}; storage compensation: {}",
                        provider_compensation
                            .err()
                            .unwrap_or_else(|| "succeeded".to_string()),
                        storage_compensation
                            .err()
                            .map(|error| error.to_string())
                            .unwrap_or_else(|| "succeeded".to_string())
                    ),
                )?;
                return Ok(true);
            }
            lease_heartbeat.checkpoint(&mut jobs)?;
            storage
                .finish_topology_apply_fenced(
                    &payload.topology_id,
                    &payload.revision_id,
                    &job.operation_id,
                    TopologyApplyOutcome::Succeeded,
                    &now_marker(),
                    &job.job_id,
                    &lease_token,
                    now_ms(),
                )
                .map_err(|error| {
                    format!("providers accepted topology but durable head did not advance: {error}")
                })?;
            complete_and_project(
                storage,
                &mut jobs,
                &job.job_id,
                &job.operation_id,
                lease_token,
                CompletionStatus::Succeeded,
                serde_json::to_value(receipt).map_err(|error| error.to_string())?,
                String::new(),
            )?;
        }
        Err(failure) => {
            let binding_compensation =
                storage.replace_topology_api_bindings(&payload.topology_id, &previous_bindings);
            let degraded = failure.state == TopologyProviderApplyState::Degraded
                || binding_compensation.is_err();
            storage
                .finish_topology_apply(
                    &payload.topology_id,
                    &payload.revision_id,
                    &job.operation_id,
                    if degraded {
                        TopologyApplyOutcome::Degraded
                    } else {
                        TopologyApplyOutcome::Failed
                    },
                    &now_marker(),
                )
                .map_err(|error| {
                    format!("provider failure could not be persisted in topology status: {error}")
                })?;
            let detail = format!(
                "{}; binding compensation: {}",
                failure,
                binding_compensation
                    .err()
                    .map(|error| error.to_string())
                    .unwrap_or_else(|| "succeeded".to_string())
            );
            complete_and_project(
                storage,
                &mut jobs,
                &job.job_id,
                &job.operation_id,
                lease_token,
                if degraded {
                    CompletionStatus::NeedsAttention
                } else {
                    CompletionStatus::Failed
                },
                serde_json::to_value(failure).map_err(|error| error.to_string())?,
                detail,
            )?;
        }
    }
    Ok(true)
}

#[derive(Debug)]
struct ExternalHealthFailure {
    status: CompletionStatus,
    code: &'static str,
    detail: String,
}

fn process_external_health(
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

fn refresh_external_runtime_health(storage: &DurableStore) -> Result<(), String> {
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

fn persist_external_probe_evidence(
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

fn external_unhealthy_failure(evidence: &Value) -> ExternalHealthFailure {
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

fn bounded_external_probe_detail(detail: &str) -> String {
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

fn external_storage_failure(error: crate::durable::DurableError) -> ExternalHealthFailure {
    ExternalHealthFailure {
        status: CompletionStatus::RetryableFailure,
        code: "EXTERNAL_PROJECTION_FAILED",
        detail: error.to_string(),
    }
}

fn probe_external_endpoint(
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

fn probe_external_uri(
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

fn external_health_timeout() -> Duration {
    let millis = std::env::var("ORCHESTRATOR_EXTERNAL_HEALTH_TIMEOUT_MS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(5_000)
        .clamp(100, 30_000);
    Duration::from_millis(millis)
}

#[derive(Debug)]
struct NodeLifecycleFailure {
    code: &'static str,
    detail: String,
}

fn process_node_lifecycle(
    storage: &DurableStore,
    kind: &JobKind,
    payload: &Value,
) -> Result<Value, NodeLifecycleFailure> {
    let payload: NodeLifecyclePayload =
        serde_json::from_value(payload.clone()).map_err(|error| NodeLifecycleFailure {
            code: "INVALID_NODE_LIFECYCLE_PAYLOAD",
            detail: format!("invalid Node lifecycle payload: {error}"),
        })?;
    if payload.node_id.trim().is_empty() || payload.node_id == CONTROL_PLANE_NODE_ID {
        return Err(NodeLifecycleFailure {
            code: "INVALID_NODE_ID",
            detail: "Node lifecycle payload requires a non-control-plane node_id".to_string(),
        });
    }
    match kind {
        JobKind::NodeDrain => drain_node(storage, &payload.node_id),
        JobKind::NodeRemove => remove_node(storage, &payload.node_id),
        _ => Err(NodeLifecycleFailure {
            code: "INVALID_NODE_LIFECYCLE_KIND",
            detail: format!("job kind {kind:?} is not a Node lifecycle action"),
        }),
    }
}

fn drain_node(storage: &DurableStore, node_id: &str) -> Result<Value, NodeLifecycleFailure> {
    let mut node = storage
        .get_node(node_id)
        .map_err(node_storage_failure)?
        .ok_or_else(|| NodeLifecycleFailure {
            code: "NODE_NOT_FOUND",
            detail: format!("node {node_id} was not found"),
        })?;
    let original_status = node.status.to_ascii_uppercase();
    if original_status == "DRAINED" {
        return Ok(serde_json::json!({"node": node, "already_drained": true}));
    }
    if !matches!(original_status.as_str(), "READY" | "DRAINING") {
        return Err(NodeLifecycleFailure {
            code: "NODE_STATE_CONFLICT",
            detail: format!("node {node_id} cannot drain from state {}", node.status),
        });
    }
    if original_status == "READY" {
        node.status = "DRAINING".to_string();
        node.updated_at = now_marker();
        storage
            .upsert_node(node.clone())
            .map_err(node_storage_failure)?;
    }
    let active_jobs = storage
        .job_store()
        .active_job_count(node_id)
        .map_err(|error| NodeLifecycleFailure {
            code: "NODE_JOB_STATE_ERROR",
            detail: error.to_string(),
        })?;
    let runtime_instances = storage
        .runtime_instances(Some(node_id))
        .map_err(node_storage_failure)?;
    if active_jobs != 0 || !runtime_instances.is_empty() {
        // A job/deployment raced the preflight. Restore admission only when
        // this operation was the writer that changed READY -> DRAINING.
        if original_status == "READY" {
            node.status = "READY".to_string();
            node.updated_at = now_marker();
            storage.upsert_node(node).map_err(node_storage_failure)?;
        }
        return Err(NodeLifecycleFailure {
            code: "NODE_NOT_EMPTY",
            detail: format!(
                "node {node_id} owns {active_jobs} active jobs and {} runtime instances",
                runtime_instances.len()
            ),
        });
    }
    node.status = "DRAINED".to_string();
    node.updated_at = now_marker();
    storage
        .upsert_node(node.clone())
        .map_err(node_storage_failure)?;
    Ok(serde_json::json!({
        "node": node,
        "active_jobs": 0,
        "runtime_instances": 0,
    }))
}

fn remove_node(storage: &DurableStore, node_id: &str) -> Result<Value, NodeLifecycleFailure> {
    let Some(node) = storage.get_node(node_id).map_err(node_storage_failure)? else {
        return Ok(serde_json::json!({"node_id": node_id, "already_absent": true}));
    };
    if !node.status.eq_ignore_ascii_case("DRAINED") {
        return Err(NodeLifecycleFailure {
            code: "NODE_NOT_DRAINED",
            detail: format!("node {node_id} must be DRAINED before removal"),
        });
    }
    let active_jobs = storage
        .job_store()
        .active_job_count(node_id)
        .map_err(|error| NodeLifecycleFailure {
            code: "NODE_JOB_STATE_ERROR",
            detail: error.to_string(),
        })?;
    let runtime_instances = storage
        .runtime_instances(Some(node_id))
        .map_err(node_storage_failure)?;
    if active_jobs != 0 || !runtime_instances.is_empty() {
        return Err(NodeLifecycleFailure {
            code: "NODE_NOT_EMPTY",
            detail: format!(
                "node {node_id} owns {active_jobs} active jobs and {} runtime instances",
                runtime_instances.len()
            ),
        });
    }
    storage.delete_node(node_id).map_err(node_storage_failure)?;
    Ok(serde_json::json!({"node_id": node_id, "removed": true}))
}

fn node_storage_failure(error: crate::durable::DurableError) -> NodeLifecycleFailure {
    NodeLifecycleFailure {
        code: "NODE_STORAGE_ERROR",
        detail: error.to_string(),
    }
}

#[allow(clippy::too_many_arguments)]
fn finalize_topology_group(
    storage: &DurableStore,
    provider: &TopologyProviderSaga,
    lease_heartbeat: &ControlPlaneLeaseHeartbeat,
    jobs: &mut crate::durable::DurableJobStore,
    job_id: &str,
    operation_id: &str,
    lease_token: String,
    group: &[TopologyApplyGroupPayloadMember],
) -> Result<bool, String> {
    let prepared = (|| -> Result<Vec<orchestrator_storage::TopologyApplyGroupMember>, String> {
        if group.is_empty() {
            return Err("FINALIZE_GROUP requires at least one topology member".to_string());
        }
        let mut identities = group
            .iter()
            .map(|member| (member.topology_id.clone(), member.revision_id.clone()))
            .collect::<Vec<_>>();
        identities.sort();
        if identities.iter().any(|(topology_id, revision_id)| {
            topology_id.trim().is_empty() || revision_id.trim().is_empty()
        }) || identities.windows(2).any(|pair| pair[0].0 == pair[1].0)
        {
            return Err(
                "FINALIZE_GROUP members must have unique non-empty topology identities".to_string(),
            );
        }
        let mut result = Vec::with_capacity(identities.len());
        for (topology_id, revision_id) in identities {
            lease_heartbeat.checkpoint(jobs)?;
            let heads = storage
                .topology_heads(&topology_id)
                .map_err(|error| error.to_string())?
                .ok_or_else(|| format!("topology {topology_id} disappeared"))?;
            if heads.applying_revision_id.as_deref() != Some(revision_id.as_str())
                || heads.applying_operation_id.as_deref() != Some(operation_id)
            {
                return Err(format!(
                    "topology {topology_id} no longer owns revision {revision_id} for operation {operation_id}"
                ));
            }
            let revision = storage
                .topology_revision(&topology_id, &revision_id)
                .map_err(|error| error.to_string())?
                .ok_or_else(|| format!("topology revision {revision_id} disappeared"))?;
            let staged_bindings = storage
                .api_bindings_for_topology(&topology_id)
                .map_err(|error| error.to_string())?;
            validate_prepared_bindings(&staged_bindings, &topology_id, &revision_id, operation_id)?;
            topology_binding_providers_healthy(storage, &staged_bindings)
                .and_then(|()| topology_binding_consumers_healthy(storage, &staged_bindings))?;
            let content_sha256 = revision
                .spec()
                .content_sha256()
                .map_err(|error| error.to_string())?;
            let projection_sha256 = provider_projection_sha256(&staged_bindings)?;
            let observed = provider.observe(&topology_id);
            lease_heartbeat.checkpoint(jobs)?;
            if !observed
                .gateway
                .matches(&revision_id, &content_sha256, &projection_sha256)
                || !observed
                    .auth
                    .matches(&revision_id, &content_sha256, &projection_sha256)
            {
                return Err(format!(
                    "topology {topology_id} provider evidence does not acknowledge revision {revision_id}"
                ));
            }
            result.push(orchestrator_storage::TopologyApplyGroupMember {
                topology_id,
                revision_id,
                active_bindings: activate_staged_bindings(staged_bindings, &now_marker()),
            });
        }
        normalize_group_binding_moves(&mut result);
        Ok(result)
    })();
    match prepared.and_then(|members| {
        lease_heartbeat.checkpoint(jobs)?;
        storage
            .finish_topology_apply_group_fenced(
                &members,
                operation_id,
                &now_marker(),
                job_id,
                &lease_token,
                now_ms(),
            )
            .map(|_| members)
            .map_err(|error| error.to_string())
    }) {
        Ok(members) => complete_and_project(
            storage,
            jobs,
            job_id,
            operation_id,
            lease_token,
            CompletionStatus::Succeeded,
            serde_json::json!({
                "phase": "FINALIZE_GROUP",
                "topologies": members.iter().map(|member| serde_json::json!({
                    "topology_id": member.topology_id,
                    "revision_id": member.revision_id,
                    "bindings": member.active_bindings.len(),
                })).collect::<Vec<_>>(),
            }),
            String::new(),
        )?,
        Err(detail) => complete_and_project(
            storage,
            jobs,
            job_id,
            operation_id,
            lease_token,
            CompletionStatus::Failed,
            serde_json::json!({
                "code": "TOPOLOGY_GROUP_FINALIZE_REJECTED",
                "phase": "FINALIZE_GROUP",
            }),
            detail,
        )?,
    }
    Ok(true)
}

#[allow(clippy::too_many_arguments)]
fn complete_and_project(
    storage: &DurableStore,
    jobs: &mut crate::durable::DurableJobStore,
    job_id: &str,
    operation_id: &str,
    lease_token: String,
    status: CompletionStatus,
    result: Value,
    error_message: String,
) -> Result<(), String> {
    jobs.complete(CompleteRequest {
        job_id: job_id.to_string(),
        lease_token,
        status,
        result,
        error_message,
        now_ms: now_ms(),
        events: Vec::new(),
    })
    .map_err(|error| error.to_string())?;
    let mut operations = storage.operation_store();
    OperationCoordinator::new(&mut operations, jobs)
        .project(operation_id, now_ms())
        .map_err(|error| error.to_string())?;
    Ok(())
}

fn lease_token() -> Result<String, String> {
    let mut bytes = [0_u8; 32];
    random_fill(&mut bytes).map_err(|_| "generate topology worker lease token".to_string())?;
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(i64::MAX as u128) as i64
}

fn now_marker() -> String {
    format!("unix-ms:{}", now_ms())
}
