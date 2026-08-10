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
};
use orchestrator_legacy::{
    ApiBinding, ApiBindingState, Endpoint, EndpointProbe, TcpEndpointProbe,
    TopologyDeploymentStatus, TopologyDesiredDeploymentState, TopologyDrift, TopologyDriftKind,
    TopologyEndpointStatus, TopologyHealth, TopologyLinkStatus, TopologyObservedDeploymentState,
    TopologyReconciliationState, TopologyResourceKind, TopologySpec, TopologyStatus,
    parse_endpoint_id, validate_endpoint_id,
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
            binding.observed_state = "ACTIVE".to_string();
            binding.health = "HEALTHY".to_string();
            binding.reason.clear();
        } else {
            binding.state = ApiBindingState::Revoked;
            binding.observed_state = "REVOKED".to_string();
            binding.health = "UNKNOWN".to_string();
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
        if job.kind != JobKind::TopologyApply {
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
        if payload.phase == TopologyApplyPhase::Abort
            && heads.applying_revision_id.is_none()
            && heads.applied_revision_id.as_deref() != Some(payload.revision_id.as_str())
        {
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
        let finish = if aborting_completed_group_member {
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
                .map_err(|error| error.to_string())
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

#[cfg(test)]
mod tests {
    use super::*;
    use orchestrator_control_plane::{
        DurableOperationStatus, OperationRepository, PlanOperation, PlannedJob,
    };
    use orchestrator_legacy::{NodeRecord, TopologyEndpointSpec, TopologyLinkSpec};
    use orchestrator_runtime::RuntimeInstance;
    use orchestrator_storage::{SqliteOrchestratorStore, StoredRuntimeInstance};
    use serde_json::json;
    use std::io::{Read, Write};
    use std::net::TcpListener;

    fn spec() -> TopologySpec {
        let gateway = TopologyEndpointSpec {
            endpoint: "127.0.0.1:8080:gateway".to_string(),
            service_id: "gateway".to_string(),
            protocol: "http".to_string(),
            health_path: "/health".to_string(),
            display_name: String::new(),
            note: String::new(),
            config: json!({}),
        };
        let worker = TopologyEndpointSpec {
            endpoint: "127.0.0.1:8081:worker".to_string(),
            service_id: "worker".to_string(),
            protocol: "http".to_string(),
            health_path: "/health".to_string(),
            display_name: String::new(),
            note: String::new(),
            config: json!({}),
        };
        TopologySpec::new(
            "primary",
            gateway.endpoint.clone(),
            "private",
            vec![gateway.clone(), worker.clone()],
            vec![TopologyLinkSpec {
                source_endpoint: gateway.endpoint,
                target_endpoint: worker.endpoint,
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
        .unwrap()
    }

    fn instance(
        service_id: &str,
        desired_state: RuntimeDesiredState,
        observed_state: RuntimeObservedState,
        health: &str,
    ) -> StoredRuntimeInstance {
        StoredRuntimeInstance {
            node_id: "node-1".to_string(),
            instance: RuntimeInstance {
                deployment_id: format!("deployment-{service_id}"),
                service_id: service_id.to_string(),
                release_version: "1.0.0".to_string(),
                container_id: format!("container-{service_id}"),
                artifact_digest: format!("sha256:{}", "a".repeat(64)),
                runtime_contract: orchestrator_runtime::RuntimeContract::standard_v1(),
                runtime_policy_sha256: String::new(),
                effective_runtime_sha256: String::new(),
                runtime_attested: true,
                desired_state,
                observed_state,
                health: health.to_string(),
            },
            management_mode: orchestrator_storage::RuntimeManagementMode::Managed,
            endpoint: match service_id {
                "gateway" => "127.0.0.1:8080:gateway".to_string(),
                "worker" => "127.0.0.1:8081:worker".to_string(),
                _ => String::new(),
            },
            external_probe_protocol: String::new(),
            external_probe_health_path: String::new(),
            last_observed_at_ms: 0,
            drift_reason: String::new(),
            credential_expires_at_ms: 0,
            credential_last_success_at_ms: 0,
            credential_last_error: String::new(),
            updated_at: "unix-ms:1".to_string(),
        }
    }

    fn providers() -> TopologyProvidersObservation {
        let observation = |provider: &str| TopologyProviderObservation {
            provider: provider.to_string(),
            state: TopologyProviderObservedState::Present,
            observed_revision_id: Some("primary:r1:test".to_string()),
            observed_content_sha256: Some("a".repeat(64)),
            observed_projection_sha256: Some(
                "fa9d28278a0d02b19bfebeae5afd5aa6dde1c685d8396acc8defe8832848865c".to_string(),
            ),
            endpoints: Vec::new(),
            links: Vec::new(),
            detail: String::new(),
        };
        TopologyProvidersObservation {
            gateway: observation("gateway"),
            auth: observation("auth"),
        }
    }

    #[test]
    fn runtime_projection_transition_is_fail_closed_for_remove_add_and_replace() {
        let previous = BTreeMap::from([
            ("binding-a".to_string(), "digest-a".to_string()),
            ("binding-b".to_string(), "digest-b".to_string()),
        ]);
        let removed = BTreeMap::from([("binding-a".to_string(), "digest-a".to_string())]);
        assert_eq!(
            runtime_projection_transition(&previous, &removed),
            RuntimeProjectionTransition::Revoke
        );
        assert_eq!(
            runtime_projection_transition(&removed, &previous),
            RuntimeProjectionTransition::Grant
        );
        let replaced = BTreeMap::from([
            ("binding-a".to_string(), "new-digest".to_string()),
            ("binding-b".to_string(), "digest-b".to_string()),
        ]);
        assert_eq!(
            runtime_projection_transition(&previous, &replaced),
            RuntimeProjectionTransition::Mixed
        );
        assert_eq!(
            runtime_projection_transition(&previous, &previous),
            RuntimeProjectionTransition::Unchanged
        );
    }

    #[test]
    fn activated_binding_route_survives_transient_health_but_not_explicit_stop_or_drift() {
        let mut runtime = instance(
            "worker",
            RuntimeDesiredState::Running,
            RuntimeObservedState::Running,
            "UNHEALTHY",
        );
        runtime.last_observed_at_ms = 1;
        assert!(
            runtime_preserves_active_binding_route(&runtime),
            "business health and stale observation are status evidence, not authorization revocation"
        );

        runtime.instance.desired_state = RuntimeDesiredState::Stopped;
        assert!(!runtime_preserves_active_binding_route(&runtime));

        runtime.instance.desired_state = RuntimeDesiredState::Running;
        runtime.instance.runtime_attested = false;
        assert!(!runtime_preserves_active_binding_route(&runtime));

        runtime.instance.runtime_attested = true;
        runtime.drift_reason = "HostConfig digest changed".to_string();
        assert!(!runtime_preserves_active_binding_route(&runtime));
    }

    #[test]
    fn external_health_replay_reprobes_and_reconciler_recovers_projection() {
        let directory = tempfile::tempdir().unwrap();
        let store = DurableStore::Sqlite(
            SqliteOrchestratorStore::open(directory.path().join("external-health.db")).unwrap(),
        );
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let serve_once = |listener: TcpListener| {
            std::thread::spawn(move || {
                let (mut stream, _) = listener.accept().unwrap();
                let mut request = [0_u8; 1024];
                let _ = stream.read(&mut request).unwrap();
                stream
                    .write_all(
                        b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok",
                    )
                    .unwrap();
            })
        };
        let initial_server = serve_once(listener);
        let payload = json!({
            "deployment_id": "deployment-external",
            "service_id": "external-api",
            "version": "1.0.0",
            "endpoint": format!("http://{address}"),
            "protocol": "http",
            "health_path": "/health",
            "artifact_digest": format!("registry.example/external@sha256:{}", "a".repeat(64)),
        });
        let first = process_external_health(&store, &payload).unwrap();
        initial_server.join().unwrap();
        assert_eq!(first["instance"]["instance"]["health"], "HEALTHY");

        let first_observed = store
            .runtime_instance("deployment-external")
            .unwrap()
            .unwrap()
            .last_observed_at_ms;
        let replay = process_external_health(&store, &payload).unwrap_err();
        assert_eq!(replay.code, "EXTERNAL_ENDPOINT_UNHEALTHY");
        let unavailable = store
            .runtime_instance("deployment-external")
            .unwrap()
            .unwrap();
        assert_eq!(unavailable.instance.health, "UNHEALTHY");
        assert_eq!(
            unavailable.instance.observed_state,
            RuntimeObservedState::Unknown
        );
        assert!(unavailable.last_observed_at_ms >= first_observed);

        let recovery_listener = TcpListener::bind(address).unwrap();
        let recovery_server = serve_once(recovery_listener);
        refresh_external_runtime_health(&store).unwrap();
        recovery_server.join().unwrap();
        let recovered = store
            .runtime_instance("deployment-external")
            .unwrap()
            .unwrap();
        assert_eq!(recovered.instance.health, "HEALTHY");
        assert_eq!(
            recovered.instance.observed_state,
            RuntimeObservedState::Running
        );
        assert!(recovered.drift_reason.is_empty());
        assert_eq!(recovered.external_probe_protocol, "http");
        assert_eq!(recovered.external_probe_health_path, "/health");
    }

    #[test]
    fn topology_status_does_not_accept_runtime_health_without_real_network_probes() {
        let runtime = vec![
            instance(
                "gateway",
                RuntimeDesiredState::Running,
                RuntimeObservedState::Running,
                "healthy",
            ),
            instance(
                "worker",
                RuntimeDesiredState::Running,
                RuntimeObservedState::Running,
                "healthy",
            ),
        ];
        let mut drift = Vec::new();
        let (deployments, endpoints, links) = runtime_topology_status(
            &spec(),
            &runtime,
            &[],
            &BTreeSet::from(["127.0.0.1:8080:gateway".to_string()]),
            &providers(),
            None,
            &NetworkProbePool::new(),
            "unix-ms:2",
            &mut drift,
        );
        assert!(!drift.is_empty());
        assert!(
            deployments
                .iter()
                .all(|deployment| deployment.health == TopologyHealth::Healthy)
        );
        assert!(endpoints.iter().all(|endpoint| {
            !endpoint.reachable && endpoint.health == TopologyHealth::Unhealthy
        }));
        assert_eq!(links[0].health, TopologyHealth::Unknown);
    }

    #[test]
    fn missing_managed_runtime_health_is_visible_as_drift() {
        let runtime = vec![instance(
            "worker",
            RuntimeDesiredState::Running,
            RuntimeObservedState::Stopped,
            "unknown",
        )];
        let mut drift = Vec::new();
        let (_deployments, endpoints, _links) = runtime_topology_status(
            &spec(),
            &runtime,
            &[],
            &BTreeSet::from(["127.0.0.1:8080:gateway".to_string()]),
            &providers(),
            None,
            &NetworkProbePool::new(),
            "unix-ms:2",
            &mut drift,
        );
        let worker = endpoints
            .iter()
            .find(|endpoint| endpoint.endpoint.ends_with(":worker"))
            .unwrap();
        assert!(!worker.reachable);
        assert_eq!(worker.health, TopologyHealth::Unknown);
        assert!(drift.iter().any(|item| {
            item.resource_kind == TopologyResourceKind::Deployment
                && item.resource_id == "deployment-worker"
        }));
        assert!(drift.iter().any(|item| {
            item.resource_kind == TopologyResourceKind::Endpoint
                && item.resource_id.ends_with(":worker")
        }));
    }

    #[test]
    fn healthy_but_unattested_managed_runtime_degrades_topology_projection() {
        let mut worker = instance(
            "worker",
            RuntimeDesiredState::Running,
            RuntimeObservedState::Running,
            "HEALTHY",
        );
        worker.instance.runtime_attested = false;
        let runtime = vec![worker];
        let mut drift = Vec::new();
        let _ = runtime_topology_status(
            &spec(),
            &runtime,
            &[],
            &BTreeSet::new(),
            &providers(),
            None,
            &NetworkProbePool::new(),
            "unix-ms:2",
            &mut drift,
        );
        assert!(drift.iter().any(|item| {
            item.resource_kind == TopologyResourceKind::Deployment
                && item.resource_id == "deployment-worker"
                && item.detail.contains("attestation")
        }));
    }

    #[test]
    fn provider_health_cannot_mask_missing_exact_runtime_endpoints() {
        let mut providers = providers();
        providers.gateway.endpoints.push(TopologyEndpointStatus {
            endpoint: "127.0.0.1:8080:gateway".to_string(),
            health: TopologyHealth::Healthy,
            reachable: true,
            latency_ms: Some(3),
            message: String::new(),
            observed_at: "unix-ms:1".to_string(),
        });
        let link = TopologyLinkStatus {
            source_endpoint: "127.0.0.1:8080:gateway".to_string(),
            target_endpoint: "127.0.0.1:8081:worker".to_string(),
            health: TopologyHealth::Healthy,
            latency_ms: Some(4),
            message: String::new(),
            observed_at: "unix-ms:1".to_string(),
        };
        providers.gateway.links.push(link.clone());
        providers.auth.links.push(link);
        let mut drift = Vec::new();
        let (_deployments, endpoints, links) = runtime_topology_status(
            &spec(),
            &[],
            &[],
            &BTreeSet::from(["127.0.0.1:8080:gateway".to_string()]),
            &providers,
            None,
            &NetworkProbePool::new(),
            "unix-ms:2",
            &mut drift,
        );
        let gateway = endpoints
            .iter()
            .find(|endpoint| endpoint.endpoint.ends_with(":gateway"))
            .unwrap();
        let worker = endpoints
            .iter()
            .find(|endpoint| endpoint.endpoint.ends_with(":worker"))
            .unwrap();
        assert_eq!(gateway.health, TopologyHealth::Unknown);
        assert!(!gateway.reachable);
        assert_eq!(worker.health, TopologyHealth::Unknown);
        assert_eq!(links[0].health, TopologyHealth::Unknown);
        assert!(!drift.is_empty());
    }

    #[test]
    fn network_probe_pool_keeps_one_fixed_worker_set_across_batches() {
        let pool = NetworkProbePool::new();
        let worker_ids = pool
            .workers
            .iter()
            .map(|worker| worker.thread().id())
            .collect::<Vec<_>>();

        assert!(pool.probe_endpoints(&[], "unix-ms:1").is_empty());
        assert!(pool.probe_links(&[], "unix-ms:2").is_empty());

        assert_eq!(pool.workers.len(), NETWORK_PROBE_CONCURRENCY);
        assert_eq!(
            pool.workers
                .iter()
                .map(|worker| worker.thread().id())
                .collect::<Vec<_>>(),
            worker_ids
        );
        assert!(pool.workers.iter().all(|worker| !worker.is_finished()));
    }

    #[test]
    fn control_plane_worker_drains_and_removes_a_node_without_topology_providers() {
        let directory = tempfile::tempdir().unwrap();
        let sqlite = SqliteOrchestratorStore::open(directory.path().join("orchestrator.db"))
            .expect("open durable store");
        let durable = DurableStore::Sqlite(sqlite);
        durable
            .upsert_node(NodeRecord {
                node_id: "node-1".to_string(),
                host_ip: "127.0.0.2".to_string(),
                parent_node_id: String::new(),
                role: "standalone".to_string(),
                labels: json!({}),
                status: "READY".to_string(),
                created_at: "unix-ms:1".to_string(),
                updated_at: "unix-ms:1".to_string(),
            })
            .unwrap();

        let enqueue = |durable: &DurableStore, operation_id: &str, action: &str, kind| {
            let mut operations = durable.operation_store();
            let mut jobs = durable.job_store();
            let mut coordinator = OperationCoordinator::new(&mut operations, &mut jobs);
            let operation = coordinator
                .plan(
                    PlanOperation {
                        operation_id: operation_id.to_string(),
                        action: action.to_string(),
                        target_type: "Node".to_string(),
                        target_id: "node-1".to_string(),
                        request: json!({"auto_enqueue": true}),
                        jobs: vec![PlannedJob {
                            step_id: "node-lifecycle".to_string(),
                            node_id: CONTROL_PLANE_NODE_ID.to_string(),
                            kind,
                            depends_on: vec![],
                            condition: Default::default(),
                            payload: json!({"node_id": "node-1"}),
                            max_attempts: 1,
                        }],
                    },
                    1,
                )
                .unwrap();
            coordinator.confirm(&operation.operation_id, 2).unwrap();
            coordinator.enqueue(&operation.operation_id, 3).unwrap();
        };

        enqueue(&durable, "op-drain", "node.drain", JobKind::NodeDrain);
        assert!(process_one(&durable, None).unwrap());
        assert_eq!(
            durable.get_node("node-1").unwrap().unwrap().status,
            "DRAINED"
        );
        assert_eq!(
            durable
                .operation_store()
                .get("op-drain")
                .unwrap()
                .unwrap()
                .status,
            DurableOperationStatus::Succeeded
        );

        enqueue(&durable, "op-remove", "node.remove", JobKind::NodeRemove);
        assert!(process_one(&durable, None).unwrap());
        assert!(durable.get_node("node-1").unwrap().is_none());
        assert_eq!(
            durable
                .operation_store()
                .get("op-remove")
                .unwrap()
                .unwrap()
                .status,
            DurableOperationStatus::Succeeded
        );
    }
}

#[cfg(test)]
mod ga_tests {
    use super::*;
    use crate::http::{ApiRequest, ApiResponse};
    use crate::topology_provider::{
        HttpManagementProviderConfig, TopologyProviderConfig, TopologyProviderSaga,
        provider_projection_sha256_from_json,
    };
    use orchestrator_control_plane::{
        ClaimRequest, DurableOperationStatus, JobStatus, JobStore, NewJob, OperationRepository,
        PlanOperation, PlannedJob,
    };
    use orchestrator_legacy::{
        OrchestratorStore, ServiceRelease, ServiceReleaseManifest, TopologyEndpointSpec,
        TopologyLinkSpec, TopologyRevision, service_manifest_from_release,
    };
    use orchestrator_storage::{
        RuntimeManagementMode, SqliteOrchestratorStore, StoredNodeRuntimeFacts,
        StoredRuntimeInstance,
    };
    use serde_json::{Value, json};
    use std::collections::BTreeMap;
    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::path::Path;
    use std::sync::{Arc, Barrier};
    use std::thread::{self, JoinHandle};
    use std::time::{Duration, Instant};

    #[derive(Clone)]
    enum ProviderCall {
        Mutation {
            action: &'static str,
            status: u16,
            expected_routes: Option<usize>,
            expected_operation_phase: Option<&'static str>,
        },
        Observe {
            status: u16,
            revision_id: String,
            content_sha256: String,
            projection_sha256: Option<String>,
        },
        ObserveAbsent,
        ObserveApplied,
    }

    struct MockProvider {
        origin: String,
        thread: JoinHandle<()>,
    }

    #[test]
    fn sqlite_v1_topology_flow_is_durable_versioned_and_reconcilable() {
        let directory = tempfile::tempdir().unwrap();
        let database_path = directory.path().join("orchestrator.db");
        let store = initialize_store(&database_path);

        let initial_spec = topology_spec("initial");
        let create = api(
            &store,
            None,
            request(
                "POST",
                "/api/v1/topologies",
                serde_json::to_string(&initial_spec).unwrap(),
                None,
                "create-initial",
            ),
            "req-create",
        );
        assert_eq!(create.status, 201, "{}", create.body);
        let first_revision_id = create.body["data"]["revision"]["revision_id"]
            .as_str()
            .unwrap()
            .to_string();
        assert_eq!(
            create.headers.get("ETag"),
            Some(&format!("\"{first_revision_id}\""))
        );

        let validate = api(
            &store,
            None,
            request(
                "POST",
                "/api/v1/topologies/primary:validate",
                serde_json::to_string(&initial_spec).unwrap(),
                None,
                "validate-initial",
            ),
            "req-validate",
        );
        assert_eq!(validate.status, 200, "{}", validate.body);
        assert_eq!(validate.body["data"]["valid"], true);

        let initial_diff_request = request(
            "POST",
            "/api/v1/topologies/primary:diff",
            "{}",
            None,
            "diff-initial",
        );
        let initial_diff_a = api(&store, None, initial_diff_request.clone(), "req-diff-a");
        let initial_diff_b = api(&store, None, initial_diff_request, "req-diff-b");
        assert_eq!(initial_diff_a.status, 200, "{}", initial_diff_a.body);
        assert_eq!(
            serde_json::to_vec(&initial_diff_a.body["data"]["diff"]).unwrap(),
            serde_json::to_vec(&initial_diff_b.body["data"]["diff"]).unwrap(),
            "the same revision pair must produce byte-stable JSON diff output"
        );

        let (initial_provider, initial_mocks) =
            provider_pair(successful_apply(), successful_apply());
        let initial_apply = api(
            &store,
            Some(&initial_provider),
            request(
                "POST",
                "/api/v1/topologies/primary:apply",
                "{}",
                Some(&first_revision_id),
                "apply-initial",
            ),
            "req-apply-initial",
        );
        assert_eq!(initial_apply.status, 202, "{}", initial_apply.body);
        let initial_operation_id = initial_apply.body["data"]["operation_id"]
            .as_str()
            .unwrap()
            .to_string();

        // A queued apply is safe to resume after the process reopens the same
        // SQLite file because no provider side effect has started yet.
        drop(store);
        let store = reopen_store(&database_path);
        process_until_operation_terminal(&store, &initial_provider, &initial_operation_id);
        join_providers(initial_mocks);
        let first_heads = store.topology_heads("primary").unwrap().unwrap();
        assert_eq!(
            first_heads.applied_revision_id.as_deref(),
            Some(first_revision_id.as_str())
        );
        let first_status = api(
            &store,
            None,
            request(
                "GET",
                "/api/v1/topologies/primary/status",
                "",
                None,
                "status-initial",
            ),
            "req-status-initial",
        );
        assert_eq!(first_status.status, 200, "{}", first_status.body);
        assert_eq!(first_status.body["data"]["status"]["state"], "IN_SYNC");

        // Two editors using the same ETag race through the real SQLite CAS.
        // Exactly one immutable revision is committed and the other receives
        // the public conflict response.
        let barrier = Arc::new(Barrier::new(3));
        let mut editors = Vec::new();
        for (index, note) in ["editor-a", "editor-b"].into_iter().enumerate() {
            let editor_store = store.clone();
            let editor_barrier = Arc::clone(&barrier);
            let expected = first_revision_id.clone();
            let spec = topology_spec(note);
            editors.push(thread::spawn(move || {
                editor_barrier.wait();
                api(
                    &editor_store,
                    None,
                    request(
                        "POST",
                        "/api/v1/topologies/primary/revisions",
                        serde_json::to_string(&spec).unwrap(),
                        Some(&expected),
                        &format!("concurrent-edit-{index}"),
                    ),
                    &format!("req-concurrent-{index}"),
                )
            }));
        }
        barrier.wait();
        let mut editor_responses = editors
            .into_iter()
            .map(|editor| editor.join().unwrap())
            .collect::<Vec<_>>();
        editor_responses.sort_by_key(|response| response.status);
        assert_eq!(
            editor_responses
                .iter()
                .map(|response| response.status)
                .collect::<Vec<_>>(),
            vec![201, 409]
        );
        assert_eq!(
            editor_responses[1].body["code"],
            "TOPOLOGY_REVISION_CONFLICT"
        );
        let second_revision_id = store
            .topology_heads("primary")
            .unwrap()
            .unwrap()
            .draft_revision_id;
        assert_ne!(second_revision_id, first_revision_id);

        let stale = api(
            &store,
            None,
            request(
                "POST",
                "/api/v1/topologies/primary/revisions",
                serde_json::to_string(&topology_spec("stale-editor")).unwrap(),
                Some(&first_revision_id),
                "stale-edit",
            ),
            "req-stale",
        );
        assert_eq!(stale.status, 409, "{}", stale.body);
        assert_eq!(stale.body["code"], "TOPOLOGY_REVISION_CONFLICT");

        let diff_body = json!({
            "from_revision_id": first_revision_id,
            "to_revision_id": second_revision_id,
        })
        .to_string();
        let diff_a = api(
            &store,
            None,
            request(
                "POST",
                "/api/v1/topologies/primary:diff",
                diff_body.clone(),
                None,
                "diff-second-a",
            ),
            "req-second-diff-a",
        );
        let diff_b = api(
            &store,
            None,
            request(
                "POST",
                "/api/v1/topologies/primary:diff",
                diff_body,
                None,
                "diff-second-b",
            ),
            "req-second-diff-b",
        );
        assert_eq!(diff_a.status, 200, "{}", diff_a.body);
        assert_eq!(
            serde_json::to_vec(&diff_a.body["data"]["diff"]).unwrap(),
            serde_json::to_vec(&diff_b.body["data"]["diff"]).unwrap()
        );
        assert!(
            diff_a.body["data"]["diff"]["changes"]
                .as_array()
                .is_some_and(|changes| !changes.is_empty())
        );

        let (second_provider, second_mocks) = provider_pair(successful_apply(), successful_apply());
        let second_apply = api(
            &store,
            Some(&second_provider),
            request(
                "POST",
                "/api/v1/topologies/primary:apply",
                "{}",
                Some(&second_revision_id),
                "apply-second",
            ),
            "req-apply-second",
        );
        assert_eq!(second_apply.status, 202, "{}", second_apply.body);
        let second_operation_id = second_apply.body["data"]["operation_id"].as_str().unwrap();
        process_until_operation_terminal(&store, &second_provider, second_operation_id);
        join_providers(second_mocks);
        assert_eq!(
            store
                .topology_heads("primary")
                .unwrap()
                .unwrap()
                .applied_revision_id
                .as_deref(),
            Some(second_revision_id.as_str())
        );

        let (rollback_provider, rollback_mocks) =
            provider_pair(successful_apply(), successful_apply());
        let rollback_response = api(
            &store,
            Some(&rollback_provider),
            request(
                "POST",
                "/api/v1/topologies/primary:rollback",
                json!({"revision_id": first_revision_id}).to_string(),
                Some(&second_revision_id),
                "rollback-first",
            ),
            "req-rollback",
        );
        assert_eq!(rollback_response.status, 202, "{}", rollback_response.body);
        let rollback_revision_id = rollback_response.body["data"]["revision_id"]
            .as_str()
            .unwrap()
            .to_string();
        assert_ne!(rollback_revision_id, first_revision_id);
        assert_ne!(rollback_revision_id, second_revision_id);
        let rollback_operation_id = rollback_response.body["data"]["operation_id"]
            .as_str()
            .unwrap();
        process_until_operation_terminal(&store, &rollback_provider, rollback_operation_id);
        join_providers(rollback_mocks);
        let rollback = store
            .topology_revision("primary", &rollback_revision_id)
            .unwrap()
            .unwrap();
        assert_eq!(rollback.revision_number(), 3);
        assert_eq!(
            rollback.rollback_of_revision_id(),
            Some(first_revision_id.as_str())
        );
        assert_eq!(rollback.spec(), &initial_spec);
        assert_eq!(
            store
                .topology_heads("primary")
                .unwrap()
                .unwrap()
                .applied_revision_id
                .as_deref(),
            Some(rollback_revision_id.as_str())
        );

        // A direct provider-side change is observed, never inferred from the
        // desired Endpoint/Link fields, and persisted as explicit drift.
        let second = store
            .topology_revision("primary", &second_revision_id)
            .unwrap()
            .unwrap();
        let stale_sha256 = second.spec().content_sha256().unwrap();
        let (drift_provider, drift_mocks) = provider_pair(
            vec![observe(&second_revision_id, &stale_sha256)],
            vec![observe(&second_revision_id, &stale_sha256)],
        );
        let last_operation_id = store
            .topology_heads("primary")
            .unwrap()
            .unwrap()
            .last_operation_id;
        reconcile_one(
            &store,
            &drift_provider,
            "primary",
            &rollback_revision_id,
            last_operation_id,
            &NetworkProbePool::new(),
        )
        .unwrap();
        join_providers(drift_mocks);
        let drifted = store.topology_status("primary").unwrap().unwrap();
        assert_eq!(drifted.state, TopologyReconciliationState::Degraded);
        assert_eq!(
            drifted.desired_revision_id.as_deref(),
            Some(rollback_revision_id.as_str())
        );
        assert!(drifted.observed_revision_id.is_none());
        assert!(drifted.drift.iter().any(|drift| {
            drift.resource_kind == TopologyResourceKind::Authority
                && drift.kind == TopologyDriftKind::Changed
        }));

        drop(store);
        let restarted = reopen_store(&database_path);
        assert_eq!(restarted.topology_revisions("primary").unwrap().len(), 3);
        assert_eq!(
            restarted.topology_status("primary").unwrap().unwrap(),
            drifted
        );
    }

    #[test]
    fn reconciler_persists_stale_runtime_evidence_as_degraded_status() {
        let directory = tempfile::tempdir().unwrap();
        let store = initialize_store(&directory.path().join("orchestrator.db"));
        let gateway = TopologyEndpointSpec {
            endpoint: "127.0.0.1:8080:gateway".to_string(),
            service_id: "gateway".to_string(),
            protocol: "http".to_string(),
            health_path: "/health".to_string(),
            display_name: "Gateway".to_string(),
            note: String::new(),
            config: json!({"deployment_id": "deployment-gateway"}),
        };
        let revision = store
            .create_initial_topology_revision(
                TopologySpec::new(
                    "primary",
                    gateway.endpoint.clone(),
                    "private",
                    vec![gateway],
                    Vec::new(),
                )
                .unwrap(),
                "unix-ms:1".to_string(),
                "admin".to_string(),
                "initial".to_string(),
            )
            .unwrap();
        let (apply_provider, apply_mocks) = provider_pair(successful_apply(), successful_apply());
        let apply_response = enqueue_revision(
            &store,
            &apply_provider,
            revision.revision_id(),
            "apply-stale",
        );
        process_until_operation_terminal(
            &store,
            &apply_provider,
            apply_response.body["data"]["operation_id"]
                .as_str()
                .unwrap(),
        );
        join_providers(apply_mocks);

        let mut facts = store
            .node_runtime_facts("node-gateway")
            .unwrap()
            .expect("runtime facts fixture");
        facts.received_at_ms =
            now_ms().saturating_sub(crate::durable::MANAGED_RUNTIME_REPORT_STALE_MS + 1);
        store.put_node_runtime_facts(&facts).unwrap();
        let content_sha256 = revision.spec().content_sha256().unwrap();
        let (observe_provider, observe_mocks) = provider_pair(
            vec![observe(revision.revision_id(), &content_sha256)],
            vec![observe(revision.revision_id(), &content_sha256)],
        );
        let last_operation_id = store
            .topology_heads("primary")
            .unwrap()
            .unwrap()
            .last_operation_id;
        reconcile_one(
            &store,
            &observe_provider,
            "primary",
            revision.revision_id(),
            last_operation_id,
            &NetworkProbePool::new(),
        )
        .unwrap();
        join_providers(observe_mocks);

        let status = store.topology_status("primary").unwrap().unwrap();
        assert_eq!(status.state, TopologyReconciliationState::Degraded);
        let deployment = status
            .deployments
            .iter()
            .find(|deployment| deployment.deployment_id == "deployment-gateway")
            .expect("gateway deployment status");
        assert_eq!(
            deployment.observed_state,
            TopologyObservedDeploymentState::Unknown
        );
        assert_eq!(deployment.health, TopologyHealth::Unknown);
        assert!(status.drift.iter().any(|drift| {
            drift.resource_kind == TopologyResourceKind::Deployment
                && drift.resource_id == "deployment-gateway"
                && drift.detail.contains("older than 60 seconds")
        }));
    }

    #[test]
    fn periodic_reconciler_observes_topology_after_runtime_projection_failure() {
        let directory = tempfile::tempdir().unwrap();
        let store = initialize_store(&directory.path().join("projection-observation.db"));
        let revision = store
            .create_initial_topology_revision(
                topology_spec("projection observation"),
                "unix-ms:1".to_string(),
                "admin".to_string(),
                "initial".to_string(),
            )
            .unwrap();
        store
            .begin_topology_apply(
                "primary",
                revision.revision_id(),
                "op-projection-observation",
                "unix-ms:2",
            )
            .unwrap();
        store
            .finish_topology_apply(
                "primary",
                revision.revision_id(),
                "op-projection-observation",
                TopologyApplyOutcome::Succeeded,
                "unix-ms:3",
            )
            .unwrap();

        let observed_at_ms = now_ms();
        store
            .put_runtime_instance(&StoredRuntimeInstance {
                node_id: "node-worker".to_string(),
                instance: RuntimeInstance {
                    deployment_id: "deployment-worker".to_string(),
                    service_id: "worker".to_string(),
                    release_version: "1.0.0".to_string(),
                    container_id: "container-worker".to_string(),
                    artifact_digest: format!("sha256:{}", "c".repeat(64)),
                    runtime_contract: orchestrator_runtime::RuntimeContract::standard_v1(),
                    runtime_policy_sha256: String::new(),
                    effective_runtime_sha256: String::new(),
                    runtime_attested: true,
                    desired_state: RuntimeDesiredState::Running,
                    observed_state: RuntimeObservedState::Running,
                    health: "HEALTHY".to_string(),
                },
                management_mode: RuntimeManagementMode::Managed,
                endpoint: "127.0.0.1:8081:worker".to_string(),
                external_probe_protocol: String::new(),
                external_probe_health_path: String::new(),
                last_observed_at_ms: observed_at_ms,
                drift_reason: String::new(),
                credential_expires_at_ms: 0,
                credential_last_success_at_ms: 0,
                credential_last_error: String::new(),
                updated_at: format!("unix-ms:{observed_at_ms}"),
            })
            .unwrap();
        store
            .put_node_runtime_facts(&StoredNodeRuntimeFacts {
                node_id: "node-worker".to_string(),
                observed_at_ms,
                received_at_ms: observed_at_ms,
                facts: json!({
                    "schema_version": 1,
                    "report_id": "projection-observation-report",
                    "inventory_complete": true
                }),
            })
            .unwrap();

        let mut binding = staged_group_binding(
            "binding-projection-observation",
            "gateway_control",
            "ACTIVE",
            false,
            revision.revision_id(),
            "op-projection-observation",
        );
        binding.state = ApiBindingState::Active;
        binding.observed_state = "ACTIVE".to_string();
        binding.health = "HEALTHY".to_string();
        store
            .replace_topology_api_bindings("primary", &[binding])
            .unwrap();

        let content_sha256 = revision.spec().content_sha256().unwrap();
        let tampered_projection_sha256 = "f".repeat(64);
        let (provider, mocks) = provider_pair(
            vec![
                observe_with_projection(
                    revision.revision_id(),
                    &content_sha256,
                    Some(&tampered_projection_sha256),
                ),
                runtime_mutation_phase(0, 200, "repair-revoke"),
                observe_with_projection(
                    revision.revision_id(),
                    &content_sha256,
                    Some(&tampered_projection_sha256),
                ),
            ],
            vec![
                observe_with_projection(
                    revision.revision_id(),
                    &content_sha256,
                    Some(&tampered_projection_sha256),
                ),
                runtime_mutation_phase(0, 503, "repair-revoke"),
                observe_with_projection(
                    revision.revision_id(),
                    &content_sha256,
                    Some(&tampered_projection_sha256),
                ),
            ],
        );

        let error = reconcile_all(&store, &provider, &NetworkProbePool::new()).unwrap_err();
        join_providers(mocks);
        assert!(
            error.contains("auth apply returned HTTP 503"),
            "runtime projection error was not returned: {error}"
        );

        let status = store.topology_status("primary").unwrap().unwrap();
        assert_eq!(status.state, TopologyReconciliationState::Degraded);
        assert_eq!(
            status.desired_revision_id.as_deref(),
            Some(revision.revision_id())
        );
        assert!(status.observed_revision_id.is_none());
        assert!(
            !status.deployments.is_empty(),
            "topology observation did not replace the apply-time placeholder Status"
        );
        assert!(status.drift.iter().any(|drift| {
            drift.resource_kind == TopologyResourceKind::Authority
                && drift.kind == TopologyDriftKind::Changed
        }));
    }

    #[test]
    fn finalize_does_not_advance_head_after_provider_report_expires() {
        assert_finalize_rejects_provider_evidence_loss(FinalizeEvidenceLoss::ManagedReport);
    }

    #[test]
    fn finalize_does_not_advance_head_after_provider_runtime_drifts() {
        assert_finalize_rejects_provider_evidence_loss(FinalizeEvidenceLoss::ManagedDrift);
    }

    #[test]
    fn finalize_does_not_advance_head_after_external_probe_expires() {
        assert_finalize_rejects_provider_evidence_loss(FinalizeEvidenceLoss::ExternalProbe);
    }

    #[derive(Clone, Copy)]
    enum FinalizeEvidenceLoss {
        ManagedReport,
        ManagedDrift,
        ExternalProbe,
    }

    fn assert_finalize_rejects_provider_evidence_loss(loss: FinalizeEvidenceLoss) {
        let directory = tempfile::tempdir().unwrap();
        let store = initialize_store(&directory.path().join("orchestrator.db"));
        store
            .put_runtime_instance(&StoredRuntimeInstance {
                node_id: "node-external".to_string(),
                instance: RuntimeInstance {
                    deployment_id: "deployment-consumer".to_string(),
                    service_id: "worker".to_string(),
                    release_version: "1.0.0".to_string(),
                    container_id: String::new(),
                    artifact_digest: format!("sha256:{}", "c".repeat(64)),
                    runtime_contract: orchestrator_runtime::RuntimeContract::standard_v1(),
                    runtime_policy_sha256: String::new(),
                    effective_runtime_sha256: String::new(),
                    runtime_attested: false,
                    desired_state: RuntimeDesiredState::Running,
                    observed_state: RuntimeObservedState::Running,
                    health: "HEALTHY".to_string(),
                },
                management_mode: RuntimeManagementMode::External,
                endpoint: "127.0.0.1:8081:worker".to_string(),
                external_probe_protocol: "http".to_string(),
                external_probe_health_path: "/health".to_string(),
                last_observed_at_ms: now_ms(),
                drift_reason: String::new(),
                credential_expires_at_ms: 0,
                credential_last_success_at_ms: 0,
                credential_last_error: String::new(),
                updated_at: "unix-ms:1".to_string(),
            })
            .unwrap();
        let revision = store
            .create_initial_topology_revision(
                topology_spec("provider evidence finalize gate"),
                "unix-ms:1".to_string(),
                "admin".to_string(),
                "initial".to_string(),
            )
            .unwrap();
        let operation_id = match loss {
            FinalizeEvidenceLoss::ManagedReport => "op-provider-report-stale",
            FinalizeEvidenceLoss::ManagedDrift => "op-provider-runtime-drift",
            FinalizeEvidenceLoss::ExternalProbe => "op-external-probe-stale",
        };
        let binding = ApiBinding {
            binding_id: "binding-worker-gateway-control".to_string(),
            requirement_name: "gateway_control".to_string(),
            api_id: "gateway.control".to_string(),
            api_version: "1.0.0".to_string(),
            consumer_deployment_id: "deployment-consumer".to_string(),
            consumer_service_id: "worker".to_string(),
            consumer_node_id: "node-external".to_string(),
            consumer_endpoint: "127.0.0.1:8081:worker".to_string(),
            provider_deployment_id: "deployment-gateway".to_string(),
            provider_service_id: "gateway".to_string(),
            provider_node_id: "node-gateway".to_string(),
            provider_endpoint: "127.0.0.1:8080:gateway".to_string(),
            provider_path: "/api/control".to_string(),
            virtual_endpoint: "/internal/apis/gateway.control".to_string(),
            protocol: "http".to_string(),
            methods: vec!["POST".to_string()],
            auth_mode: "workload".to_string(),
            provider_auth_mode: "workload".to_string(),
            permission: "gateway.control".to_string(),
            timeout_ms: Some(35_000),
            topology_id: "primary".to_string(),
            topology_revision_id: revision.revision_id().to_string(),
            link_source_endpoint: "127.0.0.1:8081:worker".to_string(),
            link_target_endpoint: "127.0.0.1:8080:gateway".to_string(),
            credential_ref: String::new(),
            credential_generation: 1,
            context_generation: 1,
            desired_state: "ACTIVE".to_string(),
            observed_state: "PENDING".to_string(),
            health: "UNKNOWN".to_string(),
            drift: Vec::new(),
            last_operation_id: operation_id.to_string(),
            state: ApiBindingState::Pending,
            optional: false,
            reason: String::new(),
            created_at: "unix-ms:1".to_string(),
            updated_at: "unix-ms:1".to_string(),
        };
        let phase_payload = |phase: &str| {
            json!({
                "topology_id": "primary",
                "revision_id": revision.revision_id(),
                "phase": phase,
                "bindings": [binding.clone()],
                "previous_bindings": []
            })
        };
        let mut operations = store.operation_store();
        let mut jobs = store.job_store();
        let mut coordinator = OperationCoordinator::new(&mut operations, &mut jobs);
        coordinator
            .plan(
                PlanOperation {
                    operation_id: operation_id.to_string(),
                    action: "topology.apply".to_string(),
                    target_type: "Topology".to_string(),
                    target_id: "primary".to_string(),
                    request: json!({"auto_enqueue": true}),
                    jobs: vec![
                        PlannedJob {
                            step_id: "prepare".to_string(),
                            node_id: CONTROL_PLANE_NODE_ID.to_string(),
                            kind: JobKind::TopologyApply,
                            depends_on: Vec::new(),
                            condition: Default::default(),
                            payload: phase_payload("PREPARE"),
                            max_attempts: 1,
                        },
                        PlannedJob {
                            step_id: "finalize".to_string(),
                            node_id: CONTROL_PLANE_NODE_ID.to_string(),
                            kind: JobKind::TopologyApply,
                            depends_on: vec!["prepare".to_string()],
                            condition: Default::default(),
                            payload: phase_payload("FINALIZE"),
                            max_attempts: 1,
                        },
                    ],
                },
                now_ms(),
            )
            .unwrap();
        coordinator.confirm(operation_id, now_ms()).unwrap();
        store
            .begin_topology_apply(
                "primary",
                revision.revision_id(),
                operation_id,
                &now_marker(),
            )
            .unwrap();
        coordinator.enqueue(operation_id, now_ms()).unwrap();
        let (provider, mocks) = provider_pair(
            vec![mutation("apply", 200), mutation("delete", 200)],
            vec![mutation("apply", 200), mutation("delete", 200)],
        );
        assert!(process_one(&store, Some(&provider)).unwrap());
        assert_eq!(
            store
                .topology_heads("primary")
                .unwrap()
                .unwrap()
                .applying_revision_id
                .as_deref(),
            Some(revision.revision_id())
        );
        match loss {
            FinalizeEvidenceLoss::ManagedReport => {
                let mut facts = store
                    .node_runtime_facts("node-gateway")
                    .unwrap()
                    .expect("provider facts");
                facts.received_at_ms =
                    now_ms().saturating_sub(crate::durable::MANAGED_RUNTIME_REPORT_STALE_MS + 1);
                store.put_node_runtime_facts(&facts).unwrap();
            }
            FinalizeEvidenceLoss::ManagedDrift => {
                let mut runtime = store
                    .runtime_instance("deployment-gateway")
                    .unwrap()
                    .expect("provider runtime");
                runtime.drift_reason = "HostConfig digest changed after PREPARE".to_string();
                store.put_runtime_instance(&runtime).unwrap();
            }
            FinalizeEvidenceLoss::ExternalProbe => {
                let mut runtime = store
                    .runtime_instance("deployment-gateway")
                    .unwrap()
                    .expect("provider runtime");
                runtime.node_id = "external".to_string();
                runtime.management_mode = RuntimeManagementMode::External;
                runtime.instance.container_id.clear();
                runtime.instance.runtime_attested = false;
                runtime.external_probe_protocol = "http".to_string();
                runtime.external_probe_health_path = "/health".to_string();
                runtime.last_observed_at_ms =
                    now_ms().saturating_sub(crate::durable::EXTERNAL_RUNTIME_PROBE_STALE_MS + 1);
                store.put_runtime_instance(&runtime).unwrap();
            }
        }
        assert!(process_one(&store, Some(&provider)).unwrap());
        join_providers(mocks);

        let heads = store.topology_heads("primary").unwrap().unwrap();
        assert!(heads.applied_revision_id.is_none());
        assert!(heads.applying_revision_id.is_none());
        let operation = store
            .operation_store()
            .get(operation_id)
            .unwrap()
            .expect("operation projection");
        assert_eq!(operation.status, DurableOperationStatus::Failed);
    }

    #[test]
    fn published_endpoint_and_link_edits_create_drafts_without_applying() {
        let directory = tempfile::tempdir().unwrap();
        let store = initialize_store(&directory.path().join("orchestrator.db"));
        let first = store
            .create_initial_topology_revision(
                topology_spec("initial"),
                "unix-ms:1".to_string(),
                "admin".to_string(),
                "initial".to_string(),
            )
            .unwrap();

        let mut worker = first
            .spec()
            .endpoints
            .iter()
            .find(|endpoint| endpoint.service_id == "worker")
            .unwrap()
            .clone();
        worker.note = "edited through endpoint action".to_string();
        let endpoint_response = api(
            &store,
            None,
            request(
                "PUT",
                "/api/v1/topologies/primary/draft/endpoints/127.0.0.1%3A8081%3Aworker",
                serde_json::to_string(&worker).unwrap(),
                Some(first.revision_id()),
                "edit-endpoint",
            ),
            "req-edit-endpoint",
        );
        assert_eq!(endpoint_response.status, 201, "{}", endpoint_response.body);
        let second_revision_id = endpoint_response.body["data"]["revision"]["revision_id"]
            .as_str()
            .unwrap()
            .to_string();
        assert_eq!(
            endpoint_response.headers.get("ETag"),
            Some(&format!("\"{second_revision_id}\""))
        );

        let stale_endpoint = api(
            &store,
            None,
            request(
                "PUT",
                "/api/v1/topologies/primary/draft/endpoints/127.0.0.1%3A8081%3Aworker",
                serde_json::to_string(&worker).unwrap(),
                Some(first.revision_id()),
                "stale-endpoint",
            ),
            "req-stale-endpoint",
        );
        assert_eq!(stale_endpoint.status, 409);
        assert_eq!(stale_endpoint.body["code"], "TOPOLOGY_REVISION_CONFLICT");

        let second = store
            .topology_revision("primary", &second_revision_id)
            .unwrap()
            .unwrap();
        let mut link = second.spec().links[0].clone();
        link.scope = "worker.admin".to_string();
        let link_response = api(
            &store,
            None,
            request(
                "PUT",
                "/api/v1/topologies/primary/draft/links/127.0.0.1%3A8080%3Agateway/127.0.0.1%3A8081%3Aworker",
                serde_json::to_string(&link).unwrap(),
                Some(&second_revision_id),
                "edit-link",
            ),
            "req-edit-link",
        );
        assert_eq!(link_response.status, 201, "{}", link_response.body);
        let third_revision_id = link_response.body["data"]["revision"]["revision_id"]
            .as_str()
            .unwrap()
            .to_string();
        let third = store
            .topology_revision("primary", &third_revision_id)
            .unwrap()
            .unwrap();
        assert_eq!(third.spec().links[0].scope, "worker.admin");

        let delete_link = api(
            &store,
            None,
            request(
                "DELETE",
                "/api/v1/topologies/primary/draft/links/127.0.0.1%3A8080%3Agateway/127.0.0.1%3A8081%3Aworker",
                "{}",
                Some(&third_revision_id),
                "delete-link",
            ),
            "req-delete-link",
        );
        assert_eq!(delete_link.status, 201, "{}", delete_link.body);
        let fourth_revision_id = delete_link.body["data"]["revision"]["revision_id"]
            .as_str()
            .unwrap()
            .to_string();
        let delete_endpoint = api(
            &store,
            None,
            request(
                "DELETE",
                "/api/v1/topologies/primary/draft/endpoints/127.0.0.1%3A8081%3Aworker",
                "{}",
                Some(&fourth_revision_id),
                "delete-endpoint",
            ),
            "req-delete-endpoint",
        );
        assert_eq!(delete_endpoint.status, 201, "{}", delete_endpoint.body);
        let fifth_revision_id = delete_endpoint.body["data"]["revision"]["revision_id"]
            .as_str()
            .unwrap();
        let fifth = store
            .topology_revision("primary", fifth_revision_id)
            .unwrap()
            .unwrap();
        assert_eq!(fifth.revision_number(), 5);
        assert!(fifth.spec().links.is_empty());
        assert!(
            fifth
                .spec()
                .endpoints
                .iter()
                .all(|endpoint| endpoint.service_id != "worker")
        );
        let heads = store.topology_heads("primary").unwrap().unwrap();
        assert!(heads.applied_revision_id.is_none());
        assert!(heads.applying_revision_id.is_none());
        let status = store.topology_status("primary").unwrap().unwrap();
        assert_eq!(status.state, TopologyReconciliationState::Draft);
        assert_eq!(
            status.desired_revision_id,
            Some(fifth_revision_id.to_string())
        );
        assert!(status.observed_revision_id.is_none());
        assert!(status.last_operation_id.is_none());
    }

    #[test]
    fn auth_failure_compensates_gateway_and_does_not_advance_applied_head() {
        let directory = tempfile::tempdir().unwrap();
        let store = initialize_store(&directory.path().join("orchestrator.db"));
        let (first, second) = seeded_second_revision(&store);
        let (provider, mocks) = provider_pair(
            vec![mutation("apply", 200), mutation("restore_previous", 200)],
            vec![mutation("apply", 500)],
        );
        let response = enqueue_revision(&store, &provider, second.revision_id(), "auth-failure");
        let operation_id = response.body["data"]["operation_id"]
            .as_str()
            .unwrap()
            .to_string();
        process_until_operation_terminal(&store, &provider, &operation_id);
        join_providers(mocks);

        let heads = store.topology_heads("primary").unwrap().unwrap();
        assert_eq!(
            heads.applied_revision_id.as_deref(),
            Some(first.revision_id())
        );
        let status = store.topology_status("primary").unwrap().unwrap();
        assert_eq!(status.state, TopologyReconciliationState::Failed);
        assert_eq!(
            status.desired_revision_id.as_deref(),
            Some(second.revision_id())
        );
        assert_eq!(
            status.observed_revision_id.as_deref(),
            Some(first.revision_id())
        );
        assert_eq!(
            store
                .operation_store()
                .get(&operation_id)
                .unwrap()
                .unwrap()
                .status,
            DurableOperationStatus::Failed
        );

        let (retry_provider, retry_mocks) = provider_pair(successful_apply(), successful_apply());
        {
            let mut operations = store.operation_store();
            let mut jobs = store.job_store();
            OperationCoordinator::new(&mut operations, &mut jobs)
                .retry(&operation_id, now_ms())
                .unwrap();
        }
        process_until_operation_terminal(&store, &retry_provider, &operation_id);
        join_providers(retry_mocks);
        let retried_heads = store.topology_heads("primary").unwrap().unwrap();
        assert_eq!(
            retried_heads.applied_revision_id.as_deref(),
            Some(second.revision_id())
        );
        assert_eq!(
            store.topology_status("primary").unwrap().unwrap().state,
            TopologyReconciliationState::InSync
        );
        assert_eq!(
            store
                .operation_store()
                .get(&operation_id)
                .unwrap()
                .unwrap()
                .status,
            DurableOperationStatus::Succeeded
        );
    }

    #[test]
    fn stale_runtime_report_keeps_route_and_repairs_tampered_projection_with_retry() {
        let directory = tempfile::tempdir().unwrap();
        let store = initialize_store(&directory.path().join("runtime-revoke.db"));
        let revision = store
            .create_initial_topology_revision(
                topology_spec("runtime revoke"),
                "unix-ms:1".to_string(),
                "admin".to_string(),
                "initial".to_string(),
            )
            .unwrap();
        store
            .begin_topology_apply(
                "primary",
                revision.revision_id(),
                "op-seed-runtime",
                "unix-ms:2",
            )
            .unwrap();
        store
            .finish_topology_apply(
                "primary",
                revision.revision_id(),
                "op-seed-runtime",
                TopologyApplyOutcome::Succeeded,
                "unix-ms:3",
            )
            .unwrap();

        let binding = ApiBinding {
            binding_id: "binding-runtime-revoke".to_string(),
            requirement_name: "worker_call".to_string(),
            api_id: "worker.call".to_string(),
            api_version: "1.0.0".to_string(),
            consumer_deployment_id: "deployment-gateway".to_string(),
            consumer_service_id: "gateway".to_string(),
            consumer_node_id: "node-gateway".to_string(),
            consumer_endpoint: "127.0.0.1:8080:gateway".to_string(),
            provider_deployment_id: "deployment-worker".to_string(),
            provider_service_id: "worker".to_string(),
            provider_node_id: "node-worker".to_string(),
            provider_endpoint: "127.0.0.1:8081:worker".to_string(),
            provider_path: "/call".to_string(),
            virtual_endpoint: "/internal/apis/worker.call".to_string(),
            protocol: "http".to_string(),
            methods: vec!["POST".to_string()],
            auth_mode: "workload".to_string(),
            provider_auth_mode: "workload".to_string(),
            permission: "worker.call".to_string(),
            timeout_ms: Some(5_000),
            topology_id: "primary".to_string(),
            topology_revision_id: revision.revision_id().to_string(),
            link_source_endpoint: "127.0.0.1:8080:gateway".to_string(),
            link_target_endpoint: "127.0.0.1:8081:worker".to_string(),
            credential_ref: String::new(),
            credential_generation: 1,
            context_generation: 1,
            desired_state: "ACTIVE".to_string(),
            observed_state: "ACTIVE".to_string(),
            health: "HEALTHY".to_string(),
            drift: Vec::new(),
            last_operation_id: "op-seed-runtime".to_string(),
            state: ApiBindingState::Active,
            optional: false,
            reason: String::new(),
            created_at: "unix-ms:3".to_string(),
            updated_at: "unix-ms:3".to_string(),
        };
        store
            .replace_topology_api_bindings("primary", std::slice::from_ref(&binding))
            .unwrap();
        let stale_at_ms = now_ms() - crate::durable::MANAGED_RUNTIME_REPORT_STALE_MS - 1;
        store
            .put_runtime_instance(&StoredRuntimeInstance {
                node_id: "node-worker".to_string(),
                instance: RuntimeInstance {
                    deployment_id: "deployment-worker".to_string(),
                    service_id: "worker".to_string(),
                    release_version: "1.0.0".to_string(),
                    container_id: "container-worker".to_string(),
                    artifact_digest: format!("sha256:{}", "b".repeat(64)),
                    runtime_contract: orchestrator_runtime::RuntimeContract::standard_v1(),
                    runtime_policy_sha256: String::new(),
                    effective_runtime_sha256: String::new(),
                    runtime_attested: true,
                    desired_state: RuntimeDesiredState::Running,
                    observed_state: RuntimeObservedState::Running,
                    health: "HEALTHY".to_string(),
                },
                management_mode: RuntimeManagementMode::Managed,
                endpoint: "127.0.0.1:8081:worker".to_string(),
                external_probe_protocol: String::new(),
                external_probe_health_path: String::new(),
                last_observed_at_ms: stale_at_ms,
                drift_reason: String::new(),
                credential_expires_at_ms: 0,
                credential_last_success_at_ms: 0,
                credential_last_error: String::new(),
                updated_at: format!("unix-ms:{stale_at_ms}"),
            })
            .unwrap();
        store
            .put_node_runtime_facts(&StoredNodeRuntimeFacts {
                node_id: "node-worker".to_string(),
                observed_at_ms: stale_at_ms,
                received_at_ms: stale_at_ms,
                facts: json!({
                    "schema_version": 1,
                    "report_id": "stale-worker-report",
                    "inventory_complete": true
                }),
            })
            .unwrap();
        let content_sha256 = revision.spec().content_sha256().unwrap();
        let seeded = runtime_projection_state(
            revision.revision_id(),
            &content_sha256,
            std::slice::from_ref(&binding),
        )
        .unwrap();
        store
            .put_state(
                RUNTIME_BINDING_PROJECTION_STATE_NAMESPACE,
                "primary",
                &seeded,
            )
            .unwrap();

        let tampered_projection_sha256 = "f".repeat(64);
        let empty_projection_sha256 = provider_projection_sha256(&[]).unwrap();
        assert_ne!(seeded.projection_sha256, tampered_projection_sha256);
        let safe_state =
            runtime_projection_state(revision.revision_id(), &content_sha256, &[]).unwrap();
        let repair_revoke_id =
            runtime_projection_operation_id("primary", &safe_state, "repair-revoke");
        let repair_grant_id = runtime_projection_operation_id("primary", &seeded, "repair-grant");
        assert_eq!(
            repair_revoke_id,
            runtime_projection_operation_id("primary", &safe_state, "repair-revoke")
        );
        assert_ne!(repair_revoke_id, repair_grant_id);

        // A same-revision/spec provider projection with a tampered effective
        // digest must first converge to empty.  If Auth rejects that revoke,
        // no desired grant is attempted and the durable desired state is not
        // rewritten.
        let (failed_provider, failed_mocks) = provider_pair(
            vec![
                observe_with_projection(
                    revision.revision_id(),
                    &content_sha256,
                    Some(&tampered_projection_sha256),
                ),
                runtime_mutation_phase(0, 200, "repair-revoke"),
            ],
            vec![
                observe_with_projection(
                    revision.revision_id(),
                    &content_sha256,
                    Some(&tampered_projection_sha256),
                ),
                runtime_mutation_phase(0, 503, "repair-revoke"),
            ],
        );
        let failure = reconcile_runtime_binding_projections(
            &store,
            Some(&failed_provider),
            Some(&BTreeSet::from(["deployment-worker".to_string()])),
            false,
        )
        .unwrap_err();
        assert!(failure.contains("auth apply returned HTTP 503"));
        join_providers(failed_mocks);
        assert_eq!(
            store
                .get_state::<RuntimeBindingProjectionState>(
                    RUNTIME_BINDING_PROJECTION_STATE_NAMESPACE,
                    "primary",
                )
                .unwrap()
                .unwrap(),
            seeded
        );

        // Retry observes the Gateway's partial empty state and replays the
        // same deterministic revoke phase before granting Auth then Gateway.
        let (retry_provider, retry_mocks) = provider_pair(
            vec![
                observe_with_projection(
                    revision.revision_id(),
                    &content_sha256,
                    Some(&empty_projection_sha256),
                ),
                runtime_mutation_phase(0, 200, "repair-revoke"),
                runtime_mutation_phase(1, 200, "repair-grant"),
            ],
            vec![
                observe_with_projection(
                    revision.revision_id(),
                    &content_sha256,
                    Some(&tampered_projection_sha256),
                ),
                runtime_mutation_phase(0, 200, "repair-revoke"),
                runtime_mutation_phase(1, 200, "repair-grant"),
            ],
        );
        reconcile_runtime_binding_projections(
            &store,
            Some(&retry_provider),
            Some(&BTreeSet::from(["deployment-worker".to_string()])),
            false,
        )
        .unwrap();
        join_providers(retry_mocks);

        // Once both providers report the same effective digest, observation
        // is a no-op and must not issue another mutation.
        let (matching_provider, matching_mocks) = provider_pair(
            vec![observe_with_projection(
                revision.revision_id(),
                &content_sha256,
                Some(&seeded.projection_sha256),
            )],
            vec![observe_with_projection(
                revision.revision_id(),
                &content_sha256,
                Some(&seeded.projection_sha256),
            )],
        );
        reconcile_runtime_binding_projections(
            &store,
            Some(&matching_provider),
            Some(&BTreeSet::from(["deployment-worker".to_string()])),
            false,
        )
        .unwrap();
        join_providers(matching_mocks);

        // A confirmed absent resource contains no stale authority. It can be
        // restored in one Auth-first/Gateway-second grant pass without the
        // preliminary empty repair phase.
        let (absent_provider, absent_mocks) = provider_pair(
            vec![
                ProviderCall::ObserveAbsent,
                runtime_mutation_phase(1, 200, "grant"),
            ],
            vec![
                ProviderCall::ObserveAbsent,
                runtime_mutation_phase(1, 200, "grant"),
            ],
        );
        reconcile_runtime_binding_projections(
            &store,
            Some(&absent_provider),
            Some(&BTreeSet::from(["deployment-worker".to_string()])),
            false,
        )
        .unwrap();
        join_providers(absent_mocks);

        let projected = store
            .get_state::<RuntimeBindingProjectionState>(
                RUNTIME_BINDING_PROJECTION_STATE_NAMESPACE,
                "primary",
            )
            .unwrap()
            .unwrap();
        assert_eq!(projected.bindings.len(), 1);
        assert!(projected.bindings.contains_key(&binding.binding_id));
        assert_eq!(projected.revision_id, revision.revision_id());
        assert_eq!(
            store
                .topology_heads("primary")
                .unwrap()
                .unwrap()
                .applied_revision_id
                .as_deref(),
            Some(revision.revision_id())
        );
    }

    #[test]
    fn group_move_normalization_drops_only_the_revoked_previous_owner() {
        let operation_id = "op-binding-owner-move";
        let mut active = staged_group_binding(
            "binding-new-owner",
            "echo",
            "ACTIVE",
            false,
            "new-topology:r1:test",
            operation_id,
        );
        active.topology_id = "new-topology".to_string();
        let mut revoked = staged_group_binding(
            "binding-old-owner",
            "echo",
            "REVOKED",
            true,
            "old-topology:r2:test",
            operation_id,
        );
        revoked.topology_id = "old-topology".to_string();
        let mut retained_tombstone = staged_group_binding(
            "binding-retained-tombstone",
            "removed_optional",
            "REVOKED",
            true,
            "old-topology:r2:test",
            operation_id,
        );
        retained_tombstone.topology_id = "old-topology".to_string();
        let mut members = vec![
            orchestrator_storage::TopologyApplyGroupMember {
                topology_id: "new-topology".to_string(),
                revision_id: "new-topology:r1:test".to_string(),
                active_bindings: activate_staged_bindings(vec![active], "unix-ms:2"),
            },
            orchestrator_storage::TopologyApplyGroupMember {
                topology_id: "old-topology".to_string(),
                revision_id: "old-topology:r2:test".to_string(),
                active_bindings: activate_staged_bindings(
                    vec![revoked, retained_tombstone],
                    "unix-ms:2",
                ),
            },
        ];

        normalize_group_binding_moves(&mut members);

        assert_eq!(members[0].active_bindings.len(), 1);
        assert_eq!(members[0].active_bindings[0].state, ApiBindingState::Active);
        assert_eq!(members[1].active_bindings.len(), 1);
        assert_eq!(
            members[1].active_bindings[0].requirement_name,
            "removed_optional"
        );
        assert_eq!(
            members[1].active_bindings[0].state,
            ApiBindingState::Revoked
        );
    }

    #[test]
    fn grouped_finalize_atomically_retains_active_sibling_and_revoked_tombstone() {
        let directory = tempfile::tempdir().unwrap();
        let store = initialize_store(&directory.path().join("group-mixed-binding.db"));
        let observed_at_ms = now_ms();
        store
            .put_runtime_instance(&StoredRuntimeInstance {
                node_id: "node-worker".to_string(),
                instance: RuntimeInstance {
                    deployment_id: "deployment-worker".to_string(),
                    service_id: "worker".to_string(),
                    release_version: "1.0.0".to_string(),
                    container_id: "container-worker".to_string(),
                    artifact_digest: format!("sha256:{}", "c".repeat(64)),
                    runtime_contract: orchestrator_runtime::RuntimeContract::standard_v1(),
                    runtime_policy_sha256: String::new(),
                    effective_runtime_sha256: String::new(),
                    runtime_attested: true,
                    desired_state: RuntimeDesiredState::Running,
                    observed_state: RuntimeObservedState::Running,
                    health: "HEALTHY".to_string(),
                },
                management_mode: RuntimeManagementMode::Managed,
                endpoint: "127.0.0.1:8081:worker".to_string(),
                external_probe_protocol: String::new(),
                external_probe_health_path: String::new(),
                last_observed_at_ms: observed_at_ms,
                drift_reason: String::new(),
                credential_expires_at_ms: 0,
                credential_last_success_at_ms: 0,
                credential_last_error: String::new(),
                updated_at: "unix-ms:1".to_string(),
            })
            .unwrap();
        store
            .put_node_runtime_facts(&StoredNodeRuntimeFacts {
                node_id: "node-worker".to_string(),
                observed_at_ms,
                received_at_ms: observed_at_ms,
                facts: json!({
                    "schema_version": 1,
                    "report_id": "group-mixed-binding-worker",
                    "inventory_complete": true
                }),
            })
            .unwrap();
        let revision = store
            .create_initial_topology_revision(
                topology_spec("mixed active and revoked bindings"),
                "unix-ms:1".to_string(),
                "admin".to_string(),
                "initial".to_string(),
            )
            .unwrap();
        let operation_id = "op-group-mixed-binding";
        let retained = staged_group_binding(
            "binding-retained-permission",
            "permission_check",
            "ACTIVE",
            false,
            revision.revision_id(),
            operation_id,
        );
        let revoked = staged_group_binding(
            "binding-revoked-echo",
            "echo",
            "REVOKED",
            true,
            revision.revision_id(),
            operation_id,
        );
        let staged_bindings = vec![retained, revoked];
        store
            .replace_topology_api_bindings("primary", &staged_bindings)
            .unwrap();
        store
            .begin_topology_apply("primary", revision.revision_id(), operation_id, "unix-ms:2")
            .unwrap();
        {
            let mut operations = store.operation_store();
            let mut jobs = store.job_store();
            let mut coordinator = OperationCoordinator::new(&mut operations, &mut jobs);
            coordinator
                .plan(
                    PlanOperation {
                        operation_id: operation_id.to_string(),
                        action: "topology.apply-group".to_string(),
                        target_type: "Topology".to_string(),
                        target_id: "primary".to_string(),
                        request: json!({"auto_enqueue": true}),
                        jobs: vec![PlannedJob {
                            step_id: "topology-binding-finalize-group".to_string(),
                            node_id: CONTROL_PLANE_NODE_ID.to_string(),
                            kind: JobKind::TopologyApply,
                            depends_on: vec![],
                            condition: Default::default(),
                            payload: json!({
                                "phase": "FINALIZE_GROUP",
                                "group": [{
                                    "topology_id": "primary",
                                    "revision_id": revision.revision_id(),
                                }]
                            }),
                            max_attempts: 1,
                        }],
                    },
                    1,
                )
                .unwrap();
            coordinator.confirm(operation_id, 2).unwrap();
            coordinator.enqueue(operation_id, 3).unwrap();
        }
        let content_sha256 = revision.spec().content_sha256().unwrap();
        let projection_sha256 = provider_projection_sha256(&staged_bindings).unwrap();
        let calls = vec![observe_with_projection(
            revision.revision_id(),
            &content_sha256,
            Some(&projection_sha256),
        )];
        let (provider, mocks) = provider_pair(calls.clone(), calls);

        assert!(process_one(&store, Some(&provider)).unwrap());
        join_providers(mocks);

        let heads = store.topology_heads("primary").unwrap().unwrap();
        assert_eq!(
            heads.applied_revision_id.as_deref(),
            Some(revision.revision_id())
        );
        let persisted = store.api_bindings_for_topology("primary").unwrap();
        assert_eq!(persisted.len(), 2);
        let retained = persisted
            .iter()
            .find(|binding| binding.requirement_name == "permission_check")
            .unwrap();
        assert_eq!(retained.state, ApiBindingState::Active);
        assert_eq!(retained.desired_state, "ACTIVE");
        let revoked = persisted
            .iter()
            .find(|binding| binding.requirement_name == "echo")
            .unwrap();
        assert_eq!(revoked.state, ApiBindingState::Revoked);
        assert_eq!(revoked.desired_state, "REVOKED");
        assert_eq!(
            revoked.credential_generation,
            retained.credential_generation
        );
        assert_eq!(
            store
                .operation_store()
                .get(operation_id)
                .unwrap()
                .unwrap()
                .status,
            DurableOperationStatus::Succeeded
        );
    }

    #[test]
    fn grouped_finalize_provider_rejection_advances_no_applied_head() {
        let directory = tempfile::tempdir().unwrap();
        let store = initialize_store(&directory.path().join("orchestrator.db"));
        let primary = store
            .create_initial_topology_revision(
                topology_spec("primary candidate"),
                "unix-ms:1".to_string(),
                "admin".to_string(),
                "initial".to_string(),
            )
            .unwrap();
        let mut secondary_spec = topology_spec("secondary candidate");
        secondary_spec.topology_id = "secondary".to_string();
        let secondary = store
            .create_initial_topology_revision(
                secondary_spec,
                "unix-ms:1".to_string(),
                "admin".to_string(),
                "initial".to_string(),
            )
            .unwrap();
        let operation_id = "op-group-finalize-rejected";
        for (topology_id, revision_id) in [
            ("primary", primary.revision_id()),
            ("secondary", secondary.revision_id()),
        ] {
            store
                .begin_topology_apply(topology_id, revision_id, operation_id, "unix-ms:2")
                .unwrap();
        }
        {
            let mut operations = store.operation_store();
            let mut jobs = store.job_store();
            let mut coordinator = OperationCoordinator::new(&mut operations, &mut jobs);
            coordinator
                .plan(
                    PlanOperation {
                        operation_id: operation_id.to_string(),
                        action: "topology.apply-group".to_string(),
                        target_type: "Topology".to_string(),
                        target_id: "primary".to_string(),
                        request: json!({"auto_enqueue": true}),
                        jobs: vec![PlannedJob {
                            step_id: "topology-binding-finalize-group".to_string(),
                            node_id: CONTROL_PLANE_NODE_ID.to_string(),
                            kind: JobKind::TopologyApply,
                            depends_on: vec![],
                            condition: Default::default(),
                            payload: json!({
                                "phase": "FINALIZE_GROUP",
                                "group": [
                                    {
                                        "topology_id": "primary",
                                        "revision_id": primary.revision_id(),
                                    },
                                    {
                                        "topology_id": "secondary",
                                        "revision_id": secondary.revision_id(),
                                    }
                                ]
                            }),
                            max_attempts: 1,
                        }],
                    },
                    1,
                )
                .unwrap();
            coordinator.confirm(operation_id, 2).unwrap();
            coordinator.enqueue(operation_id, 3).unwrap();
        }
        let wrong_revision = "primary:r999:0123456789abcdef";
        let wrong_hash = "0".repeat(64);
        let (provider, mocks) = provider_pair(
            vec![observe(wrong_revision, &wrong_hash)],
            vec![observe(wrong_revision, &wrong_hash)],
        );
        assert!(process_one(&store, Some(&provider)).unwrap());
        join_providers(mocks);

        for topology_id in ["primary", "secondary"] {
            let heads = store.topology_heads(topology_id).unwrap().unwrap();
            assert!(heads.applied_revision_id.is_none());
            assert_eq!(heads.applying_operation_id.as_deref(), Some(operation_id));
        }
        assert_eq!(
            store
                .operation_store()
                .get(operation_id)
                .unwrap()
                .unwrap()
                .status,
            DurableOperationStatus::Failed
        );
    }

    #[test]
    fn failed_gateway_compensation_is_durable_degraded_needs_attention() {
        let directory = tempfile::tempdir().unwrap();
        let store = initialize_store(&directory.path().join("orchestrator.db"));
        let (first, second) = seeded_second_revision(&store);
        let (provider, mocks) = provider_pair(
            vec![mutation("apply", 200), mutation("restore_previous", 500)],
            vec![mutation("apply", 500)],
        );
        let response = enqueue_revision(
            &store,
            &provider,
            second.revision_id(),
            "compensation-failure",
        );
        let operation_id = response.body["data"]["operation_id"]
            .as_str()
            .unwrap()
            .to_string();
        assert!(process_one(&store, Some(&provider)).unwrap());
        join_providers(mocks);

        let heads = store.topology_heads("primary").unwrap().unwrap();
        assert_eq!(
            heads.applied_revision_id.as_deref(),
            Some(first.revision_id())
        );
        assert!(heads.applying_revision_id.is_none());
        assert_eq!(
            store.topology_status("primary").unwrap().unwrap().state,
            TopologyReconciliationState::Degraded
        );
        assert_eq!(
            store
                .operation_store()
                .get(&operation_id)
                .unwrap()
                .unwrap()
                .status,
            DurableOperationStatus::NeedsAttention
        );
    }

    #[test]
    fn unknown_gateway_result_is_compensated_before_failed_projection() {
        let directory = tempfile::tempdir().unwrap();
        let store = initialize_store(&directory.path().join("orchestrator.db"));
        let (first, second) = seeded_second_revision(&store);
        let (provider, mocks) = provider_pair(
            vec![mutation("apply", 202), mutation("restore_previous", 200)],
            vec![],
        );
        enqueue_revision(&store, &provider, second.revision_id(), "gateway-unknown");
        assert!(process_one(&store, Some(&provider)).unwrap());
        join_providers(mocks);

        let heads = store.topology_heads("primary").unwrap().unwrap();
        assert_eq!(
            heads.applied_revision_id.as_deref(),
            Some(first.revision_id())
        );
        assert_eq!(
            store.topology_status("primary").unwrap().unwrap().state,
            TopologyReconciliationState::Failed
        );
    }

    #[test]
    fn expired_provider_lease_recovers_to_needs_attention_without_blind_replay() {
        let directory = tempfile::tempdir().unwrap();
        let database_path = directory.path().join("orchestrator.db");
        let store = initialize_store(&database_path);
        let first = store
            .create_initial_topology_revision(
                topology_spec("initial"),
                "unix-ms:1".to_string(),
                "admin".to_string(),
                "initial".to_string(),
            )
            .unwrap();
        let (provider, mocks) = provider_pair(vec![], vec![]);
        let response = enqueue_revision(&store, &provider, first.revision_id(), "crash-recovery");
        let operation_id = response.body["data"]["operation_id"]
            .as_str()
            .unwrap()
            .to_string();
        let claim_at = now_ms() + 1_000;
        let mut jobs = store.job_store();
        let leased = jobs
            .claim(ClaimRequest {
                node_id: CONTROL_PLANE_NODE_ID.to_string(),
                instance_id: "crashed-control-plane".to_string(),
                lease_token: "lease-before-crash".to_string(),
                now_ms: claim_at,
                lease_ms: DEFAULT_LEASE_MS,
            })
            .unwrap()
            .unwrap();
        assert_eq!(leased.kind, JobKind::TopologyApply);
        drop(jobs);
        drop(store);

        let restarted = reopen_store(&database_path);
        recover_expired(&restarted, claim_at + DEFAULT_LEASE_MS + 1).unwrap();
        join_providers(mocks);
        let heads = restarted.topology_heads("primary").unwrap().unwrap();
        assert!(heads.applying_revision_id.is_none());
        assert!(heads.applied_revision_id.is_none());
        let status = restarted.topology_status("primary").unwrap().unwrap();
        assert_eq!(status.state, TopologyReconciliationState::Degraded);
        assert!(status.drift.iter().any(|drift| {
            drift
                .detail
                .contains("lease expired with an unproven provider outcome")
        }));
        assert_eq!(
            restarted
                .operation_store()
                .get(&operation_id)
                .unwrap()
                .unwrap()
                .status,
            DurableOperationStatus::NeedsAttention
        );
    }

    #[test]
    fn expired_group_finalize_lease_releases_every_member_as_degraded() {
        let directory = tempfile::tempdir().unwrap();
        let database_path = directory.path().join("orchestrator.db");
        let store = initialize_store(&database_path);
        let primary = store
            .create_initial_topology_revision(
                topology_spec("primary candidate"),
                "unix-ms:1".to_string(),
                "admin".to_string(),
                "initial".to_string(),
            )
            .unwrap();
        let mut secondary_spec = topology_spec("secondary candidate");
        secondary_spec.topology_id = "secondary".to_string();
        let secondary = store
            .create_initial_topology_revision(
                secondary_spec,
                "unix-ms:1".to_string(),
                "admin".to_string(),
                "initial".to_string(),
            )
            .unwrap();
        let operation_id = "op-group-finalize-expired";
        for (topology_id, revision_id) in [
            ("primary", primary.revision_id()),
            ("secondary", secondary.revision_id()),
        ] {
            store
                .begin_topology_apply(topology_id, revision_id, operation_id, "unix-ms:2")
                .unwrap();
        }
        {
            let mut operations = store.operation_store();
            let mut jobs = store.job_store();
            let mut coordinator = OperationCoordinator::new(&mut operations, &mut jobs);
            coordinator
                .plan(
                    PlanOperation {
                        operation_id: operation_id.to_string(),
                        action: "topology.apply-group".to_string(),
                        target_type: "Topology".to_string(),
                        target_id: "primary".to_string(),
                        request: json!({"auto_enqueue": true}),
                        jobs: vec![PlannedJob {
                            step_id: "topology-binding-finalize-group".to_string(),
                            node_id: CONTROL_PLANE_NODE_ID.to_string(),
                            kind: JobKind::TopologyApply,
                            depends_on: vec![],
                            condition: Default::default(),
                            payload: json!({
                                "phase": "FINALIZE_GROUP",
                                "group": [
                                    {
                                        "topology_id": "primary",
                                        "revision_id": primary.revision_id(),
                                    },
                                    {
                                        "topology_id": "secondary",
                                        "revision_id": secondary.revision_id(),
                                    }
                                ]
                            }),
                            max_attempts: 1,
                        }],
                    },
                    1,
                )
                .unwrap();
            coordinator.confirm(operation_id, 2).unwrap();
            coordinator.enqueue(operation_id, 3).unwrap();
        }
        let claim_at = now_ms() + 1_000;
        let mut jobs = store.job_store();
        let leased = jobs
            .claim(ClaimRequest {
                node_id: CONTROL_PLANE_NODE_ID.to_string(),
                instance_id: "crashed-control-plane".to_string(),
                lease_token: "group-lease-before-crash".to_string(),
                now_ms: claim_at,
                lease_ms: DEFAULT_LEASE_MS,
            })
            .unwrap()
            .unwrap();
        assert_eq!(leased.kind, JobKind::TopologyApply);
        drop(jobs);
        drop(store);

        let restarted = reopen_store(&database_path);
        recover_expired(&restarted, claim_at + DEFAULT_LEASE_MS + 1).unwrap();
        for topology_id in ["primary", "secondary"] {
            let heads = restarted.topology_heads(topology_id).unwrap().unwrap();
            assert!(heads.applying_revision_id.is_none());
            assert!(heads.applied_revision_id.is_none());
            let status = restarted.topology_status(topology_id).unwrap().unwrap();
            assert_eq!(status.state, TopologyReconciliationState::Degraded);
            assert!(status.drift.iter().any(|drift| {
                drift
                    .detail
                    .contains("lease expired with an unproven grouped provider outcome")
            }));
        }
        assert_eq!(
            restarted
                .operation_store()
                .get(operation_id)
                .unwrap()
                .unwrap()
                .status,
            DurableOperationStatus::NeedsAttention
        );
    }

    #[test]
    fn expired_group_finalize_with_committed_heads_recovers_as_succeeded() {
        let directory = tempfile::tempdir().unwrap();
        let database_path = directory.path().join("orchestrator.db");
        let store = initialize_store(&database_path);
        let primary = store
            .create_initial_topology_revision(
                topology_spec("primary committed"),
                "unix-ms:1".to_string(),
                "admin".to_string(),
                "initial".to_string(),
            )
            .unwrap();
        let mut secondary_spec = topology_spec("secondary committed");
        secondary_spec.topology_id = "secondary".to_string();
        let secondary = store
            .create_initial_topology_revision(
                secondary_spec,
                "unix-ms:1".to_string(),
                "admin".to_string(),
                "initial".to_string(),
            )
            .unwrap();
        let operation_id = "op-group-finalize-committed-before-complete";
        let group = vec![
            orchestrator_storage::TopologyApplyGroupMember {
                topology_id: "primary".to_string(),
                revision_id: primary.revision_id().to_string(),
                active_bindings: Vec::new(),
            },
            orchestrator_storage::TopologyApplyGroupMember {
                topology_id: "secondary".to_string(),
                revision_id: secondary.revision_id().to_string(),
                active_bindings: Vec::new(),
            },
        ];
        for member in &group {
            store
                .begin_topology_apply(
                    &member.topology_id,
                    &member.revision_id,
                    operation_id,
                    "unix-ms:2",
                )
                .unwrap();
        }
        {
            let mut operations = store.operation_store();
            let mut jobs = store.job_store();
            let mut coordinator = OperationCoordinator::new(&mut operations, &mut jobs);
            coordinator
                .plan(
                    PlanOperation {
                        operation_id: operation_id.to_string(),
                        action: "topology.apply-group".to_string(),
                        target_type: "Topology".to_string(),
                        target_id: "primary".to_string(),
                        request: json!({"auto_enqueue": true}),
                        jobs: vec![PlannedJob {
                            step_id: "topology-binding-finalize-group".to_string(),
                            node_id: CONTROL_PLANE_NODE_ID.to_string(),
                            kind: JobKind::TopologyApply,
                            depends_on: vec![],
                            condition: Default::default(),
                            payload: json!({
                                "phase": "FINALIZE_GROUP",
                                "group": group.iter().map(|member| json!({
                                    "topology_id": member.topology_id,
                                    "revision_id": member.revision_id,
                                })).collect::<Vec<_>>(),
                            }),
                            max_attempts: 1,
                        }],
                    },
                    1,
                )
                .unwrap();
            coordinator.confirm(operation_id, 2).unwrap();
            coordinator.enqueue(operation_id, 3).unwrap();
        }
        let claim_at = now_ms() + 1_000;
        let mut jobs = store.job_store();
        let leased = jobs
            .claim(ClaimRequest {
                node_id: CONTROL_PLANE_NODE_ID.to_string(),
                instance_id: "committed-control-plane".to_string(),
                lease_token: "group-committed-lease".to_string(),
                now_ms: claim_at,
                lease_ms: DEFAULT_LEASE_MS,
            })
            .unwrap()
            .unwrap();
        store
            .finish_topology_apply_group_fenced(
                &group,
                operation_id,
                "unix-ms:3",
                &leased.job_id,
                "group-committed-lease",
                claim_at + 1,
            )
            .unwrap();
        // Simulate a crash after the atomic head commit but before Job
        // completion and Operation projection.
        drop(jobs);
        drop(store);

        let restarted = reopen_store(&database_path);
        recover_expired(&restarted, claim_at + DEFAULT_LEASE_MS + 1).unwrap();
        let operation = restarted
            .operation_store()
            .get(operation_id)
            .unwrap()
            .unwrap();
        assert_eq!(operation.status, DurableOperationStatus::Succeeded);
        let job_id = operation.job_bindings[0].job_id.clone();
        assert_eq!(
            restarted.job_store().get(&job_id).unwrap().unwrap().status,
            JobStatus::Succeeded
        );
        for member in &group {
            let heads = restarted
                .topology_heads(&member.topology_id)
                .unwrap()
                .unwrap();
            assert_eq!(
                heads.applied_revision_id.as_deref(),
                Some(member.revision_id.as_str())
            );
            assert!(heads.applying_revision_id.is_none());
            assert_eq!(
                restarted
                    .topology_status(&member.topology_id)
                    .unwrap()
                    .unwrap()
                    .state,
                TopologyReconciliationState::InSync
            );
        }
    }

    #[test]
    fn periodic_projection_repair_closes_terminal_job_crash_window() {
        let directory = tempfile::tempdir().unwrap();
        let store = initialize_store(&directory.path().join("orchestrator.db"));
        let operation_id = "op-terminal-before-projection";
        {
            let mut operations = store.operation_store();
            let mut jobs = store.job_store();
            let mut coordinator = OperationCoordinator::new(&mut operations, &mut jobs);
            coordinator
                .plan(
                    PlanOperation {
                        operation_id: operation_id.to_string(),
                        action: "diagnostic.projection-repair".to_string(),
                        target_type: "Diagnostic".to_string(),
                        target_id: "projection-repair".to_string(),
                        request: json!({"auto_enqueue": true}),
                        jobs: vec![PlannedJob {
                            step_id: "probe".to_string(),
                            node_id: CONTROL_PLANE_NODE_ID.to_string(),
                            kind: JobKind::Health,
                            depends_on: vec![],
                            condition: Default::default(),
                            payload: json!({}),
                            max_attempts: 1,
                        }],
                    },
                    1,
                )
                .unwrap();
            coordinator.confirm(operation_id, 2).unwrap();
            coordinator.enqueue(operation_id, 3).unwrap();
        }
        let mut jobs = store.job_store();
        let leased = jobs
            .claim(ClaimRequest {
                node_id: CONTROL_PLANE_NODE_ID.to_string(),
                instance_id: "projection-repair-test".to_string(),
                lease_token: "projection-repair-lease".to_string(),
                now_ms: 4,
                lease_ms: DEFAULT_LEASE_MS,
            })
            .unwrap()
            .unwrap();
        jobs.complete(CompleteRequest {
            job_id: leased.job_id,
            lease_token: "projection-repair-lease".to_string(),
            status: CompletionStatus::Succeeded,
            result: json!({"healthy": true}),
            error_message: String::new(),
            now_ms: 5,
            events: vec![],
        })
        .unwrap();
        drop(jobs);

        assert_eq!(
            store
                .operation_store()
                .get(operation_id)
                .unwrap()
                .unwrap()
                .status,
            DurableOperationStatus::Running
        );
        repair_recoverable_operation_projections(&store, 6).unwrap();
        assert_eq!(
            store
                .operation_store()
                .get(operation_id)
                .unwrap()
                .unwrap()
                .status,
            DurableOperationStatus::Succeeded
        );
    }

    #[test]
    fn stalled_control_plane_job_stops_heartbeating_and_fails_closed() {
        let directory = tempfile::tempdir().unwrap();
        let store = initialize_store(&directory.path().join("orchestrator.db"));
        let claimed_at = now_ms();
        let mut jobs = store.job_store();
        jobs.enqueue(
            NewJob {
                job_id: "job-stalled-heartbeat".to_string(),
                operation_id: "op-stalled-heartbeat".to_string(),
                node_id: CONTROL_PLANE_NODE_ID.to_string(),
                kind: JobKind::Health,
                payload: json!({}),
                idempotency_key: "stalled-heartbeat".to_string(),
                max_attempts: 1,
            },
            claimed_at,
        )
        .unwrap();
        let claimed = jobs
            .claim(ClaimRequest {
                node_id: CONTROL_PLANE_NODE_ID.to_string(),
                instance_id: "stalled-heartbeat-test".to_string(),
                lease_token: "stalled-heartbeat-lease".to_string(),
                now_ms: claimed_at + 1,
                lease_ms: DEFAULT_LEASE_MS,
            })
            .unwrap()
            .unwrap();
        let guard = ControlPlaneLeaseHeartbeat::start_with_timing(
            store.clone(),
            claimed.job_id,
            "stalled-heartbeat-lease".to_string(),
            claimed.lease_expires_at_ms.unwrap(),
            Duration::from_millis(5),
            15,
        )
        .unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        while !guard.state.lost.load(Ordering::Acquire) && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(5));
        }

        assert!(guard.state.lost.load(Ordering::Acquire));
        assert!(guard.checkpoint(&mut jobs).is_err());
    }

    #[test]
    fn periodic_projection_repair_resumes_enqueue_and_cancel_boundaries() {
        let directory = tempfile::tempdir().unwrap();
        let store = initialize_store(&directory.path().join("orchestrator.db"));
        let operation_id = "op-repair-transition-boundaries";
        {
            let mut operations = store.operation_store();
            let mut jobs = store.job_store();
            let mut coordinator = OperationCoordinator::new(&mut operations, &mut jobs);
            coordinator
                .plan(
                    PlanOperation {
                        operation_id: operation_id.to_string(),
                        action: "diagnostic.transition-repair".to_string(),
                        target_type: "Diagnostic".to_string(),
                        target_id: "transition-repair".to_string(),
                        request: json!({"auto_enqueue": true}),
                        jobs: vec![PlannedJob {
                            step_id: "probe".to_string(),
                            node_id: CONTROL_PLANE_NODE_ID.to_string(),
                            kind: JobKind::Health,
                            depends_on: vec![],
                            condition: Default::default(),
                            payload: json!({}),
                            max_attempts: 1,
                        }],
                    },
                    1,
                )
                .unwrap();
            coordinator.confirm(operation_id, 2).unwrap();
        }

        // Emulate a crash after enqueue persisted ENQUEUING but before the
        // deterministic child Job was materialized.
        {
            let mut operations = store.operation_store();
            let mut enqueuing = operations.get(operation_id).unwrap().unwrap();
            let expected_revision = enqueuing.revision;
            enqueuing.status = DurableOperationStatus::Enqueuing;
            enqueuing.revision += 1;
            enqueuing.updated_at_ms = 3;
            operations
                .compare_and_swap(expected_revision, enqueuing)
                .unwrap();
        }
        repair_recoverable_operation_projections(&store, 4).unwrap();
        let running = store.operation_store().get(operation_id).unwrap().unwrap();
        assert_eq!(running.status, DurableOperationStatus::Running);
        assert_eq!(running.job_bindings.len(), 1);

        // Emulate a crash after CANCELLING was persisted but before the
        // queued child received its idempotent cancellation request.
        {
            let mut operations = store.operation_store();
            let mut cancelling = operations.get(operation_id).unwrap().unwrap();
            let expected_revision = cancelling.revision;
            cancelling.status = DurableOperationStatus::Cancelling;
            cancelling.revision += 1;
            cancelling.updated_at_ms = 5;
            operations
                .compare_and_swap(expected_revision, cancelling)
                .unwrap();
        }
        repair_recoverable_operation_projections(&store, 6).unwrap();
        assert_eq!(
            store
                .operation_store()
                .get(operation_id)
                .unwrap()
                .unwrap()
                .status,
            DurableOperationStatus::Cancelled
        );
    }

    #[test]
    fn cancelling_a_queued_apply_releases_topology_ownership() {
        let directory = tempfile::tempdir().unwrap();
        let store = initialize_store(&directory.path().join("orchestrator.db"));
        let first = store
            .create_initial_topology_revision(
                topology_spec("cancelled"),
                "unix-ms:1".to_string(),
                "admin".to_string(),
                "initial".to_string(),
            )
            .unwrap();
        let (provider, mocks) = provider_pair(vec![], vec![]);
        let response = enqueue_revision(&store, &provider, first.revision_id(), "cancel-queued");
        let operation_id = response.body["data"]["operation_id"]
            .as_str()
            .unwrap()
            .to_string();
        {
            let mut operations = store.operation_store();
            let mut jobs = store.job_store();
            let cancelled = OperationCoordinator::new(&mut operations, &mut jobs)
                .cancel(&operation_id, now_ms())
                .unwrap();
            assert_eq!(cancelled.status, DurableOperationStatus::Cancelled);
        }
        recover_terminal_topology_applies(&store).unwrap();
        join_providers(mocks);
        let heads = store.topology_heads("primary").unwrap().unwrap();
        assert!(heads.applying_revision_id.is_none());
        assert!(heads.applied_revision_id.is_none());
        assert_eq!(
            store.topology_status("primary").unwrap().unwrap().state,
            TopologyReconciliationState::Failed
        );
    }

    fn initialize_store(database_path: &Path) -> DurableStore {
        let mut sqlite = SqliteOrchestratorStore::open(database_path).unwrap();
        for (service_id, port, link_probe) in
            [("gateway", 8080_u16, true), ("worker", 8081_u16, false)]
        {
            let release = release_manifest(service_id, port, link_probe);
            let source_url = release.source.url.clone();
            sqlite
                .register_service_release_atomic(
                    service_manifest_from_release(&release, &source_url).unwrap(),
                    ServiceRelease {
                        service_name: service_id.to_string(),
                        version: release.version.clone(),
                        release_url: source_url,
                        manifest: serde_json::to_value(&release).unwrap(),
                        checksum: release.source.checksum.clone(),
                        created_at: "unix-ms:1".to_string(),
                    },
                )
                .unwrap();
        }
        let durable = DurableStore::Sqlite(sqlite);
        let observed_at_ms = now_ms();
        durable
            .put_runtime_instance(&StoredRuntimeInstance {
                node_id: "node-gateway".to_string(),
                instance: RuntimeInstance {
                    deployment_id: "deployment-gateway".to_string(),
                    service_id: "gateway".to_string(),
                    release_version: "1.0.0".to_string(),
                    container_id: "container-gateway".to_string(),
                    artifact_digest: format!("sha256:{}", "b".repeat(64)),
                    runtime_contract: orchestrator_runtime::RuntimeContract::standard_v1(),
                    runtime_policy_sha256: String::new(),
                    effective_runtime_sha256: String::new(),
                    runtime_attested: true,
                    desired_state: RuntimeDesiredState::Running,
                    observed_state: RuntimeObservedState::Running,
                    health: "HEALTHY".to_string(),
                },
                management_mode: RuntimeManagementMode::Managed,
                endpoint: "127.0.0.1:8080:gateway".to_string(),
                external_probe_protocol: String::new(),
                external_probe_health_path: String::new(),
                last_observed_at_ms: observed_at_ms,
                drift_reason: String::new(),
                credential_expires_at_ms: 0,
                credential_last_success_at_ms: 0,
                credential_last_error: String::new(),
                updated_at: "unix-ms:1".to_string(),
            })
            .unwrap();
        durable
            .put_node_runtime_facts(&StoredNodeRuntimeFacts {
                node_id: "node-gateway".to_string(),
                observed_at_ms,
                received_at_ms: observed_at_ms,
                facts: json!({
                    "schema_version": 1,
                    "report_id": "topology-ga-fixture",
                    "inventory_complete": true
                }),
            })
            .unwrap();
        durable
    }

    fn reopen_store(database_path: &Path) -> DurableStore {
        DurableStore::Sqlite(SqliteOrchestratorStore::open(database_path).unwrap())
    }

    fn release_manifest(service_id: &str, port: u16, link_probe: bool) -> ServiceReleaseManifest {
        let apis = if link_probe {
            json!([{
                "api_id": "orchestrator.link-probe.v1",
                "protocol": "http",
                "port_name": "default",
                "path_prefix": "/probe",
                "methods": ["GET"],
                "visibility": "global",
                "auth_mode": "public",
                "permission": "public",
                "stability": "stable",
                "version": "v1"
            }])
        } else {
            json!([])
        };
        serde_json::from_value(json!({
            "schema_version": 1,
            "service_name": service_id,
            "version": "1.0.0",
            "description": "topology GA fixture",
            "service_type": "backend-api",
            "source": {
                "kind": "url",
                "url": format!("https://catalog.example/{service_id}/1.0.0.json"),
                "checksum": format!("sha256:{}", "a".repeat(64))
            },
            "runtime": {
                "kind": "image",
                "image": format!("registry.example/{service_id}@sha256:{}", "b".repeat(64))
            },
            "backend": {"protocol": "http", "port": port, "health_path": "/health"},
            "apis": apis
        }))
        .unwrap()
    }

    fn topology_spec(note: &str) -> TopologySpec {
        let gateway = TopologyEndpointSpec {
            endpoint: "127.0.0.1:8080:gateway".to_string(),
            service_id: "gateway".to_string(),
            protocol: "http".to_string(),
            health_path: "/health".to_string(),
            display_name: "Gateway".to_string(),
            note: note.to_string(),
            config: json!({}),
        };
        let worker = TopologyEndpointSpec {
            endpoint: "127.0.0.1:8081:worker".to_string(),
            service_id: "worker".to_string(),
            protocol: "http".to_string(),
            health_path: "/health".to_string(),
            display_name: "Worker".to_string(),
            note: String::new(),
            config: json!({}),
        };
        TopologySpec::new(
            "primary",
            gateway.endpoint.clone(),
            "private",
            vec![gateway.clone(), worker.clone()],
            vec![TopologyLinkSpec {
                source_endpoint: gateway.endpoint,
                target_endpoint: worker.endpoint,
                protocol: "http".to_string(),
                auth_mode: "internal".to_string(),
                scope: "worker.invoke".to_string(),
                enabled: true,
                config_ref: String::new(),
                secret_ref: String::new(),
                policy: json!({}),
                api_bindings: Vec::new(),
            }],
        )
        .unwrap()
    }

    fn staged_group_binding(
        binding_id: &str,
        requirement_name: &str,
        desired_state: &str,
        optional: bool,
        revision_id: &str,
        operation_id: &str,
    ) -> ApiBinding {
        ApiBinding {
            binding_id: binding_id.to_string(),
            requirement_name: requirement_name.to_string(),
            api_id: format!("fixture.{requirement_name}"),
            api_version: "1.0.0".to_string(),
            consumer_deployment_id: "deployment-worker".to_string(),
            consumer_service_id: "worker".to_string(),
            consumer_node_id: "node-worker".to_string(),
            consumer_endpoint: "127.0.0.1:8081:worker".to_string(),
            provider_deployment_id: "deployment-gateway".to_string(),
            provider_service_id: "gateway".to_string(),
            provider_node_id: "node-gateway".to_string(),
            provider_endpoint: "127.0.0.1:8080:gateway".to_string(),
            provider_path: format!("/{requirement_name}"),
            virtual_endpoint: format!("/internal/apis/fixture.{requirement_name}"),
            protocol: "http".to_string(),
            methods: vec!["GET".to_string()],
            auth_mode: "workload".to_string(),
            provider_auth_mode: "workload".to_string(),
            permission: format!("fixture.{requirement_name}"),
            timeout_ms: Some(35_000),
            topology_id: "primary".to_string(),
            topology_revision_id: revision_id.to_string(),
            link_source_endpoint: "127.0.0.1:8081:worker".to_string(),
            link_target_endpoint: "127.0.0.1:8080:gateway".to_string(),
            credential_ref: String::new(),
            credential_generation: 2,
            context_generation: 2,
            desired_state: desired_state.to_string(),
            observed_state: "PENDING".to_string(),
            health: "UNKNOWN".to_string(),
            drift: Vec::new(),
            last_operation_id: operation_id.to_string(),
            state: ApiBindingState::Pending,
            optional,
            reason: String::new(),
            created_at: "unix-ms:1".to_string(),
            updated_at: "unix-ms:1".to_string(),
        }
    }

    fn seeded_second_revision(store: &DurableStore) -> (TopologyRevision, TopologyRevision) {
        let first = store
            .create_initial_topology_revision(
                topology_spec("proven"),
                "unix-ms:1".to_string(),
                "admin".to_string(),
                "initial".to_string(),
            )
            .unwrap();
        store
            .begin_topology_apply("primary", first.revision_id(), "op-seed", "unix-ms:2")
            .unwrap();
        store
            .finish_topology_apply(
                "primary",
                first.revision_id(),
                "op-seed",
                TopologyApplyOutcome::Succeeded,
                "unix-ms:3",
            )
            .unwrap();
        let second = store
            .create_next_topology_revision(
                "primary",
                first.revision_id(),
                topology_spec("candidate"),
                "unix-ms:4".to_string(),
                "admin".to_string(),
                "edit".to_string(),
            )
            .unwrap();
        (first, second)
    }

    fn enqueue_revision(
        store: &DurableStore,
        provider: &TopologyProviderSaga,
        revision_id: &str,
        idempotency_key: &str,
    ) -> ApiResponse {
        let response = api(
            store,
            Some(provider),
            request(
                "POST",
                "/api/v1/topologies/primary:apply",
                "{}",
                Some(revision_id),
                idempotency_key,
            ),
            &format!("req-{idempotency_key}"),
        );
        assert_eq!(response.status, 202, "{}", response.body);
        response
    }

    fn request(
        method: &str,
        path: &str,
        body: impl Into<String>,
        if_match: Option<&str>,
        idempotency_key: &str,
    ) -> ApiRequest {
        let mut headers = BTreeMap::from([
            ("content-type".to_string(), "application/json".to_string()),
            ("idempotency-key".to_string(), idempotency_key.to_string()),
            ("x-actor-id".to_string(), "topology-ga".to_string()),
        ]);
        if let Some(revision_id) = if_match {
            headers.insert("if-match".to_string(), format!("\"{revision_id}\""));
        }
        ApiRequest {
            method: method.to_string(),
            path: path.to_string(),
            headers,
            body: body.into(),
        }
    }

    fn api(
        store: &DurableStore,
        provider: Option<&TopologyProviderSaga>,
        request: ApiRequest,
        request_id: &str,
    ) -> ApiResponse {
        crate::topology_api::route(Some(store), provider, &request, request_id)
            .expect("topology route")
    }

    fn mutation(action: &'static str, status: u16) -> ProviderCall {
        ProviderCall::Mutation {
            action,
            status,
            expected_routes: None,
            expected_operation_phase: None,
        }
    }

    fn runtime_mutation_phase(
        expected_routes: usize,
        status: u16,
        expected_operation_phase: &'static str,
    ) -> ProviderCall {
        ProviderCall::Mutation {
            action: "apply",
            status,
            expected_routes: Some(expected_routes),
            expected_operation_phase: Some(expected_operation_phase),
        }
    }

    fn successful_apply() -> Vec<ProviderCall> {
        vec![mutation("apply", 200), ProviderCall::ObserveApplied]
    }

    fn observe(revision_id: &str, content_sha256: &str) -> ProviderCall {
        observe_with_projection(
            revision_id,
            content_sha256,
            Some("fa9d28278a0d02b19bfebeae5afd5aa6dde1c685d8396acc8defe8832848865c"),
        )
    }

    fn observe_with_projection(
        revision_id: &str,
        content_sha256: &str,
        projection_sha256: Option<&str>,
    ) -> ProviderCall {
        ProviderCall::Observe {
            status: 200,
            revision_id: revision_id.to_string(),
            content_sha256: content_sha256.to_string(),
            projection_sha256: projection_sha256.map(str::to_string),
        }
    }

    fn provider_pair(
        gateway_calls: Vec<ProviderCall>,
        auth_calls: Vec<ProviderCall>,
    ) -> (TopologyProviderSaga, Vec<MockProvider>) {
        let gateway = spawn_provider("gateway", gateway_calls);
        let auth = spawn_provider("auth", auth_calls);
        let saga = TopologyProviderSaga::from_config(
            TopologyProviderConfig::new(
                Some(HttpManagementProviderConfig::new(&gateway.origin).unwrap()),
                Some(HttpManagementProviderConfig::new(&auth.origin).unwrap()),
            )
            .with_timeout(Duration::from_secs(2))
            .unwrap(),
        )
        .unwrap();
        (saga, vec![gateway, auth])
    }

    fn spawn_provider(provider: &'static str, calls: Vec<ProviderCall>) -> MockProvider {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let thread = thread::spawn(move || {
            let mut applied_revision_id = String::new();
            let mut applied_content_sha256 = String::new();
            let mut applied_projection_sha256 = String::new();
            for call in calls {
                let mut stream = accept_before(&listener, Instant::now() + Duration::from_secs(4));
                stream
                    .set_read_timeout(Some(Duration::from_secs(2)))
                    .unwrap();
                let received = read_request(&mut stream);
                assert_eq!(received.path, "/api/v1/topologies/primary");
                match call {
                    ProviderCall::Mutation {
                        action,
                        status,
                        expected_routes,
                        expected_operation_phase,
                    } => {
                        let expected_method = if action == "delete" { "DELETE" } else { "PUT" };
                        assert_eq!(received.method, expected_method);
                        let body: Value = serde_json::from_slice(&received.body).unwrap();
                        assert_eq!(body["api_version"], "v1");
                        assert_eq!(body["provider"], provider);
                        assert_eq!(body["action"], action);
                        assert_eq!(body["topology_id"], "primary");
                        if let Some(expected_routes) = expected_routes {
                            assert_eq!(
                                body["routes"].as_array().map(Vec::len),
                                Some(expected_routes)
                            );
                            assert_eq!(
                                body["grants"].as_array().map(Vec::len),
                                Some(expected_routes)
                            );
                        }
                        if let Some(expected_operation_phase) = expected_operation_phase {
                            assert!(
                                body["operation_id"]
                                    .as_str()
                                    .is_some_and(|operation_id| operation_id
                                        .ends_with(expected_operation_phase)),
                                "unexpected runtime repair operation id: {}",
                                body["operation_id"]
                            );
                        }
                        let expected_key = format!(
                            "{}:{provider}:{action}",
                            body["operation_id"].as_str().unwrap()
                        );
                        assert_eq!(received.headers.get("idempotency-key"), Some(&expected_key));
                        let response = if (200..=299).contains(&status) {
                            if action == "apply" {
                                applied_revision_id = body["desired_revision_id"]
                                    .as_str()
                                    .unwrap_or_default()
                                    .to_string();
                                applied_content_sha256 = body["desired_content_sha256"]
                                    .as_str()
                                    .unwrap_or_default()
                                    .to_string();
                                applied_projection_sha256 = provider_projection_sha256_from_json(
                                    &body["routes"],
                                    &body["grants"],
                                )
                                .expect("canonical provider projection digest");
                            }
                            json!({
                                "api_version": "v1",
                                "provider": provider,
                                "action": action,
                                "topology_id": "primary",
                                "operation_id": body["operation_id"],
                                "completed": true,
                                "observed_revision_id": body["desired_revision_id"],
                                "observed_content_sha256": body["desired_content_sha256"],
                                "absent": action == "delete"
                            })
                        } else {
                            json!({"code": "MOCK_PROVIDER_FAILURE", "detail": "rejected"})
                        };
                        write_response(&mut stream, status, &response);
                    }
                    ProviderCall::Observe {
                        status,
                        revision_id,
                        content_sha256,
                        projection_sha256,
                    } => {
                        assert_eq!(received.method, "GET");
                        assert!(received.body.is_empty());
                        write_response(
                            &mut stream,
                            status,
                            &json!({
                                "api_version": "v1",
                                "provider": provider,
                                "topology_id": "primary",
                                "observed_revision_id": revision_id,
                                "observed_content_sha256": content_sha256,
                                "observed_projection_sha256": projection_sha256,
                                "absent": false,
                                "endpoints": [],
                                "links": []
                            }),
                        );
                    }
                    ProviderCall::ObserveAbsent => {
                        assert_eq!(received.method, "GET");
                        assert!(received.body.is_empty());
                        write_response(
                            &mut stream,
                            200,
                            &json!({
                                "api_version": "v1",
                                "provider": provider,
                                "topology_id": "primary",
                                "absent": true,
                                "endpoints": [],
                                "links": []
                            }),
                        );
                    }
                    ProviderCall::ObserveApplied => {
                        assert_eq!(received.method, "GET");
                        assert!(received.body.is_empty());
                        assert!(!applied_revision_id.is_empty());
                        assert!(!applied_content_sha256.is_empty());
                        assert!(!applied_projection_sha256.is_empty());
                        write_response(
                            &mut stream,
                            200,
                            &json!({
                                "api_version": "v1",
                                "provider": provider,
                                "topology_id": "primary",
                                "observed_revision_id": applied_revision_id,
                                "observed_content_sha256": applied_content_sha256,
                                "observed_projection_sha256": applied_projection_sha256,
                                "absent": false,
                                "endpoints": [],
                                "links": []
                            }),
                        );
                    }
                }
            }
        });
        MockProvider { origin, thread }
    }

    fn join_providers(providers: Vec<MockProvider>) {
        for provider in providers {
            provider.thread.join().expect("provider mock completed");
        }
    }

    fn process_until_operation_terminal(
        store: &DurableStore,
        provider: &TopologyProviderSaga,
        operation_id: &str,
    ) {
        for _ in 0..16 {
            let status = store
                .operation_store()
                .get(operation_id)
                .unwrap()
                .expect("operation exists")
                .status;
            if status.is_terminal() {
                return;
            }
            assert!(
                process_one(store, Some(provider)).unwrap(),
                "operation {operation_id} is non-terminal without a runnable control-plane job"
            );
        }
        panic!("operation {operation_id} did not become terminal within 16 control-plane jobs");
    }

    fn accept_before(listener: &TcpListener, deadline: Instant) -> TcpStream {
        loop {
            match listener.accept() {
                Ok((stream, _)) => {
                    stream.set_nonblocking(false).unwrap();
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
        const MAX_REQUEST: usize = 2 * 1024 * 1024;
        let mut bytes = Vec::new();
        let header_end = loop {
            let mut chunk = [0_u8; 4096];
            let read = stream.read(&mut chunk).unwrap();
            assert!(read > 0, "provider request closed before headers");
            bytes.extend_from_slice(&chunk[..read]);
            assert!(bytes.len() <= MAX_REQUEST);
            if let Some(index) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
                break index + 4;
            }
        };
        let head = std::str::from_utf8(&bytes[..header_end]).unwrap();
        let mut lines = head.split("\r\n");
        let mut request_line = lines.next().unwrap().split_whitespace();
        let method = request_line.next().unwrap().to_string();
        let path = request_line.next().unwrap().to_string();
        let headers = lines
            .filter_map(|line| line.split_once(':'))
            .map(|(name, value)| (name.trim().to_ascii_lowercase(), value.trim().to_string()))
            .collect::<BTreeMap<_, _>>();
        let content_length = headers
            .get("content-length")
            .map(|value| value.parse::<usize>().unwrap())
            .unwrap_or_default();
        while bytes.len() < header_end + content_length {
            let mut chunk = [0_u8; 4096];
            let read = stream.read(&mut chunk).unwrap();
            assert!(read > 0, "provider request closed before body");
            bytes.extend_from_slice(&chunk[..read]);
            assert!(bytes.len() <= MAX_REQUEST);
        }
        MockRequest {
            method,
            path,
            headers,
            body: bytes[header_end..header_end + content_length].to_vec(),
        }
    }

    fn write_response(stream: &mut TcpStream, status: u16, body: &Value) {
        let body = body.to_string();
        write!(
            stream,
            "HTTP/1.1 {status} Mock\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )
        .unwrap();
        stream.flush().unwrap();
    }
}
