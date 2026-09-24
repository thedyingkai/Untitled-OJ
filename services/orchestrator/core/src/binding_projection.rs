//! Deterministic Binding generation, activation and projection transition rules.
//! Callers supply the current/desired records and timestamps. This module has no
//! storage, clock, network, runtime execution or lease ownership.
use crate::{
    ApiBinding, ApiBindingDesiredState, ApiBindingHealth, ApiBindingObservedState, ApiBindingState,
};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TopologyApplyGroupMember {
    pub topology_id: String,
    pub revision_id: String,
    pub active_bindings: Vec<ApiBinding>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeProjectionTransition {
    Unchanged,
    Revoke,
    Grant,
    Mixed,
}

pub fn runtime_projection_transition(
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

/// A workload JWT carries one deployment-wide credential generation. If any
/// route for a consumer changes, every still-active sibling is therefore
/// staged with the same next generation. This makes old tokens fail every API
/// immediately after the Gateway atomically switches the route table.
pub fn stage_binding_generations(
    mut desired: Vec<ApiBinding>,
    current: Vec<ApiBinding>,
    revision_id: &str,
    operation_id: &str,
    now: &str,
) -> Vec<ApiBinding> {
    let mut consumers = current
        .iter()
        .map(|binding| binding.consumer_deployment_id.clone())
        .chain(
            desired
                .iter()
                .map(|binding| binding.consumer_deployment_id.clone()),
        )
        .collect::<BTreeSet<_>>();
    let desired_keys = desired
        .iter()
        .map(|binding| {
            (
                binding.consumer_deployment_id.clone(),
                binding.requirement_name.clone(),
            )
        })
        .collect::<BTreeSet<_>>();

    for consumer in std::mem::take(&mut consumers) {
        let mut wanted = desired
            .iter()
            .filter(|binding| binding.consumer_deployment_id == consumer)
            .collect::<Vec<_>>();
        let mut active = current
            .iter()
            .filter(|binding| {
                binding.consumer_deployment_id == consumer
                    && binding.desired_state == "ACTIVE"
                    && binding.state == ApiBindingState::Active
            })
            .collect::<Vec<_>>();
        wanted.sort_by_key(|binding| binding.requirement_name.as_str());
        active.sort_by_key(|binding| binding.requirement_name.as_str());
        let changed = wanted.len() != active.len()
            || wanted
                .iter()
                .zip(active.iter())
                .any(|(wanted, active)| !same_binding_route(wanted, active));
        let previous_generation = current
            .iter()
            .filter(|binding| binding.consumer_deployment_id == consumer)
            .map(|binding| {
                binding
                    .credential_generation
                    .max(binding.context_generation)
            })
            .max()
            .unwrap_or(0);
        let generation = if changed {
            previous_generation.saturating_add(1).max(1)
        } else {
            previous_generation.max(1)
        };
        for binding in desired
            .iter_mut()
            .filter(|binding| binding.consumer_deployment_id == consumer)
        {
            binding.credential_generation = generation;
            binding.context_generation = generation;
            if let Some(existing) = current
                .iter()
                .find(|existing| existing.binding_id == binding.binding_id)
            {
                binding.created_at = existing.created_at.clone();
            }
        }
        for existing in current.iter().filter(|binding| {
            binding.consumer_deployment_id == consumer
                && binding.desired_state == "ACTIVE"
                && !desired_keys.contains(&(
                    binding.consumer_deployment_id.clone(),
                    binding.requirement_name.clone(),
                ))
        }) {
            let mut revoked = existing.clone();
            revoked.topology_revision_id = revision_id.to_string();
            revoked.credential_generation = generation;
            revoked.context_generation = generation;
            revoked.desired_state = ApiBindingDesiredState::Revoked;
            revoked.observed_state = ApiBindingObservedState::Pending;
            revoked.health = ApiBindingHealth::Unknown;
            revoked.drift.clear();
            revoked.last_operation_id = operation_id.to_string();
            revoked.state = ApiBindingState::Pending;
            revoked.updated_at = now.to_string();
            desired.push(revoked);
        }
    }
    desired.sort_by(|left, right| {
        (&left.consumer_deployment_id, &left.requirement_name)
            .cmp(&(&right.consumer_deployment_id, &right.requirement_name))
    });
    desired
}

fn same_binding_route(left: &ApiBinding, right: &ApiBinding) -> bool {
    left.requirement_name == right.requirement_name
        && left.api_id == right.api_id
        && left.api_version == right.api_version
        && left.consumer_deployment_id == right.consumer_deployment_id
        && left.consumer_service_id == right.consumer_service_id
        && left.consumer_node_id == right.consumer_node_id
        && left.consumer_endpoint == right.consumer_endpoint
        && left.provider_deployment_id == right.provider_deployment_id
        && left.provider_service_id == right.provider_service_id
        && left.provider_node_id == right.provider_node_id
        && left.provider_endpoint == right.provider_endpoint
        && left.provider_path == right.provider_path
        && left.virtual_endpoint == right.virtual_endpoint
        && left.protocol == right.protocol
        && left.methods == right.methods
        && left.auth_mode == right.auth_mode
        && left.provider_auth_mode == right.provider_auth_mode
        && left.permission == right.permission
        && left.timeout_ms == right.timeout_ms
        && left.link_source_endpoint == right.link_source_endpoint
        && left.link_target_endpoint == right.link_target_endpoint
        && left.optional == right.optional
}

pub fn validate_prepared_bindings(
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

pub fn activate_staged_bindings(
    mut bindings: Vec<ApiBinding>,
    observed_at: &str,
) -> Vec<ApiBinding> {
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

pub fn normalize_group_binding_moves(members: &mut [TopologyApplyGroupMember]) {
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
