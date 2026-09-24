//! Background reconciliation responsibilities.
use crate::durable::DurableStore;
use crate::topology_provider::TopologyProviderObservation;
use crate::topology_provider::TopologyProviderObservedState;
use crate::topology_provider::TopologyProviderSaga;
use crate::topology_provider::TopologyProvidersObservation;
use crate::topology_provider::provider_projection_sha256;
use crate::topology_worker::context::{bounded_detail, now_marker, now_ms};
use crate::topology_worker::external_health::refresh_external_runtime_health;
use crate::topology_worker::network::{
    NetworkObservationContext, NetworkProbePool, observed_network_status,
};
use crate::topology_worker::observation::{
    desired_deployment_state, observed_deployment_state, runtime_health, runtime_states_match,
};
use crate::topology_worker::projection::{
    reconcile_runtime_binding_projections, runtime_binding_route_is_admissible,
};
use orchestrator_core::ApiBinding;
use orchestrator_core::TopologyDeploymentStatus;
use orchestrator_core::TopologyDrift;
use orchestrator_core::TopologyDriftKind;
use orchestrator_core::TopologyEndpointStatus;
use orchestrator_core::TopologyHealth;
use orchestrator_core::TopologyLinkStatus;
use orchestrator_core::TopologyReconciliationState;
use orchestrator_core::TopologyResourceKind;
use orchestrator_core::TopologySpec;
use orchestrator_core::TopologyStatus;
use orchestrator_runtime::RuntimeObservedState;
use orchestrator_storage::RuntimeManagementMode;
use std::collections::BTreeSet;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;
use std::thread;
use std::time::Duration;

pub(super) const RECONCILE_INTERVAL: Duration = Duration::from_secs(5);

pub(super) fn run_reconciler_loop(
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

pub(super) fn reconcile_all(
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

pub(super) fn reconcile_one(
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

pub(super) fn add_provider_drift(
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
pub(super) fn runtime_topology_status(
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
