//! Background projection responsibilities.
use crate::durable::DurableStore;
use crate::topology_provider::RuntimeProjectionOrder;
use crate::topology_provider::TopologyProviderObservedState;
use crate::topology_provider::TopologyProviderSaga;
use crate::topology_provider::provider_projection_sha256;
use orchestrator_core::ApiBinding;
use orchestrator_core::ApiBindingState;
use orchestrator_core::binding_projection::RuntimeProjectionTransition;
use orchestrator_core::binding_projection::runtime_projection_transition;
use orchestrator_protocol::RuntimeDesiredState;
use orchestrator_storage::RuntimeManagementMode;
use orchestrator_storage::StoredRuntimeInstance;
use serde::Deserialize;
use serde::Serialize;
use sha2::Digest;
use sha2::Sha256;
use std::collections::BTreeMap;
use std::collections::BTreeSet;

pub(super) const RUNTIME_BINDING_PROJECTION_STATE_NAMESPACE: &str =
    "topology-runtime-binding-projection-v1";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(super) struct RuntimeBindingProjectionState {
    pub(super) schema_version: u8,
    pub(super) revision_id: String,
    pub(super) content_sha256: String,
    #[serde(default)]
    pub(super) projection_sha256: String,
    pub(super) bindings: BTreeMap<String, String>,
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

pub(super) fn runtime_binding_route_is_admissible(
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

pub(super) fn runtime_projection_state(
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

pub(super) fn runtime_projection_operation_id(
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
