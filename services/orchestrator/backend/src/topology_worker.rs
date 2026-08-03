use crate::durable::DurableStore;
use crate::topology_provider::{
    TopologyProviderApplyState, TopologyProviderObservation, TopologyProviderObservedState,
    TopologyProviderSaga, TopologyProvidersObservation,
};
use getrandom::fill as random_fill;
use orchestrator_control_plane::{
    ClaimRequest, CompleteRequest, CompletionStatus, DEFAULT_LEASE_MS, DurableOperationStatus,
    JobKind, JobStatus, JobStore, OperationCoordinator, OperationRepository,
};
use orchestrator_legacy::{
    Endpoint, EndpointProbe, TcpEndpointProbe, TopologyDeploymentStatus,
    TopologyDesiredDeploymentState, TopologyDrift, TopologyDriftKind, TopologyEndpointStatus,
    TopologyHealth, TopologyLinkStatus, TopologyObservedDeploymentState,
    TopologyReconciliationState, TopologyResourceKind, TopologySpec, TopologyStatus,
    parse_endpoint_id, validate_endpoint_id,
};
use orchestrator_runtime::{RuntimeDesiredState, RuntimeInstance, RuntimeObservedState};
use orchestrator_storage::{RuntimeManagementMode, StoredRuntimeInstance, TopologyApplyOutcome};
use serde::Deserialize;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::io::Read;
use std::net::{TcpStream, ToSocketAddrs};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread;
use std::thread::JoinHandle;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const CONTROL_PLANE_NODE_ID: &str = "control-plane";
const RECONCILE_INTERVAL: Duration = Duration::from_secs(5);
const NETWORK_PROBE_TIMEOUT: Duration = Duration::from_millis(750);
const NETWORK_PROBE_CONCURRENCY: usize = 16;
const ENDPOINT_PROBE_BATCH: usize = 512;
const LINK_PROBE_BATCH: usize = 1_024;
const NETWORK_OBSERVATION_MAX_AGE_MS: i64 = 120_000;
const NETWORK_RESPONSE_LIMIT: usize = 4_096;
const ENDPOINT_EVIDENCE_PREFIX: &str = "network probe:";
const LINK_EVIDENCE_PREFIX: &str = "source probe:";

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct TopologyApplyPayload {
    topology_id: String,
    revision_id: String,
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

pub(crate) fn run_loop(
    storage: DurableStore,
    provider: Option<TopologyProviderSaga>,
    shutdown: Arc<AtomicBool>,
) {
    let reconciler = provider.clone().and_then(|reconcile_provider| {
        let reconcile_storage = storage.clone();
        let reconcile_shutdown = Arc::clone(&shutdown);
        thread::Builder::new()
            .name("orchestrator-topology-reconciler".to_string())
            .spawn(move || {
                run_reconciler_loop(&reconcile_storage, &reconcile_provider, &reconcile_shutdown)
            })
            .ok()
    });
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
        let deadline = std::time::Instant::now() + Duration::from_secs(1);
        while !shutdown.load(Ordering::Acquire) && std::time::Instant::now() < deadline {
            thread::sleep(Duration::from_millis(100));
        }
    }
}

fn run_reconciler_loop(
    storage: &DurableStore,
    provider: &TopologyProviderSaga,
    shutdown: &AtomicBool,
) {
    let network_probes = NetworkProbePool::new();
    while !shutdown.load(Ordering::Acquire) {
        if let Err(error) = reconcile_all(storage, provider, &network_probes) {
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
    Ok(())
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
    let runtime_instances = storage
        .runtime_instances(None)
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
    );
    add_provider_drift(
        &mut drift,
        topology_id,
        &providers.auth,
        applied_revision_id,
        &content_sha256,
    );
    let (deployments, endpoints, links) = runtime_topology_status(
        revision.spec(),
        &runtime_instances,
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
    let providers_match = providers
        .gateway
        .matches(applied_revision_id, &content_sha256)
        && providers.auth.matches(applied_revision_id, &content_sha256);
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
) {
    if observation.matches(desired_revision_id, desired_content_sha256) {
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
                "{} provider reports revision {:?} with content hash {:?}; expected {desired_revision_id} with {desired_content_sha256}",
                observation.provider,
                observation.observed_revision_id,
                observation.observed_content_sha256
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
            if !runtime_states_match(
                &stored.instance.desired_state,
                &stored.instance.observed_state,
            ) {
                drift.push(TopologyDrift {
                    resource_kind: TopologyResourceKind::Deployment,
                    resource_id: stored.instance.deployment_id.clone(),
                    kind: if stored.instance.observed_state == RuntimeObservedState::Missing {
                        TopologyDriftKind::Missing
                    } else {
                        TopologyDriftKind::Changed
                    },
                    detail: "runtime observed state does not match desired state".to_string(),
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
        link_probe_source_endpoints,
        previous_status,
        network_probes,
        observed_at,
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

fn observed_network_status(
    spec: &TopologySpec,
    relevant: &[&StoredRuntimeInstance],
    link_probe_source_endpoints: &BTreeSet<String>,
    previous_status: Option<&TopologyStatus>,
    network_probes: &NetworkProbePool,
    observed_at: &str,
    drift: &mut Vec<TopologyDrift>,
) -> (Vec<TopologyEndpointStatus>, Vec<TopologyLinkStatus>) {
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
    for endpoint in &spec.endpoints {
        let matching = relevant
            .iter()
            .copied()
            .filter(|stored| {
                stored.endpoint == endpoint.endpoint
                    && stored.instance.service_id == endpoint.service_id
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

fn bounded_detail(detail: &str) -> String {
    detail
        .chars()
        .filter(|character| !character.is_control())
        .take(2_048)
        .collect()
}

fn recover_expired(storage: &DurableStore, now_ms: i64) -> Result<(), String> {
    let mut jobs = storage.job_store();
    let recovered = jobs
        .recover_expired(now_ms)
        .map_err(|error| error.to_string())?;
    for job in recovered {
        if job.status != JobStatus::NeedsAttention {
            continue;
        }
        let mut operations = storage.operation_store();
        OperationCoordinator::new(&mut operations, &mut jobs)
            .project(&job.operation_id, now_ms)
            .map_err(|error| error.to_string())?;
        if job.kind == JobKind::TopologyApply
            && let Ok(payload) = serde_json::from_value::<TopologyApplyPayload>(job.payload.clone())
        {
            recover_unknown_topology_apply(
                storage,
                &payload.topology_id,
                &payload.revision_id,
                &job.operation_id,
                "control-plane worker lease expired with an unproven provider outcome",
            )?;
        }
    }
    Ok(())
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
        if let Ok(payload) = serde_json::from_value::<TopologyApplyPayload>(job.payload.clone()) {
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
    let mut heads = storage
        .topology_heads(&payload.topology_id)
        .map_err(|error| error.to_string())?
        .ok_or_else(|| format!("topology {} disappeared", payload.topology_id))?;
    if heads.applying_revision_id.is_none()
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
    if heads.applying_revision_id.as_deref() != Some(payload.revision_id.as_str())
        || heads.applying_operation_id.as_deref() != Some(job.operation_id.as_str())
    {
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
    let previous = heads
        .applied_revision_id
        .as_deref()
        .map(|revision_id| {
            storage
                .topology_revision(&payload.topology_id, revision_id)
                .map_err(|error| error.to_string())?
                .ok_or_else(|| format!("applied topology revision {revision_id} disappeared"))
        })
        .transpose()?;

    // All provider I/O happens after the topology transaction that established
    // apply ownership has committed and before the completion transaction.
    match provider.apply(
        &payload.topology_id,
        &payload.revision_id,
        revision.spec(),
        previous.as_ref().map(|revision| revision.revision_id()),
        previous.as_ref().map(|revision| revision.spec()),
        &job.operation_id,
    ) {
        Ok(receipt) => {
            storage
                .finish_topology_apply(
                    &payload.topology_id,
                    &payload.revision_id,
                    &job.operation_id,
                    TopologyApplyOutcome::Succeeded,
                    &now_marker(),
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
            let degraded = failure.state == TopologyProviderApplyState::Degraded;
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
            let detail = failure.to_string();
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
    if let Some(existing) = storage
        .runtime_instance(&payload.deployment_id)
        .map_err(external_storage_failure)?
    {
        if existing.management_mode == RuntimeManagementMode::External
            && existing.endpoint == payload.endpoint
            && existing.instance.service_id == payload.service_id
            && existing.instance.release_version == payload.version
            && existing.instance.artifact_digest == payload.artifact_digest
            && existing.instance.observed_state == RuntimeObservedState::Running
            && existing.instance.health.eq_ignore_ascii_case("healthy")
        {
            return Ok(serde_json::json!({
                "instance": existing,
                "health": {"healthy": true, "replayed": true},
                "version": payload.version,
            }));
        }
        return Err(ExternalHealthFailure {
            status: CompletionStatus::NeedsAttention,
            code: "EXTERNAL_DEPLOYMENT_CONFLICT",
            detail: format!(
                "deployment {} already has a different runtime projection",
                payload.deployment_id
            ),
        });
    }

    let evidence = probe_external_endpoint(&payload)?;
    if !evidence
        .get("healthy")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        return Err(ExternalHealthFailure {
            status: CompletionStatus::Failed,
            code: "EXTERNAL_ENDPOINT_UNHEALTHY",
            detail: evidence
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("External endpoint did not pass its protocol health probe")
                .to_string(),
        });
    }
    let stored = StoredRuntimeInstance {
        node_id: "external".to_string(),
        instance: RuntimeInstance {
            deployment_id: payload.deployment_id.clone(),
            service_id: payload.service_id.clone(),
            release_version: payload.version.clone(),
            container_id: String::new(),
            artifact_digest: payload.artifact_digest.clone(),
            desired_state: RuntimeDesiredState::Running,
            observed_state: RuntimeObservedState::Running,
            health: "HEALTHY".to_string(),
        },
        management_mode: RuntimeManagementMode::External,
        endpoint: payload.endpoint.clone(),
        updated_at: now_marker(),
    };
    storage
        .put_runtime_instance(&stored)
        .map_err(external_storage_failure)?;
    Ok(serde_json::json!({
        "instance": stored,
        "health": evidence,
        "version": payload.version,
    }))
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
            updated_at: "unix-ms:1".to_string(),
        }
    }

    fn providers() -> TopologyProvidersObservation {
        let observation = |provider: &str| TopologyProviderObservation {
            provider: provider.to_string(),
            state: TopologyProviderObservedState::Present,
            observed_revision_id: Some("primary:r1:test".to_string()),
            observed_content_sha256: Some("a".repeat(64)),
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
    };
    use orchestrator_control_plane::{
        ClaimRequest, DurableOperationStatus, JobStore, OperationRepository,
    };
    use orchestrator_legacy::{
        OrchestratorStore, ServiceRelease, ServiceReleaseManifest, TopologyEndpointSpec,
        TopologyLinkSpec, TopologyRevision, service_manifest_from_release,
    };
    use orchestrator_storage::{
        RuntimeManagementMode, SqliteOrchestratorStore, StoredRuntimeInstance,
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
        },
        Observe {
            status: u16,
            revision_id: String,
            content_sha256: String,
        },
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
            provider_pair(vec![mutation("apply", 200)], vec![mutation("apply", 200)]);
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
        assert!(
            initial_apply.body["data"]["operation_id"]
                .as_str()
                .is_some()
        );

        // A queued apply is safe to resume after the process reopens the same
        // SQLite file because no provider side effect has started yet.
        drop(store);
        let store = reopen_store(&database_path);
        assert!(process_one(&store, Some(&initial_provider)).unwrap());
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

        let (second_provider, second_mocks) =
            provider_pair(vec![mutation("apply", 200)], vec![mutation("apply", 200)]);
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
        assert!(process_one(&store, Some(&second_provider)).unwrap());
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
            provider_pair(vec![mutation("apply", 200)], vec![mutation("apply", 200)]);
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
        assert!(process_one(&store, Some(&rollback_provider)).unwrap());
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
        assert!(process_one(&store, Some(&provider)).unwrap());
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

        let (retry_provider, retry_mocks) =
            provider_pair(vec![mutation("apply", 200)], vec![mutation("apply", 200)]);
        {
            let mut operations = store.operation_store();
            let mut jobs = store.job_store();
            OperationCoordinator::new(&mut operations, &mut jobs)
                .retry(&operation_id, now_ms())
                .unwrap();
        }
        assert!(process_one(&store, Some(&retry_provider)).unwrap());
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
        durable
            .put_runtime_instance(&StoredRuntimeInstance {
                node_id: "node-gateway".to_string(),
                instance: RuntimeInstance {
                    deployment_id: "deployment-gateway".to_string(),
                    service_id: "gateway".to_string(),
                    release_version: "1.0.0".to_string(),
                    container_id: "container-gateway".to_string(),
                    artifact_digest: format!("sha256:{}", "b".repeat(64)),
                    desired_state: RuntimeDesiredState::Running,
                    observed_state: RuntimeObservedState::Running,
                    health: "HEALTHY".to_string(),
                },
                management_mode: RuntimeManagementMode::Managed,
                endpoint: "127.0.0.1:8080:gateway".to_string(),
                updated_at: "unix-ms:1".to_string(),
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
            }],
        )
        .unwrap()
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
        ProviderCall::Mutation { action, status }
    }

    fn observe(revision_id: &str, content_sha256: &str) -> ProviderCall {
        ProviderCall::Observe {
            status: 200,
            revision_id: revision_id.to_string(),
            content_sha256: content_sha256.to_string(),
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
            for call in calls {
                let mut stream = accept_before(&listener, Instant::now() + Duration::from_secs(4));
                stream
                    .set_read_timeout(Some(Duration::from_secs(2)))
                    .unwrap();
                let received = read_request(&mut stream);
                assert_eq!(received.path, "/api/v1/topologies/primary");
                match call {
                    ProviderCall::Mutation { action, status } => {
                        let expected_method = if action == "delete" { "DELETE" } else { "PUT" };
                        assert_eq!(received.method, expected_method);
                        let body: Value = serde_json::from_slice(&received.body).unwrap();
                        assert_eq!(body["api_version"], "v1");
                        assert_eq!(body["provider"], provider);
                        assert_eq!(body["action"], action);
                        assert_eq!(body["topology_id"], "primary");
                        let expected_key = format!(
                            "{}:{provider}:{action}",
                            body["operation_id"].as_str().unwrap()
                        );
                        assert_eq!(received.headers.get("idempotency-key"), Some(&expected_key));
                        let response = if (200..=299).contains(&status) {
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
