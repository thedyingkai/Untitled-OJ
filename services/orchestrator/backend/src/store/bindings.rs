//! Store bindings responsibilities.
use crate::durable::DurableStore;
use crate::registry::RegistryContext;
use crate::store::commands::required_text;
use crate::store::context::{now_marker, now_ms};
use crate::store::error::{StoreError, core_error, storage_error};
use orchestrator_core::ApiBinding;
use orchestrator_core::ApiBindingDesiredState;
use orchestrator_core::ApiBindingHealth;
use orchestrator_core::ApiBindingObservedState;
use orchestrator_core::ApiBindingResolutionRequest;
use orchestrator_core::ApiBindingState;
use orchestrator_core::ApiProviderCandidate;
use orchestrator_core::ServiceReleaseContract;
use orchestrator_core::TopologySpec;
use orchestrator_core::api_version_matches;
use orchestrator_core::parse_endpoint_id;
use orchestrator_core::resolve_api_binding_candidate;
use orchestrator_core::validate_endpoint_id;
use orchestrator_manager::store::validation::InstallBindingSelection;
use orchestrator_manager::store::validation::InstallTopologySelection;
use orchestrator_protocol::RuntimeObservedState;
use serde_json::Value;
use serde_json::json;
use sha2::Digest;
use sha2::Sha256;
use std::collections::BTreeMap;
use std::collections::BTreeSet;

#[derive(Debug, Clone)]
pub(crate) struct TopologyBindingContext {
    pub(crate) topology_id: String,
    pub(crate) revision_id: String,
    pub(crate) source_endpoint: String,
    pub(crate) target_endpoint: String,
    pub(crate) api_id: String,
    pub(crate) version: String,
    pub(crate) optional: bool,
    pub(crate) provider_deployment_id: String,
    pub(crate) selection: String,
}

pub(crate) fn topology_contains_provider_candidate(
    spec: &TopologySpec,
    candidate: &ApiProviderCandidate,
) -> bool {
    spec.endpoints.iter().any(|endpoint| {
        endpoint.endpoint == candidate.endpoint
            && endpoint.service_id == candidate.service_id
            && endpoint
                .config
                .as_object()
                .and_then(|config| config.get("deployment_id"))
                .and_then(Value::as_str)
                .filter(|deployment_id| !deployment_id.trim().is_empty())
                .is_none_or(|deployment_id| deployment_id == candidate.deployment_id)
    })
}

pub(crate) fn preview_install_api_bindings(
    registry_context: &RegistryContext,
    storage: &DurableStore,
    contract: &ServiceReleaseContract,
    consumer_node_id: &str,
    requested_consumer_endpoint: &str,
    selections: &[InstallBindingSelection],
    topology: Option<&InstallTopologySelection>,
) -> Result<(bool, Vec<Value>), StoreError> {
    let mut selections_by_name = BTreeMap::new();
    for selection in selections {
        let name = required_text(&selection.name, "bindings[].name")?.to_string();
        let provider = required_text(
            &selection.provider_deployment_id,
            "bindings[].provider_deployment_id",
        )?
        .to_string();
        if selections_by_name.insert(name.clone(), provider).is_some() {
            return Err(StoreError::new(
                422,
                "STORE_BINDING_DUPLICATE",
                format!("binding selection {name} is declared more than once"),
            ));
        }
    }

    let (topology_spec, topology_revision_id) = topology
        .map(|selection| selected_topology_spec(storage, selection))
        .transpose()?
        .map_or((None, String::new()), |(spec, revision_id)| {
            (Some(spec), revision_id)
        });
    let consumer_endpoint = topology_spec
        .as_ref()
        .map(|spec| {
            topology_consumer_endpoint(
                spec,
                &contract.release.service_name,
                requested_consumer_endpoint,
            )
        })
        .transpose()?
        .unwrap_or_else(|| requested_consumer_endpoint.trim().to_string());
    let topology_bindings = topology_spec
        .as_ref()
        .map(|spec| topology_binding_contexts(spec, &topology_revision_id, &consumer_endpoint))
        .transpose()?
        .unwrap_or_default();
    let all_candidates = provider_candidates(registry_context, storage)?;
    let mut requirements = Vec::with_capacity(contract.requirements().len());
    let mut valid = true;
    let mut used_selections = BTreeSet::new();
    let mut used_topology_bindings = BTreeSet::new();

    for requirement in contract.requirements() {
        let name = requirement.binding_name();
        let explicit_provider = selections_by_name.get(name).cloned().unwrap_or_default();
        if !explicit_provider.is_empty() {
            used_selections.insert(name.to_string());
        }
        let topology_binding = topology_bindings.get(name);
        if let Some(binding) = topology_binding {
            used_topology_bindings.insert(name.to_string());
            if binding.api_id != requirement.api_id() {
                return Err(StoreError::new(
                    422,
                    "STORE_BINDING_API_MISMATCH",
                    format!(
                        "topology binding {name} declares {}, but release requires {}",
                        binding.api_id,
                        requirement.api_id()
                    ),
                ));
            }
        }
        let provider_deployment_id = match topology_binding {
            Some(binding)
                if !binding.provider_deployment_id.is_empty()
                    && !explicit_provider.is_empty()
                    && binding.provider_deployment_id != explicit_provider =>
            {
                return Err(StoreError::new(
                    409,
                    "STORE_BINDING_PROVIDER_CONFLICT",
                    format!(
                        "binding {name} selects conflicting providers {} and {}",
                        binding.provider_deployment_id, explicit_provider
                    ),
                ));
            }
            Some(binding) if !binding.provider_deployment_id.is_empty() => {
                binding.provider_deployment_id.clone()
            }
            _ => explicit_provider,
        };
        let optional = topology_binding
            .map(|binding| binding.optional)
            .unwrap_or_else(|| requirement.optional());
        let selection = topology_binding
            .map(|binding| binding.selection.as_str())
            .unwrap_or_else(|| requirement.selection())
            .to_string();
        let version_requirement = topology_binding
            .map(|binding| binding.version.as_str())
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| requirement.version_requirement())
            .to_string();
        let auth_incompatible = all_candidates.iter().any(|candidate| {
            candidate.healthy
                && candidate.api_id == requirement.api_id()
                && api_version_matches(&version_requirement, &candidate.api_version)
                && !provider_auth_supported(contract.contract_version, &candidate.auth_mode)
                && topology_spec
                    .as_ref()
                    .is_none_or(|spec| topology_contains_provider_candidate(spec, candidate))
        });
        let mut candidates = all_candidates
            .iter()
            .filter(|candidate| {
                candidate.healthy
                    && candidate.api_id == requirement.api_id()
                    && api_version_matches(&version_requirement, &candidate.api_version)
                    && provider_auth_supported(contract.contract_version, &candidate.auth_mode)
                    && (selection != "same-node" || candidate.node_id == consumer_node_id)
                    && topology_spec
                        .as_ref()
                        .is_none_or(|spec| topology_contains_provider_candidate(spec, candidate))
                    && topology_binding.is_none_or(|binding| {
                        candidate.endpoint == binding.target_endpoint
                            && (binding.provider_deployment_id.is_empty()
                                || candidate.deployment_id == binding.provider_deployment_id)
                    })
            })
            .cloned()
            .collect::<Vec<_>>();
        candidates.sort_by(|left, right| {
            left.deployment_id
                .cmp(&right.deployment_id)
                .then_with(|| left.endpoint.cmp(&right.endpoint))
        });
        let selection_missing = !provider_deployment_id.is_empty()
            && !candidates
                .iter()
                .any(|candidate| candidate.deployment_id == provider_deployment_id);
        let ambiguous = provider_deployment_id.is_empty() && candidates.len() > 1;
        let missing = candidates.is_empty() || selection_missing;
        if ambiguous || selection_missing || (!optional && missing) {
            valid = false;
        }
        let recommended_provider_deployment_id =
            if !provider_deployment_id.is_empty() && !selection_missing {
                Some(provider_deployment_id.clone())
            } else if candidates.len() == 1 {
                Some(candidates[0].deployment_id.clone())
            } else {
                None
            };
        requirements.push(json!({
            "requirement_name": name,
            "api_id": requirement.api_id(),
            "version": version_requirement,
            "optional": optional,
            "selection": selection,
            "candidates": candidates,
            "recommended_provider_deployment_id": recommended_provider_deployment_id,
            "ambiguous": ambiguous,
            "missing": missing,
            "reason": if missing && auth_incompatible {
                "matching providers use a non-workload upstream auth mode; production ApiBindings support only workload or public upstream auth"
            } else {
                ""
            },
        }));
    }
    if let Some(unused) = selections_by_name
        .keys()
        .find(|name| !used_selections.contains(*name))
    {
        return Err(StoreError::new(
            422,
            "STORE_BINDING_UNKNOWN",
            format!("bindings[] references undeclared requirement {unused}"),
        ));
    }
    if let Some(unused) = topology_bindings
        .keys()
        .find(|name| !used_topology_bindings.contains(*name))
    {
        return Err(StoreError::new(
            422,
            "STORE_BINDING_UNKNOWN",
            format!("topology api_bindings references undeclared requirement {unused}"),
        ));
    }
    Ok((valid, requirements))
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn resolve_install_api_bindings(
    registry_context: &RegistryContext,
    storage: &DurableStore,
    contract: &ServiceReleaseContract,
    consumer_deployment_id: &str,
    consumer_node_id: &str,
    requested_consumer_endpoint: &str,
    selections: &[InstallBindingSelection],
    topology: Option<&InstallTopologySelection>,
    allow_removed_topology_requirements: bool,
) -> Result<Vec<ApiBinding>, StoreError> {
    let mut selections_by_name = BTreeMap::new();
    for selection in selections {
        let name = required_text(&selection.name, "bindings[].name")?.to_string();
        let provider = required_text(
            &selection.provider_deployment_id,
            "bindings[].provider_deployment_id",
        )?
        .to_string();
        if selections_by_name.insert(name.clone(), provider).is_some() {
            return Err(StoreError::new(
                422,
                "STORE_BINDING_DUPLICATE",
                format!("binding selection {name} is declared more than once"),
            ));
        }
    }

    let (topology_spec, topology_revision_id) = topology
        .map(|selection| selected_topology_spec(storage, selection))
        .transpose()?
        .map_or((None, String::new()), |(spec, revision_id)| {
            (Some(spec), revision_id)
        });
    let consumer_endpoint = topology_spec
        .as_ref()
        .map(|spec| {
            topology_consumer_endpoint(
                spec,
                &contract.release.service_name,
                requested_consumer_endpoint,
            )
        })
        .transpose()?
        .unwrap_or_else(|| requested_consumer_endpoint.trim().to_string());
    let topology_bindings = topology_spec
        .as_ref()
        .map(|spec| topology_binding_contexts(spec, &topology_revision_id, &consumer_endpoint))
        .transpose()?
        .unwrap_or_default();
    let candidates = provider_candidates(registry_context, storage)?;
    let mut resolved = Vec::with_capacity(contract.requirements().len());
    let mut used_selections = BTreeSet::new();
    let mut used_topology_bindings = BTreeSet::new();

    for requirement in contract.requirements() {
        let name = requirement.binding_name();
        let explicit_provider = selections_by_name.get(name).cloned().unwrap_or_default();
        if !explicit_provider.is_empty() {
            used_selections.insert(name.to_string());
        }
        let topology_binding = topology_bindings.get(name);
        if let Some(binding) = topology_binding {
            used_topology_bindings.insert(name.to_string());
            if binding.api_id != requirement.api_id() {
                return Err(StoreError::new(
                    422,
                    "STORE_BINDING_API_MISMATCH",
                    format!(
                        "topology binding {name} declares {}, but release requires {}",
                        binding.api_id,
                        requirement.api_id()
                    ),
                ));
            }
        }
        let provider_deployment_id = match topology_binding {
            Some(binding)
                if !binding.provider_deployment_id.is_empty()
                    && !explicit_provider.is_empty()
                    && binding.provider_deployment_id != explicit_provider =>
            {
                return Err(StoreError::new(
                    409,
                    "STORE_BINDING_PROVIDER_CONFLICT",
                    format!(
                        "binding {name} selects conflicting providers {} and {}",
                        binding.provider_deployment_id, explicit_provider
                    ),
                ));
            }
            Some(binding) if !binding.provider_deployment_id.is_empty() => {
                binding.provider_deployment_id.clone()
            }
            _ => explicit_provider,
        };
        let optional = topology_binding
            .map(|binding| binding.optional)
            .unwrap_or_else(|| requirement.optional());
        let selection = topology_binding
            .map(|binding| binding.selection.as_str())
            .unwrap_or_else(|| requirement.selection())
            .to_string();
        let version_requirement = topology_binding
            .map(|binding| binding.version.as_str())
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| requirement.version_requirement())
            .to_string();

        let rejected_auth = candidates.iter().any(|candidate| {
            candidate.api_id == requirement.api_id()
                && api_version_matches(&version_requirement, &candidate.api_version)
                && !provider_auth_supported(contract.contract_version, &candidate.auth_mode)
                && topology_spec
                    .as_ref()
                    .is_none_or(|spec| topology_contains_provider_candidate(spec, candidate))
                && topology_binding.is_none_or(|binding| {
                    candidate.endpoint == binding.target_endpoint
                        && (binding.provider_deployment_id.is_empty()
                            || candidate.deployment_id == binding.provider_deployment_id)
                })
        });
        let candidate_pool = candidates
            .iter()
            .filter(|candidate| {
                provider_auth_supported(contract.contract_version, &candidate.auth_mode)
                    && topology_spec
                        .as_ref()
                        .is_none_or(|spec| topology_contains_provider_candidate(spec, candidate))
                    && topology_binding.is_none_or(|binding| {
                        candidate.endpoint == binding.target_endpoint
                            && (binding.provider_deployment_id.is_empty()
                                || candidate.deployment_id == binding.provider_deployment_id)
                    })
            })
            .cloned()
            .collect::<Vec<_>>();
        if rejected_auth && candidate_pool.is_empty() {
            return Err(StoreError::new(
                422,
                "STORE_BINDING_PROVIDER_AUTH_UNSUPPORTED",
                format!(
                    "requirement {name} has only non-workload providers; production Deployment SDK calls support workload or public upstream auth"
                ),
            ));
        }
        let request = ApiBindingResolutionRequest {
            requirement_name: name.to_string(),
            api_id: requirement.api_id().to_string(),
            version_requirement: version_requirement.clone(),
            consumer_node_id: consumer_node_id.to_string(),
            provider_deployment_id,
            optional,
            selection,
        };
        let candidate = resolve_api_binding_candidate(&request, &candidate_pool)
            .map_err(|error| StoreError::new(422, "STORE_BINDING_UNRESOLVED", error.to_string()))?;
        let now = now_marker();
        let binding = match candidate {
            Some(candidate) => ApiBinding {
                binding_id: binding_id(consumer_deployment_id, name),
                requirement_name: name.to_string(),
                api_id: requirement.api_id().to_string(),
                api_version: candidate.api_version,
                consumer_deployment_id: consumer_deployment_id.to_string(),
                consumer_service_id: contract.release.service_name.clone(),
                consumer_node_id: consumer_node_id.to_string(),
                consumer_endpoint: consumer_endpoint.clone(),
                provider_deployment_id: candidate.deployment_id,
                provider_service_id: candidate.service_id,
                provider_node_id: candidate.node_id,
                provider_endpoint: candidate.endpoint,
                provider_path: candidate.path,
                virtual_endpoint: format!("/internal/apis/{}", requirement.api_id()),
                protocol: candidate.protocol,
                methods: candidate.methods,
                // `/internal/apis/*` is always a workload-authenticated
                // consumer surface. `provider_auth_mode` separately records
                // how Gateway must authenticate (or not authenticate) to the
                // selected upstream provider. A public upstream therefore
                // still needs a Deployment credential for scoped routing.
                auth_mode: "workload".to_string(),
                provider_auth_mode: candidate.auth_mode,
                permission: candidate.permission,
                timeout_ms: requirement.timeout_ms(),
                topology_id: topology_binding
                    .map(|binding| binding.topology_id.clone())
                    .unwrap_or_default(),
                topology_revision_id: topology_binding
                    .map(|binding| binding.revision_id.clone())
                    .unwrap_or_default(),
                link_source_endpoint: topology_binding
                    .map(|binding| binding.source_endpoint.clone())
                    .unwrap_or_default(),
                link_target_endpoint: topology_binding
                    .map(|binding| binding.target_endpoint.clone())
                    .unwrap_or_default(),
                credential_ref: String::new(),
                credential_generation: 1,
                context_generation: 1,
                desired_state: ApiBindingDesiredState::Active,
                observed_state: ApiBindingObservedState::Resolved,
                health: ApiBindingHealth::Unknown,
                drift: Vec::new(),
                last_operation_id: String::new(),
                state: ApiBindingState::Resolved,
                optional,
                reason: String::new(),
                created_at: now.clone(),
                updated_at: now,
            },
            None => unbound_binding(
                consumer_deployment_id,
                &contract.release.service_name,
                consumer_node_id,
                &consumer_endpoint,
                name,
                requirement.api_id(),
                &version_requirement,
                "no healthy provider currently satisfies this optional requirement",
            ),
        };
        binding.validate().map_err(|error| {
            StoreError::new(
                500,
                "STORE_BINDING_INVALID",
                format!("resolved binding {name} is invalid: {error}"),
            )
        })?;
        resolved.push(binding);
    }

    if let Some(unused) = selections_by_name
        .keys()
        .find(|name| !used_selections.contains(*name))
    {
        return Err(StoreError::new(
            422,
            "STORE_BINDING_UNKNOWN",
            format!("bindings[] references undeclared requirement {unused}"),
        ));
    }
    if !allow_removed_topology_requirements
        && let Some(unused) = topology_bindings
            .keys()
            .find(|name| !used_topology_bindings.contains(*name))
    {
        return Err(StoreError::new(
            422,
            "STORE_BINDING_UNKNOWN",
            format!("topology api_bindings references undeclared requirement {unused}"),
        ));
    }
    resolved.sort_by(|left, right| left.binding_id.cmp(&right.binding_id));
    Ok(resolved)
}

pub(crate) fn production_binding_plan<'a>(
    bindings: impl IntoIterator<Item = &'a ApiBinding>,
) -> Vec<ApiBinding> {
    let mut planned = bindings
        .into_iter()
        .map(|binding| {
            let mut planned = binding.clone();
            // PENDING belongs only to the internal Topology PREPARE payload.
            // Public Store plans and Agent contexts carry a proven provider
            // resolution; activation remains represented by the Operation.
            if planned.state == ApiBindingState::Pending {
                planned.state = ApiBindingState::Resolved;
                planned.observed_state = ApiBindingObservedState::Resolved;
            }
            planned
        })
        .collect::<Vec<_>>();
    planned.sort_by(|left, right| left.binding_id.cmp(&right.binding_id));
    planned
}

pub(crate) fn ensure_managed_api_bindings_ready(
    storage: &DurableStore,
    contract: &ServiceReleaseContract,
    bindings: &[ApiBinding],
    topology: Option<&InstallTopologySelection>,
) -> Result<(), StoreError> {
    if contract.requirements().is_empty() {
        return Ok(());
    }
    let topology = topology.ok_or_else(|| {
        StoreError::new(
            422,
            "STORE_BINDING_TOPOLOGY_REQUIRED",
            "required APIs must be confirmed through an immutable applied Topology revision",
        )
    })?;
    let (spec, _) = selected_topology_spec(storage, topology)?;

    if let Some(binding) = bindings.iter().find(|binding| {
        matches!(
            binding.state,
            ApiBindingState::Pending | ApiBindingState::Error
        )
    }) {
        return Err(StoreError::new(
            422,
            "STORE_BINDING_NOT_READY",
            format!(
                "requirement {} cannot enter a production plan in {:?} state",
                binding.requirement_name, binding.state
            ),
        ));
    }

    for requirement in contract.requirements() {
        let matches = bindings
            .iter()
            .filter(|binding| binding.requirement_name == requirement.binding_name())
            .collect::<Vec<_>>();
        let [binding] = matches.as_slice() else {
            return Err(StoreError::new(
                422,
                "STORE_BINDING_UNRESOLVED",
                format!(
                    "requirement {} must resolve to exactly one binding",
                    requirement.binding_name()
                ),
            ));
        };
        if binding.state == ApiBindingState::Unbound && requirement.optional() {
            continue;
        }
        if !matches!(
            binding.state,
            ApiBindingState::Resolved | ApiBindingState::Active
        ) || binding.provider_deployment_id.trim().is_empty()
            || binding.provider_endpoint.trim().is_empty()
        {
            return Err(StoreError::new(
                422,
                "STORE_BINDING_UNRESOLVED",
                format!(
                    "required API binding {} has no healthy resolved provider",
                    requirement.binding_name()
                ),
            ));
        }
        let provider_endpoint = spec.endpoints.iter().find(|endpoint| {
            endpoint.endpoint == binding.provider_endpoint
                && endpoint.service_id == binding.provider_service_id
        });
        let Some(provider_endpoint) = provider_endpoint else {
            return Err(StoreError::new(
                422,
                "STORE_BINDING_PROVIDER_NOT_APPLIED",
                format!(
                    "provider deployment {} endpoint {} is not present in applied topology {}",
                    binding.provider_deployment_id, binding.provider_endpoint, spec.topology_id
                ),
            ));
        };
        if let Some(configured_deployment_id) = provider_endpoint
            .config
            .as_object()
            .and_then(|config| config.get("deployment_id"))
            .and_then(Value::as_str)
            .filter(|deployment_id| !deployment_id.trim().is_empty())
            && configured_deployment_id != binding.provider_deployment_id
        {
            return Err(StoreError::new(
                409,
                "STORE_BINDING_PROVIDER_NOT_APPLIED",
                format!(
                    "applied topology endpoint {} selects deployment {}, not requested provider {}",
                    provider_endpoint.endpoint,
                    configured_deployment_id,
                    binding.provider_deployment_id
                ),
            ));
        }
    }
    Ok(())
}

pub(crate) fn provider_auth_supported(_contract_version: u32, auth_mode: &str) -> bool {
    // Compatibility manifests may still describe legacy service/internal
    // provider authentication, but a production ApiBinding always uses the
    // workload identity path (or an explicitly public upstream). Legacy auth
    // remains importable and usable by development Compose only.
    matches!(auth_mode, "workload" | "public")
}

pub(crate) fn selected_topology_spec(
    storage: &DurableStore,
    selection: &InstallTopologySelection,
) -> Result<(TopologySpec, String), StoreError> {
    let topology_id = required_text(&selection.topology_id, "topology.topology_id")?;
    let heads = storage
        .topology_heads(topology_id)
        .map_err(storage_error)?
        .ok_or_else(|| {
            StoreError::new(
                404,
                "STORE_BINDING_TOPOLOGY_NOT_FOUND",
                format!("topology {topology_id} was not found"),
            )
        })?;
    if heads.applying_revision_id.is_some() {
        return Err(StoreError::new(
            409,
            "STORE_BINDING_TOPOLOGY_APPLYING",
            format!("topology {topology_id} already has an apply in progress"),
        ));
    }
    let revision_id = heads.applied_revision_id.ok_or_else(|| {
        StoreError::new(
            409,
            "STORE_BINDING_TOPOLOGY_NOT_APPLIED",
            format!(
                "topology {topology_id} has no applied head; apply its initial revision before installing a bound service"
            ),
        )
    })?;
    if selection.revision_id.trim() != revision_id {
        return Err(StoreError::new(
            409,
            "STORE_BINDING_TOPOLOGY_ETAG_CONFLICT",
            format!(
                "topology {topology_id} applied head is {revision_id}, but install confirmed {}",
                selection.revision_id.trim()
            ),
        ));
    }
    if heads.draft_revision_id != revision_id {
        return Err(StoreError::new(
            409,
            "STORE_BINDING_TOPOLOGY_DRAFT_DIVERGED",
            format!(
                "topology {topology_id} has unapplied draft {}; apply or discard it before Store creates a deployment Binding revision",
                heads.draft_revision_id
            ),
        ));
    }
    let revision = storage
        .topology_revision(topology_id, &revision_id)
        .map_err(storage_error)?
        .ok_or_else(|| {
            StoreError::new(
                404,
                "STORE_BINDING_TOPOLOGY_REVISION_NOT_FOUND",
                format!("topology {topology_id} revision {revision_id} was not found"),
            )
        })?;
    Ok((revision.spec().clone(), revision_id))
}

pub(crate) fn topology_consumer_endpoint(
    spec: &TopologySpec,
    service_id: &str,
    requested: &str,
) -> Result<String, StoreError> {
    if !requested.trim().is_empty() {
        validate_endpoint_id(requested.trim()).map_err(|error| {
            StoreError::new(
                422,
                "STORE_BINDING_CONSUMER_ENDPOINT_INVALID",
                format!("consumer endpoint is invalid: {error}"),
            )
        })?;
        let identity = parse_endpoint_id(requested.trim()).map_err(|error| {
            StoreError::new(
                422,
                "STORE_BINDING_CONSUMER_ENDPOINT_INVALID",
                error.to_string(),
            )
        })?;
        if identity.service_name != service_id {
            return Err(StoreError::new(
                422,
                "STORE_BINDING_CONSUMER_ENDPOINT_MISMATCH",
                format!(
                    "consumer endpoint service {} must match {service_id}",
                    identity.service_name
                ),
            ));
        }
        if spec.endpoints.iter().any(|endpoint| {
            endpoint.endpoint == requested.trim() && endpoint.service_id == service_id
        }) {
            return Ok(requested.trim().to_string());
        }
        // Store will add this exact endpoint to the proposed immutable
        // revision. Resolution still uses only explicitly selected providers.
        return Ok(requested.trim().to_string());
    }
    let endpoints = spec
        .endpoints
        .iter()
        .filter(|endpoint| endpoint.service_id == service_id)
        .map(|endpoint| endpoint.endpoint.clone())
        .collect::<Vec<_>>();
    match endpoints.as_slice() {
        [endpoint] => Ok(endpoint.clone()),
        [] => Err(StoreError::new(
            422,
            "STORE_BINDING_CONSUMER_ENDPOINT_REQUIRED",
            format!(
                "topology {} has no endpoint for consumer service {service_id}",
                spec.topology_id
            ),
        )),
        _ => Err(StoreError::new(
            409,
            "STORE_BINDING_CONSUMER_ENDPOINT_AMBIGUOUS",
            format!(
                "topology {} has multiple endpoints for {service_id}; install endpoint is required",
                spec.topology_id
            ),
        )),
    }
}

pub(crate) fn topology_binding_contexts(
    spec: &TopologySpec,
    revision_id: &str,
    consumer_endpoint: &str,
) -> Result<BTreeMap<String, TopologyBindingContext>, StoreError> {
    let mut bindings = BTreeMap::new();
    for link in spec
        .links
        .iter()
        .filter(|link| link.enabled && link.source_endpoint == consumer_endpoint)
    {
        for binding in &link.api_bindings {
            let context = TopologyBindingContext {
                topology_id: spec.topology_id.clone(),
                revision_id: revision_id.to_string(),
                source_endpoint: link.source_endpoint.clone(),
                target_endpoint: link.target_endpoint.clone(),
                api_id: binding.api_id.clone(),
                version: binding.version.clone(),
                optional: binding.optional,
                provider_deployment_id: binding.provider_deployment_id.clone(),
                selection: binding.selection.clone(),
            };
            if bindings
                .insert(binding.requirement_name.clone(), context)
                .is_some()
            {
                return Err(StoreError::new(
                    409,
                    "STORE_BINDING_TOPOLOGY_AMBIGUOUS",
                    format!(
                        "topology {} binds requirement {} through more than one Link",
                        spec.topology_id, binding.requirement_name
                    ),
                ));
            }
        }
    }
    Ok(bindings)
}

pub(crate) fn provider_candidates(
    registry_context: &RegistryContext,
    storage: &DurableStore,
) -> Result<Vec<ApiProviderCandidate>, StoreError> {
    let mut contracts = BTreeMap::new();
    for record in registry_context.service_releases().map_err(core_error)? {
        let Ok(contract) = ServiceReleaseContract::from_json_value(record.manifest.clone()) else {
            continue;
        };
        contracts.insert((record.service_name, record.version), contract);
    }
    let mut candidates = Vec::new();
    let evidence_at_ms = now_ms();
    for stored in storage.runtime_instances(None).map_err(storage_error)? {
        let stored = storage
            .runtime_with_current_evidence(stored, evidence_at_ms)
            .map_err(storage_error)?;
        let healthy = stored.instance.observed_state == RuntimeObservedState::Running
            && stored.instance.health.eq_ignore_ascii_case("HEALTHY");
        let managed_observation_ready =
            if stored.management_mode == orchestrator_storage::RuntimeManagementMode::Managed {
                stored.instance.runtime_attested
                    && stored.drift_reason.is_empty()
                    && storage
                        .managed_runtime_report_unavailable_reason(&stored, evidence_at_ms)
                        .map_err(storage_error)?
                        .is_none()
            } else {
                stored.drift_reason.is_empty()
            };
        if !healthy || !managed_observation_ready || stored.endpoint.trim().is_empty() {
            continue;
        }
        let Some(contract) = contracts.get(&(
            stored.instance.service_id.clone(),
            stored.instance.release_version.clone(),
        )) else {
            continue;
        };
        for api in &contract.release.apis {
            candidates.push(ApiProviderCandidate {
                deployment_id: stored.instance.deployment_id.clone(),
                service_id: stored.instance.service_id.clone(),
                node_id: stored.node_id.clone(),
                endpoint: stored.endpoint.clone(),
                path: api.path_prefix.clone(),
                api_id: api.api_id.clone(),
                api_version: api.version.clone(),
                protocol: api.protocol.clone(),
                methods: api.methods.clone(),
                auth_mode: api.auth_mode.clone(),
                permission: api.permission.clone(),
                healthy,
            });
        }
    }
    Ok(candidates)
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn unbound_binding(
    deployment_id: &str,
    service_id: &str,
    node_id: &str,
    endpoint: &str,
    name: &str,
    api_id: &str,
    version: &str,
    reason: &str,
) -> ApiBinding {
    unresolved_binding(
        deployment_id,
        service_id,
        node_id,
        endpoint,
        name,
        api_id,
        version,
        true,
        ApiBindingState::Unbound,
        reason,
    )
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn unresolved_binding(
    deployment_id: &str,
    service_id: &str,
    node_id: &str,
    endpoint: &str,
    name: &str,
    api_id: &str,
    version: &str,
    optional: bool,
    state: ApiBindingState,
    reason: &str,
) -> ApiBinding {
    let now = now_marker();
    ApiBinding {
        binding_id: binding_id(deployment_id, name),
        requirement_name: name.to_string(),
        api_id: api_id.to_string(),
        api_version: version.to_string(),
        consumer_deployment_id: deployment_id.to_string(),
        consumer_service_id: service_id.to_string(),
        consumer_node_id: node_id.to_string(),
        consumer_endpoint: endpoint.to_string(),
        provider_deployment_id: String::new(),
        provider_service_id: String::new(),
        provider_node_id: String::new(),
        provider_endpoint: String::new(),
        provider_path: String::new(),
        virtual_endpoint: format!("/internal/apis/{api_id}"),
        protocol: String::new(),
        methods: Vec::new(),
        auth_mode: String::new(),
        provider_auth_mode: String::new(),
        permission: String::new(),
        timeout_ms: None,
        topology_id: String::new(),
        topology_revision_id: String::new(),
        link_source_endpoint: String::new(),
        link_target_endpoint: String::new(),
        credential_ref: String::new(),
        credential_generation: 1,
        context_generation: 1,
        desired_state: ApiBindingDesiredState::Active,
        observed_state: match state {
            ApiBindingState::Unbound => ApiBindingObservedState::Revoked,
            ApiBindingState::Error => ApiBindingObservedState::Error,
            _ => ApiBindingObservedState::Pending,
        },
        health: ApiBindingHealth::Unknown,
        drift: Vec::new(),
        last_operation_id: String::new(),
        state,
        optional,
        reason: reason.to_string(),
        created_at: now.clone(),
        updated_at: now,
    }
}

pub(crate) fn binding_id(deployment_id: &str, requirement_name: &str) -> String {
    let digest = Sha256::digest(format!("{deployment_id}\0{requirement_name}").as_bytes());
    format!("binding-{digest:x}")
}
