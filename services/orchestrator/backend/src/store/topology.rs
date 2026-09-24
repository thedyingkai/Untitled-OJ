//! Store topology responsibilities.
use crate::durable::DurableStore;
use crate::store::bindings::{provider_auth_supported, selected_topology_spec};
use crate::store::commands::required_text;
use crate::store::context::now_marker;
use crate::store::error::{StoreError, core_error, storage_error};
use crate::store::service_context::managed_service_context_spec;
use orchestrator_core::ApiBinding;
use orchestrator_core::ApiBindingObservedState;
use orchestrator_core::ApiBindingState;
use orchestrator_core::ServiceReleaseContract;
use orchestrator_core::TopologyApiBindingSpec;
use orchestrator_core::TopologyEndpointSpec;
use orchestrator_core::TopologyLinkSpec;
use orchestrator_core::TopologySpec;
use orchestrator_core::api_version_matches;
use orchestrator_core::parse_endpoint_id;
use orchestrator_core::validate_endpoint_id;
use orchestrator_manager::store::validation::InstallTopologySelection;
use orchestrator_protocol::BindingContextApplyPayload;
use orchestrator_protocol::ManagedServiceContextProjection;
use serde_json::json;
use std::collections::BTreeSet;

#[derive(Debug, Clone)]
pub(crate) struct StoreTopologyApplyPlan {
    pub(crate) topology_id: String,
    pub(crate) revision_id: String,
    pub(crate) staged_bindings: Vec<ApiBinding>,
    pub(crate) previous_bindings: Vec<ApiBinding>,
}

#[derive(Debug, Clone)]
pub(crate) struct BindingContextTransitionPlan {
    pub(crate) deployment_id: String,
    pub(crate) node_id: String,
    pub(crate) container_id: String,
    pub(crate) forward: BindingContextApplyPayload,
    pub(crate) rollback: BindingContextApplyPayload,
}

/// A validated, materializable view of bindings that are still staged by a

/// Topology apply. The durable rows remain `PENDING`; only these private clones

/// are represented as `RESOLVED` while constructing the desired Agent context.

/// Keeping this conversion behind a distinct type prevents ordinary context

/// callers from treating an arbitrary uncommitted binding as available.

#[derive(Debug, Clone)]
pub(crate) struct StagedApplyDesiredContextBindings(Vec<ApiBinding>);

impl StagedApplyDesiredContextBindings {
    pub(crate) fn from_plans(
        plans: &[StoreTopologyApplyPlan],
        deployment_id: &str,
    ) -> Result<Self, StoreError> {
        let mut materializable = Vec::new();
        let mut requirements = BTreeSet::new();
        let mut operation_id = None::<String>;
        for plan in plans {
            for binding in plan.staged_bindings.iter().filter(|binding| {
                binding.consumer_deployment_id == deployment_id && binding.desired_state == "ACTIVE"
            }) {
                if binding.state != ApiBindingState::Pending
                    || binding.observed_state != "PENDING"
                    || binding.topology_id != plan.topology_id
                    || binding.topology_revision_id != plan.revision_id
                    || binding.last_operation_id.trim().is_empty()
                {
                    return Err(StoreError::new(
                        409,
                        "STORE_STAGED_BINDING_CONTEXT_INVALID",
                        format!(
                            "binding {} is not a valid staged activation for topology {} revision {}",
                            binding.binding_id, plan.topology_id, plan.revision_id
                        ),
                    ));
                }
                match operation_id.as_deref() {
                    Some(expected) if expected != binding.last_operation_id => {
                        return Err(StoreError::new(
                            409,
                            "STORE_STAGED_BINDING_CONTEXT_INVALID",
                            format!(
                                "consumer {deployment_id} staged bindings span more than one Operation"
                            ),
                        ));
                    }
                    None => operation_id = Some(binding.last_operation_id.clone()),
                    Some(_) => {}
                }
                if !requirements.insert(binding.requirement_name.clone()) {
                    return Err(StoreError::new(
                        409,
                        "STORE_BINDING_REQUIREMENT_CONFLICT",
                        format!(
                            "consumer {deployment_id} requirement {} is staged more than once",
                            binding.requirement_name
                        ),
                    ));
                }

                let mut resolved_view = binding.clone();
                resolved_view.state = ApiBindingState::Resolved;
                resolved_view.observed_state = ApiBindingObservedState::Resolved;
                resolved_view.validate().map_err(|error| {
                    StoreError::new(
                        409,
                        "STORE_STAGED_BINDING_CONTEXT_INVALID",
                        format!(
                            "binding {} cannot materialize a desired Service Context: {error}",
                            binding.binding_id
                        ),
                    )
                })?;
                materializable.push(resolved_view);
            }
        }
        materializable.sort_by(|left, right| {
            left.requirement_name
                .cmp(&right.requirement_name)
                .then_with(|| left.binding_id.cmp(&right.binding_id))
        });
        Ok(Self(materializable))
    }

    pub(crate) fn as_slice(&self) -> &[ApiBinding] {
        &self.0
    }
}

pub(crate) fn propose_generation_sibling_topology(
    storage: &DurableStore,
    selection: &InstallTopologySelection,
    operation_id: &str,
) -> Result<StoreTopologyApplyPlan, StoreError> {
    let (spec, expected_draft) = selected_topology_spec(storage, selection)?;
    let topology_id = spec.topology_id.clone();
    let revision = storage
        .create_next_topology_revision(
            &topology_id,
            &expected_draft,
            spec,
            now_marker(),
            "store-replacement".to_string(),
            "reproject deployment-wide binding credential generation".to_string(),
        )
        .map_err(storage_error)?;
    let previous_bindings = storage
        .api_bindings_for_topology(&topology_id)
        .map_err(storage_error)?;
    let desired = previous_bindings
        .iter()
        .filter(|binding| {
            binding.desired_state == "ACTIVE" && binding.state == ApiBindingState::Active
        })
        .cloned()
        .collect::<Vec<_>>();
    let staged_bindings = storage
        .stage_precomputed_topology_api_bindings(
            &topology_id,
            revision.revision_id(),
            operation_id,
            desired,
        )
        .map_err(|error| {
            StoreError::new(422, "STORE_BINDING_TOPOLOGY_INVALID", error.to_string())
        })?;
    Ok(StoreTopologyApplyPlan {
        topology_id,
        revision_id: revision.revision_id().to_string(),
        staged_bindings,
        previous_bindings,
    })
}

pub(crate) fn align_group_binding_generations(
    storage: &DurableStore,
    plans: &mut [StoreTopologyApplyPlan],
    affected_consumers: &BTreeSet<String>,
) -> Result<(), StoreError> {
    let planned_topologies = plans
        .iter()
        .map(|plan| plan.topology_id.clone())
        .collect::<BTreeSet<_>>();
    let mut seen_requirements = BTreeSet::new();
    for consumer in affected_consumers {
        let current = active_consumer_bindings(storage, consumer)?;
        let current_topologies = current
            .iter()
            .map(|binding| binding.topology_id.clone())
            .filter(|topology_id| !topology_id.is_empty())
            .collect::<BTreeSet<_>>();
        if !current_topologies.is_subset(&planned_topologies) {
            return Err(StoreError::new(
                409,
                "STORE_REPLACEMENT_SIBLING_TOPOLOGY_REQUIRED",
                format!(
                    "consumer {consumer} has deployment-wide sibling bindings in {:?}; every sibling topology requires one strong CAS entry",
                    current_topologies
                        .difference(&planned_topologies)
                        .collect::<Vec<_>>()
                ),
            ));
        }
        let binding_generation = current
            .iter()
            .map(|binding| {
                binding
                    .credential_generation
                    .max(binding.context_generation)
            })
            .max()
            .unwrap_or(0);
        let projected_generation = storage
            .get_state::<ManagedServiceContextProjection>("managed-service-context-v1", consumer)
            .map_err(storage_error)?
            .map(|projection| {
                projection
                    .current
                    .as_ref()
                    .unwrap_or(&projection.last_nonempty)
                    .generation
                    .max(projection.last_nonempty.generation)
            })
            .unwrap_or(0);
        let next_generation = binding_generation
            .max(projected_generation)
            .saturating_add(1)
            .max(1);
        for plan in plans.iter_mut() {
            for binding in plan.staged_bindings.iter_mut().filter(|binding| {
                binding.consumer_deployment_id == *consumer && binding.desired_state == "ACTIVE"
            }) {
                if !seen_requirements.insert((consumer.clone(), binding.requirement_name.clone())) {
                    return Err(StoreError::new(
                        409,
                        "STORE_BINDING_REQUIREMENT_CONFLICT",
                        format!(
                            "consumer {consumer} requirement {} is active in more than one topology",
                            binding.requirement_name
                        ),
                    ));
                }
                binding.credential_generation = next_generation;
                binding.context_generation = next_generation;
            }
        }
    }
    Ok(())
}

pub(crate) fn binding_context_transition_plans(
    storage: &DurableStore,
    plans: &[StoreTopologyApplyPlan],
    affected_consumers: &BTreeSet<String>,
) -> Result<Vec<BindingContextTransitionPlan>, StoreError> {
    let mut transitions = Vec::new();
    for deployment_id in affected_consumers {
        let runtime = storage
            .runtime_instance(deployment_id)
            .map_err(storage_error)?
            .ok_or_else(|| {
                StoreError::new(
                    409,
                    "STORE_BINDING_CONSUMER_RUNTIME_MISSING",
                    format!("consumer deployment {deployment_id} has no runtime projection"),
                )
            })?;
        if runtime.management_mode != orchestrator_storage::RuntimeManagementMode::Managed {
            return Err(StoreError::new(
                422,
                "STORE_EXTERNAL_BINDING_CONTEXT_REQUIRED",
                format!(
                    "consumer deployment {deployment_id} is External and cannot receive a managed binding context"
                ),
            ));
        }
        let contract = storage
            .service_release_contract(
                &runtime.instance.service_id,
                &runtime.instance.release_version,
            )
            .map_err(storage_error)?
            .ok_or_else(|| {
                StoreError::new(
                    409,
                    "STORE_BINDING_CONSUMER_RELEASE_MISSING",
                    format!(
                        "consumer deployment {deployment_id} has no exact release contract {}@{}",
                        runtime.instance.service_id, runtime.instance.release_version
                    ),
                )
            })?;
        let previous_bindings = active_consumer_bindings(storage, deployment_id)?;
        let desired_bindings = StagedApplyDesiredContextBindings::from_plans(plans, deployment_id)?;
        let derived_previous = managed_service_context_spec(
            storage,
            &contract,
            &runtime.node_id,
            &previous_bindings,
            false,
        )?;
        let persisted = storage
            .get_state::<ManagedServiceContextProjection>(
                "managed-service-context-v1",
                deployment_id,
            )
            .map_err(storage_error)?;
        if let (Some(projected), Some(derived)) = (
            persisted
                .as_ref()
                .and_then(|projection| projection.current.as_ref()),
            derived_previous.as_ref(),
        ) && projected != derived
        {
            return Err(StoreError::new(
                409,
                "STORE_BINDING_CONTEXT_PROJECTION_DRIFT",
                format!(
                    "consumer deployment {deployment_id} persisted Agent context does not match active ApiBindings"
                ),
            ));
        }
        let previous = persisted
            .map(|projection| projection.last_nonempty)
            .or(derived_previous)
            .ok_or_else(|| {
                StoreError::new(
                    409,
                    "STORE_BINDING_PREVIOUS_CONTEXT_REQUIRED",
                    format!("consumer deployment {deployment_id} has no previous managed context"),
                )
            })?;
        // Removing the last required API is an explicit credential/context
        // revocation before uninstall. A partial required set remains invalid,
        // while optional-only and event-only contracts keep their ordinary
        // materialization semantics.
        let desired = if desired_bindings.as_slice().is_empty()
            && contract
                .requirements()
                .iter()
                .any(|requirement| !requirement.optional())
        {
            None
        } else {
            managed_service_context_spec(
                storage,
                &contract,
                &runtime.node_id,
                desired_bindings.as_slice(),
                false,
            )?
        };
        let forward = BindingContextApplyPayload {
            deployment_id: deployment_id.clone(),
            service_id: runtime.instance.service_id.clone(),
            context: desired.clone(),
            previous_context: Some(previous.clone()),
        };
        let rollback = BindingContextApplyPayload {
            deployment_id: deployment_id.clone(),
            service_id: runtime.instance.service_id.clone(),
            context: Some(previous.clone()),
            previous_context: desired.or(Some(previous)),
        };
        forward.validate().map_err(|error| {
            StoreError::new(500, "STORE_BINDING_CONTEXT_INVALID", error.to_string())
        })?;
        rollback.validate().map_err(|error| {
            StoreError::new(500, "STORE_BINDING_CONTEXT_INVALID", error.to_string())
        })?;
        transitions.push(BindingContextTransitionPlan {
            deployment_id: deployment_id.clone(),
            node_id: runtime.node_id,
            container_id: runtime.instance.container_id,
            forward,
            rollback,
        });
    }
    transitions.sort_by(|left, right| left.deployment_id.cmp(&right.deployment_id));
    Ok(transitions)
}

pub(crate) fn active_consumer_bindings(
    storage: &DurableStore,
    deployment_id: &str,
) -> Result<Vec<ApiBinding>, StoreError> {
    let mut bindings = storage
        .api_bindings_for_deployment(deployment_id)
        .map_err(storage_error)?
        .into_iter()
        .filter(|binding| {
            binding.desired_state == "ACTIVE" && binding.state == ApiBindingState::Active
        })
        .collect::<Vec<_>>();
    bindings.sort_by(|left, right| left.requirement_name.cmp(&right.requirement_name));
    Ok(bindings)
}

pub(crate) fn active_provider_bindings(
    storage: &DurableStore,
    deployment_id: &str,
) -> Result<Vec<ApiBinding>, StoreError> {
    let mut bindings = Vec::new();
    for heads in storage.list_topology_heads().map_err(storage_error)? {
        bindings.extend(
            storage
                .api_bindings_for_topology(&heads.topology_id)
                .map_err(storage_error)?
                .into_iter()
                .filter(|binding| {
                    binding.provider_deployment_id == deployment_id
                        && binding.desired_state == "ACTIVE"
                        && binding.state == ApiBindingState::Active
                }),
        );
    }
    bindings.sort_by(|left, right| {
        (
            &left.topology_id,
            &left.consumer_deployment_id,
            &left.requirement_name,
        )
            .cmp(&(
                &right.topology_id,
                &right.consumer_deployment_id,
                &right.requirement_name,
            ))
    });
    Ok(bindings)
}

pub(crate) fn require_matching_replacement_topologies<'a>(
    selected: &'a [InstallTopologySelection],
    existing: &[ApiBinding],
) -> Result<Vec<&'a InstallTopologySelection>, StoreError> {
    if selected.is_empty() {
        return Err(StoreError::new(
            422,
            "STORE_REPLACEMENT_TOPOLOGY_REQUIRED",
            "a topology-bound replacement requires strong ETag confirmation for every affected topology",
        ));
    }
    let topology_ids = existing
        .iter()
        .map(|binding| binding.topology_id.as_str())
        .filter(|topology_id| !topology_id.is_empty())
        .collect::<BTreeSet<_>>();
    if topology_ids.is_empty() {
        if selected.len() != 1 {
            return Err(StoreError::new(
                422,
                "STORE_REPLACEMENT_TOPOLOGY_AMBIGUOUS",
                "a consumer without an existing binding authority requires exactly one topology CAS input",
            ));
        }
        return Ok(vec![&selected[0]]);
    }
    let selected_ids = selected
        .iter()
        .map(|selection| selection.topology_id.as_str())
        .collect::<BTreeSet<_>>();
    if selected_ids != topology_ids {
        return Err(StoreError::new(
            409,
            "STORE_REPLACEMENT_TOPOLOGY_CONFLICT",
            format!(
                "replacement topology CAS set {:?} must exactly match affected applied topologies {:?}",
                selected_ids, topology_ids
            ),
        ));
    }
    Ok(selected
        .iter()
        .filter(|selection| topology_ids.contains(selection.topology_id.as_str()))
        .collect())
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn propose_provider_replacement_topology(
    storage: &DurableStore,
    selection: &InstallTopologySelection,
    contract: &ServiceReleaseContract,
    old_deployment_id: &str,
    new_deployment_id: &str,
    new_node_id: &str,
    new_endpoint: &str,
    operation_id: &str,
) -> Result<StoreTopologyApplyPlan, StoreError> {
    let (mut spec, expected_draft) = selected_topology_spec(storage, selection)?;
    let new_endpoint = required_text(new_endpoint, "endpoint")?;
    validate_endpoint_id(new_endpoint).map_err(|error| {
        StoreError::new(
            422,
            "STORE_REPLACEMENT_PROVIDER_ENDPOINT_INVALID",
            error.to_string(),
        )
    })?;
    let identity = parse_endpoint_id(new_endpoint).map_err(|error| {
        StoreError::new(
            422,
            "STORE_REPLACEMENT_PROVIDER_ENDPOINT_INVALID",
            error.to_string(),
        )
    })?;
    if identity.service_name != contract.release.service_name {
        return Err(StoreError::new(
            422,
            "STORE_REPLACEMENT_PROVIDER_ENDPOINT_MISMATCH",
            format!(
                "replacement endpoint service {} must match {}",
                identity.service_name, contract.release.service_name
            ),
        ));
    }
    let previous_bindings = storage
        .api_bindings_for_topology(&spec.topology_id)
        .map_err(storage_error)?;
    let affected = previous_bindings
        .iter()
        .filter(|binding| {
            binding.provider_deployment_id == old_deployment_id
                && binding.desired_state == "ACTIVE"
                && binding.state == ApiBindingState::Active
        })
        .map(|binding| binding.binding_id.as_str())
        .collect::<BTreeSet<_>>();
    if affected.is_empty() {
        return Err(StoreError::new(
            409,
            "STORE_REPLACEMENT_PROVIDER_BINDINGS_MISSING",
            format!(
                "deployment {old_deployment_id} has no active provider binding in topology {}",
                spec.topology_id
            ),
        ));
    }

    let old_targets = previous_bindings
        .iter()
        .filter(|binding| affected.contains(binding.binding_id.as_str()))
        .map(|binding| binding.link_target_endpoint.as_str())
        .collect::<BTreeSet<_>>();
    if old_targets.len() != 1 {
        return Err(StoreError::new(
            409,
            "STORE_REPLACEMENT_PROVIDER_ENDPOINT_AMBIGUOUS",
            "active provider bindings do not share one topology endpoint",
        ));
    }
    let old_target = *old_targets.first().expect("one target was checked");
    match spec
        .endpoints
        .iter_mut()
        .find(|endpoint| endpoint.endpoint == new_endpoint)
    {
        Some(endpoint) if endpoint.service_id != contract.release.service_name => {
            return Err(StoreError::new(
                409,
                "STORE_REPLACEMENT_PROVIDER_ENDPOINT_CONFLICT",
                format!(
                    "endpoint {new_endpoint} already belongs to {}",
                    endpoint.service_id
                ),
            ));
        }
        Some(endpoint) => {
            endpoint.protocol = contract.release.backend.protocol.clone();
            endpoint.health_path = contract.release.backend.health_path.clone();
            endpoint.config = json!({
                "deployment_id": new_deployment_id,
                "node_id": new_node_id,
            });
        }
        None => spec.endpoints.push(TopologyEndpointSpec {
            endpoint: new_endpoint.to_string(),
            service_id: contract.release.service_name.clone(),
            protocol: contract.release.backend.protocol.clone(),
            health_path: contract.release.backend.health_path.clone(),
            display_name: contract.release.service_name.clone(),
            note: "Store-managed replacement provider endpoint".to_string(),
            config: json!({
                "deployment_id": new_deployment_id,
                "node_id": new_node_id,
            }),
        }),
    }
    let mut rewritten_links = Vec::with_capacity(spec.links.len() + 1);
    for mut link in std::mem::take(&mut spec.links) {
        if link.target_endpoint != old_target {
            rewritten_links.push(link);
            continue;
        }
        let replacement_template = link.clone();
        let (mut affected_bindings, retained_bindings): (Vec<_>, Vec<_>) = link
            .api_bindings
            .into_iter()
            .partition(|binding| binding.provider_deployment_id == old_deployment_id);
        if affected_bindings.is_empty() {
            link.api_bindings = retained_bindings;
            rewritten_links.push(link);
            continue;
        }
        for binding in &mut affected_bindings {
            binding.provider_deployment_id = new_deployment_id.to_string();
        }
        if !retained_bindings.is_empty() {
            // A Link may aggregate multiple named requirements that happen to
            // share an endpoint. Move only the affected requirements to a new
            // target Link and leave unrelated provider bindings untouched.
            let mut replacement_link = replacement_template;
            replacement_link.target_endpoint = new_endpoint.to_string();
            replacement_link.api_bindings = affected_bindings;
            link.api_bindings = retained_bindings;
            rewritten_links.push(link);
            rewritten_links.push(replacement_link);
        } else {
            link.target_endpoint = new_endpoint.to_string();
            link.api_bindings = affected_bindings;
            rewritten_links.push(link);
        }
    }
    spec.links = rewritten_links;
    if spec.root_endpoint == old_target {
        spec.root_endpoint = new_endpoint.to_string();
    }
    if old_target != spec.root_endpoint
        && !spec
            .links
            .iter()
            .any(|link| link.source_endpoint == old_target || link.target_endpoint == old_target)
    {
        spec.endpoints
            .retain(|endpoint| endpoint.endpoint != old_target);
    }
    // Compatibility is a plan-time precondition. Validate it before creating
    // the immutable draft so a rejected upgrade cannot leave an unusable
    // revision behind.
    for binding in previous_bindings
        .iter()
        .filter(|binding| affected.contains(binding.binding_id.as_str()))
    {
        let (link, selection) = spec
            .links
            .iter()
            .flat_map(|link| {
                link.api_bindings
                    .iter()
                    .map(move |selection| (link, selection))
            })
            .find(|(link, selection)| {
                link.source_endpoint == binding.link_source_endpoint
                    && selection.requirement_name == binding.requirement_name
                    && selection.provider_deployment_id == new_deployment_id
            })
            .ok_or_else(|| {
                StoreError::new(
                    500,
                    "STORE_REPLACEMENT_BINDING_REVISION_INVALID",
                    format!(
                        "replacement Link for binding {} is missing",
                        binding.binding_id
                    ),
                )
            })?;
        let version_requirement = if selection.version.trim().is_empty() {
            binding.api_version.as_str()
        } else {
            selection.version.as_str()
        };
        if !contract.release.apis.iter().any(|api| {
            api.api_id == binding.api_id
                && api.protocol == link.protocol
                && api_version_matches(version_requirement, &api.version)
        }) {
            return Err(StoreError::new(
                422,
                "STORE_REPLACEMENT_PROVIDER_API_INCOMPATIBLE",
                format!(
                    "replacement release does not provide {} compatible with {}",
                    binding.api_id, version_requirement
                ),
            ));
        }
    }
    spec = spec.canonicalized().map_err(core_error)?;
    let topology_id = spec.topology_id.clone();
    let revision = storage
        .create_next_topology_revision(
            &topology_id,
            &expected_draft,
            spec,
            now_marker(),
            "store-replacement".to_string(),
            format!("switch provider {old_deployment_id} to {new_deployment_id}"),
        )
        .map_err(storage_error)?;

    let mut desired = previous_bindings
        .iter()
        .filter(|binding| {
            binding.desired_state == "ACTIVE" && binding.state == ApiBindingState::Active
        })
        .cloned()
        .collect::<Vec<_>>();
    for binding in &mut desired {
        binding.topology_revision_id = revision.revision_id().to_string();
        binding.last_operation_id = operation_id.to_string();
        if !affected.contains(binding.binding_id.as_str()) {
            continue;
        }
        let topology_binding = revision
            .spec()
            .links
            .iter()
            .flat_map(|link| {
                link.api_bindings
                    .iter()
                    .map(move |selection| (link, selection))
            })
            .find(|(link, selection)| {
                link.source_endpoint == binding.link_source_endpoint
                    && selection.requirement_name == binding.requirement_name
                    && selection.provider_deployment_id == new_deployment_id
            })
            .ok_or_else(|| {
                StoreError::new(
                    500,
                    "STORE_REPLACEMENT_BINDING_REVISION_INVALID",
                    format!(
                        "replacement Link for binding {} is missing",
                        binding.binding_id
                    ),
                )
            })?;
        let version_requirement = if topology_binding.1.version.trim().is_empty() {
            binding.api_version.as_str()
        } else {
            topology_binding.1.version.as_str()
        };
        let provider_api = contract
            .release
            .apis
            .iter()
            .find(|api| {
                api.api_id == binding.api_id
                    && api.protocol == topology_binding.0.protocol
                    && api_version_matches(version_requirement, &api.version)
            })
            .ok_or_else(|| {
                StoreError::new(
                    422,
                    "STORE_REPLACEMENT_PROVIDER_API_INCOMPATIBLE",
                    format!(
                        "replacement release does not provide {} compatible with {}",
                        binding.api_id, version_requirement
                    ),
                )
            })?;
        binding.api_version = provider_api.version.clone();
        binding.provider_deployment_id = new_deployment_id.to_string();
        binding.provider_service_id = contract.release.service_name.clone();
        binding.provider_node_id = new_node_id.to_string();
        binding.provider_endpoint = new_endpoint.to_string();
        binding.provider_path = provider_api.path_prefix.clone();
        binding.protocol = provider_api.protocol.clone();
        binding.methods = provider_api.methods.clone();
        binding.provider_auth_mode = provider_api.auth_mode.clone();
        binding.permission = provider_api.permission.clone();
        binding.link_target_endpoint = new_endpoint.to_string();
    }
    let staged_bindings = storage
        .stage_precomputed_topology_api_bindings(
            revision.spec().topology_id.as_str(),
            revision.revision_id(),
            operation_id,
            desired,
        )
        .map_err(|error| {
            StoreError::new(422, "STORE_BINDING_TOPOLOGY_INVALID", error.to_string())
        })?;
    Ok(StoreTopologyApplyPlan {
        topology_id: revision.spec().topology_id.clone(),
        revision_id: revision.revision_id().to_string(),
        staged_bindings,
        previous_bindings,
    })
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn propose_dual_role_replacement_topology(
    storage: &DurableStore,
    selection: &InstallTopologySelection,
    contract: &ServiceReleaseContract,
    old_deployment_id: &str,
    new_deployment_id: &str,
    new_node_id: &str,
    new_endpoint: &str,
    consumer_bindings: &[ApiBinding],
    operation_id: &str,
) -> Result<StoreTopologyApplyPlan, StoreError> {
    let (mut spec, expected_draft) = selected_topology_spec(storage, selection)?;
    validate_endpoint_id(new_endpoint).map_err(|error| {
        StoreError::new(422, "STORE_REPLACEMENT_ENDPOINT_INVALID", error.to_string())
    })?;
    let previous_bindings = storage
        .api_bindings_for_topology(&spec.topology_id)
        .map_err(storage_error)?;
    let provider_affected = previous_bindings
        .iter()
        .filter(|binding| {
            binding.provider_deployment_id == old_deployment_id
                && binding.desired_state == "ACTIVE"
                && binding.state == ApiBindingState::Active
        })
        .map(|binding| binding.binding_id.clone())
        .collect::<BTreeSet<_>>();
    if provider_affected.is_empty() {
        return Err(StoreError::new(
            409,
            "STORE_REPLACEMENT_PROVIDER_BINDINGS_MISSING",
            format!(
                "deployment {old_deployment_id} has no provider binding in topology {}",
                spec.topology_id
            ),
        ));
    }
    let old_targets = previous_bindings
        .iter()
        .filter(|binding| provider_affected.contains(&binding.binding_id))
        .map(|binding| binding.link_target_endpoint.clone())
        .collect::<BTreeSet<_>>();
    if old_targets.len() != 1 {
        return Err(StoreError::new(
            409,
            "STORE_REPLACEMENT_PROVIDER_ENDPOINT_AMBIGUOUS",
            "dual-role provider bindings must share one topology endpoint",
        ));
    }
    let old_target = old_targets
        .first()
        .expect("one provider endpoint was checked")
        .clone();
    let old_sources = previous_bindings
        .iter()
        .filter(|binding| binding.consumer_deployment_id == old_deployment_id)
        .map(|binding| binding.link_source_endpoint.clone())
        .collect::<BTreeSet<_>>();
    let old_consumer_requirements = previous_bindings
        .iter()
        .filter(|binding| binding.consumer_deployment_id == old_deployment_id)
        .map(|binding| binding.requirement_name.clone())
        .collect::<BTreeSet<_>>();

    let endpoint_config = json!({
        "deployment_id": new_deployment_id,
        "node_id": new_node_id,
        "outbound_only": false,
    });
    match spec
        .endpoints
        .iter_mut()
        .find(|endpoint| endpoint.endpoint == new_endpoint)
    {
        Some(endpoint) if endpoint.service_id != contract.release.service_name => {
            return Err(StoreError::new(
                409,
                "STORE_REPLACEMENT_ENDPOINT_CONFLICT",
                format!("endpoint {new_endpoint} belongs to {}", endpoint.service_id),
            ));
        }
        Some(endpoint) => {
            endpoint.protocol = contract.release.backend.protocol.clone();
            endpoint.health_path = contract.release.backend.health_path.clone();
            endpoint.config = endpoint_config;
        }
        None => spec.endpoints.push(TopologyEndpointSpec {
            endpoint: new_endpoint.to_string(),
            service_id: contract.release.service_name.clone(),
            protocol: contract.release.backend.protocol.clone(),
            health_path: contract.release.backend.health_path.clone(),
            display_name: contract.release.service_name.clone(),
            note: "Store-managed dual-role replacement endpoint".to_string(),
            config: endpoint_config,
        }),
    }

    // Remove only the old consumer's named requirements. Links may aggregate
    // unrelated requirements and must remain intact.
    for link in &mut spec.links {
        if old_sources.contains(&link.source_endpoint) {
            link.api_bindings
                .retain(|binding| !old_consumer_requirements.contains(&binding.requirement_name));
        }
    }
    for binding in consumer_bindings.iter().filter(|binding| {
        binding.desired_state == "ACTIVE"
            && matches!(
                binding.state,
                ApiBindingState::Resolved | ApiBindingState::Active
            )
    }) {
        if !spec
            .endpoints
            .iter()
            .any(|endpoint| endpoint.endpoint == binding.provider_endpoint)
            && binding.provider_deployment_id != old_deployment_id
        {
            return Err(StoreError::new(
                422,
                "STORE_BINDING_PROVIDER_ENDPOINT_MISSING",
                format!(
                    "provider endpoint {} for {} is absent from topology {}",
                    binding.provider_endpoint, binding.requirement_name, spec.topology_id
                ),
            ));
        }
        let target_endpoint = if binding.provider_deployment_id == old_deployment_id {
            old_target.as_str()
        } else {
            binding.provider_endpoint.as_str()
        };
        let link = if let Some(index) = spec.links.iter().position(|link| {
            link.source_endpoint == new_endpoint && link.target_endpoint == target_endpoint
        }) {
            &mut spec.links[index]
        } else {
            spec.links.push(TopologyLinkSpec {
                source_endpoint: new_endpoint.to_string(),
                target_endpoint: target_endpoint.to_string(),
                protocol: binding.protocol.clone(),
                auth_mode: "workload".to_string(),
                scope: "api-binding".to_string(),
                enabled: true,
                config_ref: String::new(),
                secret_ref: String::new(),
                policy: json!({}),
                api_bindings: Vec::new(),
            });
            spec.links.last_mut().expect("dual-role link was inserted")
        };
        link.api_bindings.push(TopologyApiBindingSpec {
            requirement_name: binding.requirement_name.clone(),
            api_id: binding.api_id.clone(),
            version: contract
                .requirements()
                .iter()
                .find(|requirement| requirement.binding_name() == binding.requirement_name)
                .map(|requirement| requirement.version_requirement().to_string())
                .unwrap_or_else(|| binding.api_version.clone()),
            optional: binding.optional,
            provider_deployment_id: binding.provider_deployment_id.clone(),
            selection: "explicit".to_string(),
        });
    }

    // Split mixed target Links and move only bindings supplied by the old
    // provider to the transient replacement endpoint.
    let mut rewritten_links = Vec::with_capacity(spec.links.len() + 1);
    for mut link in std::mem::take(&mut spec.links) {
        if link.target_endpoint != old_target {
            rewritten_links.push(link);
            continue;
        }
        let template = link.clone();
        let (mut affected, retained): (Vec<_>, Vec<_>) = link
            .api_bindings
            .into_iter()
            .partition(|binding| binding.provider_deployment_id == old_deployment_id);
        for binding in &mut affected {
            binding.provider_deployment_id = new_deployment_id.to_string();
        }
        match (affected.is_empty(), retained.is_empty()) {
            (true, _) => {
                link.api_bindings = retained;
                rewritten_links.push(link);
            }
            (false, true) => {
                link.target_endpoint = new_endpoint.to_string();
                link.api_bindings = affected;
                rewritten_links.push(link);
            }
            (false, false) => {
                link.api_bindings = retained;
                let mut replacement = template;
                replacement.target_endpoint = new_endpoint.to_string();
                replacement.api_bindings = affected;
                rewritten_links.push(link);
                rewritten_links.push(replacement);
            }
        }
    }
    spec.links = rewritten_links;
    if spec.root_endpoint == old_target || old_sources.contains(&spec.root_endpoint) {
        spec.root_endpoint = new_endpoint.to_string();
    }
    spec.links
        .retain(|link| !link.api_bindings.is_empty() || link.scope != "api-binding");
    if !spec
        .links
        .iter()
        .any(|link| link.source_endpoint == old_target || link.target_endpoint == old_target)
    {
        spec.endpoints
            .retain(|endpoint| endpoint.endpoint != old_target);
    }
    for old_source in &old_sources {
        if old_source != &spec.root_endpoint
            && !spec.links.iter().any(|link| {
                &link.source_endpoint == old_source || &link.target_endpoint == old_source
            })
        {
            spec.endpoints
                .retain(|endpoint| &endpoint.endpoint != old_source);
        }
    }
    spec = spec.canonicalized().map_err(core_error)?;
    let topology_id = spec.topology_id.clone();
    let revision = storage
        .create_next_topology_revision(
            &topology_id,
            &expected_draft,
            spec,
            now_marker(),
            "store-replacement".to_string(),
            format!("replace dual-role deployment {old_deployment_id} with {new_deployment_id}"),
        )
        .map_err(storage_error)?;

    let mut desired = previous_bindings
        .iter()
        .filter(|binding| {
            binding.desired_state == "ACTIVE"
                && binding.state == ApiBindingState::Active
                && binding.consumer_deployment_id != old_deployment_id
        })
        .cloned()
        .collect::<Vec<_>>();
    desired.extend(consumer_bindings.iter().cloned());
    for binding in &mut desired {
        binding.topology_id = topology_id.clone();
        binding.topology_revision_id = revision.revision_id().to_string();
        binding.last_operation_id = operation_id.to_string();
        if binding.consumer_deployment_id == new_deployment_id {
            binding.consumer_endpoint = new_endpoint.to_string();
            binding.link_source_endpoint = new_endpoint.to_string();
        }
        if binding.provider_deployment_id != old_deployment_id {
            continue;
        }
        let provider_api = contract
            .release
            .apis
            .iter()
            .find(|api| {
                api.api_id == binding.api_id
                    && api_version_matches(&binding.api_version, &api.version)
            })
            .ok_or_else(|| {
                StoreError::new(
                    422,
                    "STORE_REPLACEMENT_PROVIDER_API_INCOMPATIBLE",
                    format!("replacement release cannot provide {}", binding.api_id),
                )
            })?;
        if !provider_auth_supported(contract.contract_version, &provider_api.auth_mode) {
            return Err(StoreError::new(
                422,
                "STORE_BINDING_PROVIDER_AUTH_UNSUPPORTED",
                format!(
                    "replacement provider API {} uses unsupported {} auth",
                    binding.api_id, provider_api.auth_mode
                ),
            ));
        }
        binding.api_version = provider_api.version.clone();
        binding.provider_deployment_id = new_deployment_id.to_string();
        binding.provider_service_id = contract.release.service_name.clone();
        binding.provider_node_id = new_node_id.to_string();
        binding.provider_endpoint = new_endpoint.to_string();
        binding.provider_path = provider_api.path_prefix.clone();
        binding.protocol = provider_api.protocol.clone();
        binding.methods = provider_api.methods.clone();
        binding.provider_auth_mode = provider_api.auth_mode.clone();
        binding.permission = provider_api.permission.clone();
        binding.link_target_endpoint = new_endpoint.to_string();
    }
    let staged_bindings = storage
        .stage_precomputed_topology_api_bindings(
            &topology_id,
            revision.revision_id(),
            operation_id,
            desired,
        )
        .map_err(|error| {
            StoreError::new(422, "STORE_BINDING_TOPOLOGY_INVALID", error.to_string())
        })?;
    Ok(StoreTopologyApplyPlan {
        topology_id,
        revision_id: revision.revision_id().to_string(),
        staged_bindings,
        previous_bindings,
    })
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn stage_replacement_consumer_bootstrap(
    storage: &DurableStore,
    plan: &StoreTopologyApplyPlan,
    old_consumer_deployment_id: &str,
    new_consumer_deployment_id: &str,
    new_consumer_endpoint: &str,
    consumer_bindings: &[ApiBinding],
    operation_id: &str,
) -> Result<Vec<ApiBinding>, StoreError> {
    let mut desired = plan
        .previous_bindings
        .iter()
        .filter(|binding| {
            binding.desired_state == "ACTIVE"
                && binding.state == ApiBindingState::Active
                && binding.consumer_deployment_id != old_consumer_deployment_id
        })
        .cloned()
        .collect::<Vec<_>>();
    desired.extend(consumer_bindings.iter().cloned());
    for binding in &mut desired {
        binding.topology_id = plan.topology_id.clone();
        binding.topology_revision_id = plan.revision_id.clone();
        binding.last_operation_id = operation_id.to_string();
        if binding.consumer_deployment_id == new_consumer_deployment_id {
            binding.consumer_endpoint = new_consumer_endpoint.to_string();
            binding.link_source_endpoint = new_consumer_endpoint.to_string();
        }
    }
    storage
        .stage_precomputed_topology_api_bindings(
            &plan.topology_id,
            &plan.revision_id,
            operation_id,
            desired,
        )
        .map_err(|error| StoreError::new(422, "STORE_BINDING_BOOTSTRAP_INVALID", error.to_string()))
}

pub(crate) struct StoreConsumerBindingMergeContext<'a> {
    pub(crate) consumer_deployment_id: &'a str,
    pub(crate) replaced_consumer_deployment_id: Option<&'a str>,
    pub(crate) consumer_endpoint: &'a str,
    pub(crate) topology_id: &'a str,
    pub(crate) revision_id: &'a str,
    pub(crate) operation_id: &'a str,
}

/// Builds the exact immutable Topology shape that a Store install would

/// propose, without creating a revision or touching apply ownership.  The

/// validate endpoint uses this to return a truthful prospective diff.

pub(crate) fn preview_store_install_topology_spec(
    mut spec: TopologySpec,
    contract: &ServiceReleaseContract,
    consumer_deployment_id: &str,
    consumer_node_id: &str,
    consumer_endpoint: &str,
    bindings: &[ApiBinding],
) -> Result<TopologySpec, StoreError> {
    let consumer_endpoint = required_text(consumer_endpoint, "endpoint")?;
    validate_endpoint_id(consumer_endpoint).map_err(|error| {
        StoreError::new(
            422,
            "STORE_BINDING_CONSUMER_ENDPOINT_INVALID",
            error.to_string(),
        )
    })?;
    let endpoint_config = json!({
        "deployment_id": consumer_deployment_id,
        "node_id": consumer_node_id,
        "outbound_only": true,
    });
    match spec
        .endpoints
        .iter_mut()
        .find(|endpoint| endpoint.endpoint == consumer_endpoint)
    {
        Some(endpoint) if endpoint.service_id != contract.release.service_name => {
            return Err(StoreError::new(
                409,
                "STORE_BINDING_CONSUMER_ENDPOINT_CONFLICT",
                format!(
                    "endpoint {consumer_endpoint} already belongs to {}",
                    endpoint.service_id
                ),
            ));
        }
        Some(endpoint) => {
            endpoint.config = endpoint_config;
            endpoint.protocol = contract.release.backend.protocol.clone();
            endpoint.health_path = contract.release.backend.health_path.clone();
        }
        None => spec.endpoints.push(TopologyEndpointSpec {
            endpoint: consumer_endpoint.to_string(),
            service_id: contract.release.service_name.clone(),
            protocol: contract.release.backend.protocol.clone(),
            health_path: contract.release.backend.health_path.clone(),
            display_name: contract.release.service_name.clone(),
            note: "Store-managed outbound workload endpoint".to_string(),
            config: endpoint_config,
        }),
    }
    for link in &mut spec.links {
        if link.source_endpoint == consumer_endpoint {
            link.api_bindings.clear();
        }
    }
    for binding in bindings.iter().filter(|binding| {
        matches!(
            binding.state,
            ApiBindingState::Resolved | ApiBindingState::Active
        ) && binding.desired_state == "ACTIVE"
    }) {
        let target = spec
            .endpoints
            .iter()
            .find(|endpoint| endpoint.endpoint == binding.provider_endpoint)
            .ok_or_else(|| {
                StoreError::new(
                    422,
                    "STORE_BINDING_PROVIDER_ENDPOINT_MISSING",
                    format!(
                        "provider deployment {} endpoint {} is not present in topology {}",
                        binding.provider_deployment_id, binding.provider_endpoint, spec.topology_id
                    ),
                )
            })?;
        if target.service_id != binding.provider_service_id {
            return Err(StoreError::new(
                422,
                "STORE_BINDING_PROVIDER_ENDPOINT_MISMATCH",
                format!(
                    "provider endpoint {} belongs to {}, expected {}",
                    target.endpoint, target.service_id, binding.provider_service_id
                ),
            ));
        }
        let link = if let Some(index) = spec.links.iter().position(|link| {
            link.source_endpoint == consumer_endpoint
                && link.target_endpoint == binding.provider_endpoint
        }) {
            &mut spec.links[index]
        } else {
            spec.links.push(TopologyLinkSpec {
                source_endpoint: consumer_endpoint.to_string(),
                target_endpoint: binding.provider_endpoint.clone(),
                protocol: binding.protocol.clone(),
                auth_mode: "workload".to_string(),
                scope: "api-binding".to_string(),
                enabled: true,
                config_ref: String::new(),
                secret_ref: String::new(),
                policy: json!({}),
                api_bindings: Vec::new(),
            });
            spec.links.last_mut().expect("link was just inserted")
        };
        if link.protocol != binding.protocol {
            return Err(StoreError::new(
                422,
                "STORE_BINDING_LINK_PROTOCOL_CONFLICT",
                format!(
                    "Link {} -> {} uses {}, but requirement {} needs {}",
                    consumer_endpoint,
                    binding.provider_endpoint,
                    link.protocol,
                    binding.requirement_name,
                    binding.protocol
                ),
            ));
        }
        link.enabled = true;
        link.auth_mode = "workload".to_string();
        let version_requirement = contract
            .requirements()
            .iter()
            .find(|requirement| requirement.binding_name() == binding.requirement_name)
            .map(|requirement| requirement.version_requirement())
            .filter(|version| !version.trim().is_empty())
            .unwrap_or(binding.api_version.as_str());
        link.api_bindings.push(TopologyApiBindingSpec {
            requirement_name: binding.requirement_name.clone(),
            api_id: binding.api_id.clone(),
            version: version_requirement.to_string(),
            optional: binding.optional,
            provider_deployment_id: binding.provider_deployment_id.clone(),
            selection: "explicit".to_string(),
        });
    }
    spec.links
        .retain(|link| !link.api_bindings.is_empty() || link.scope != "api-binding");
    spec.canonicalized().map_err(core_error)
}

pub(crate) fn merge_store_consumer_bindings(
    previous_bindings: &[ApiBinding],
    consumer_bindings: &[ApiBinding],
    context: StoreConsumerBindingMergeContext<'_>,
) -> Vec<ApiBinding> {
    let mut merged = previous_bindings
        .iter()
        .filter(|binding| {
            binding.desired_state == "ACTIVE"
                && binding.state == ApiBindingState::Active
                && binding.consumer_deployment_id != context.consumer_deployment_id
                && context
                    .replaced_consumer_deployment_id
                    .is_none_or(|old| binding.consumer_deployment_id != old)
                && binding.link_source_endpoint != context.consumer_endpoint
        })
        .cloned()
        .collect::<Vec<_>>();
    merged.extend(consumer_bindings.iter().cloned());
    for binding in &mut merged {
        binding.topology_id = context.topology_id.to_string();
        binding.topology_revision_id = context.revision_id.to_string();
        binding.last_operation_id = context.operation_id.to_string();
        if binding.consumer_deployment_id == context.consumer_deployment_id {
            binding.link_source_endpoint = context.consumer_endpoint.to_string();
            binding.link_target_endpoint = binding.provider_endpoint.clone();
        }
    }
    merged
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn propose_store_install_topology(
    storage: &DurableStore,
    selection: &InstallTopologySelection,
    contract: &ServiceReleaseContract,
    consumer_deployment_id: &str,
    consumer_node_id: &str,
    consumer_endpoint: &str,
    bindings: &[ApiBinding],
    operation_id: &str,
    replaced_consumer_deployment_id: Option<&str>,
) -> Result<StoreTopologyApplyPlan, StoreError> {
    let (spec, expected_draft) = selected_topology_spec(storage, selection)?;
    let spec = preview_store_install_topology_spec(
        spec,
        contract,
        consumer_deployment_id,
        consumer_node_id,
        consumer_endpoint,
        bindings,
    )?;
    let consumer_endpoint = required_text(consumer_endpoint, "endpoint")?;
    let topology_id = spec.topology_id.clone();
    let revision = storage
        .create_next_topology_revision(
            &topology_id,
            &expected_draft,
            spec,
            now_marker(),
            "store-install".to_string(),
            format!(
                "bind {} deployment {}",
                contract.release.service_name, consumer_deployment_id
            ),
        )
        .map_err(storage_error)?;
    let previous_bindings = storage
        .api_bindings_for_topology(&revision.spec().topology_id)
        .map_err(storage_error)?;
    // A Store edit owns only the selected consumer. Preserve every other
    // active consumer in the topology so staging a new deployment cannot
    // accidentally revoke unrelated routes.
    let annotated = merge_store_consumer_bindings(
        &previous_bindings,
        bindings,
        StoreConsumerBindingMergeContext {
            consumer_deployment_id,
            replaced_consumer_deployment_id,
            consumer_endpoint,
            topology_id: &revision.spec().topology_id,
            revision_id: revision.revision_id(),
            operation_id,
        },
    );
    let staged_bindings = storage
        .stage_precomputed_topology_api_bindings(
            &revision.spec().topology_id,
            revision.revision_id(),
            operation_id,
            annotated,
        )
        .map_err(|error| {
            StoreError::new(422, "STORE_BINDING_TOPOLOGY_INVALID", error.to_string())
        })?;
    Ok(StoreTopologyApplyPlan {
        topology_id: revision.spec().topology_id.clone(),
        revision_id: revision.revision_id().to_string(),
        staged_bindings,
        previous_bindings,
    })
}
