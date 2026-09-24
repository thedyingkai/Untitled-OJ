//! Upgrade and rollback coordination, including historical artifact proof and topology cutover.
use crate::artifact_store::ArtifactStore;
use crate::catalog_registry::CatalogRegistry;
use crate::contribution_controller::ContributionReplacementDagV1;
use crate::contribution_controller::append_contribution_replacement_job_fragment;
use crate::durable::DurableStore;
use crate::store::admission::{StoreAdmission, TopologyReservation};
use crate::store::artifacts::{
    artifact_matches, missing_resolved_dependencies, offline_artifact_for_release,
};
use crate::store::bindings::{
    production_binding_plan, resolve_install_api_bindings, selected_topology_spec,
};
use crate::store::commands::{
    ReplaceReleaseRequest, non_empty, normalize_replacement_topologies,
    normalize_store_topology_selection, parse_release_channel, required_text,
};
use crate::store::context::{CONTROL_PLANE_NODE_ID, MutationContext, operation_id};
use crate::store::contribution::stage_replacement_contribution;
use crate::store::error::{
    StoreError, catalog_registry_error, contribution_controller_error, core_error, storage_error,
};
use crate::store::history::{provider_revision_from_operation, release_history};
use crate::store::metadata::{ensure_release_checksum, select_catalog_document_release};
use crate::store::node::{
    ensure_ready_docker_node, ensure_release_runtime_supported, release_runtime_contract,
    target_platform,
};
use crate::store::placement::{
    allocate_replacement_endpoint, container_spec, endpoint_socket, ensure_deployment_available,
    ensure_endpoint_available, ensure_no_active_deployment_mutation, ensure_no_active_replacement,
    managed_published_endpoint,
};
use crate::store::runtime_plan::release_pipeline_payload;
use crate::store::service_context::{
    attach_release_runtime_volume, contract_has_retained_runtime_volume,
    managed_service_context_spec,
};
use crate::store::topology::{
    active_consumer_bindings, active_provider_bindings, align_group_binding_generations,
    binding_context_transition_plans, propose_dual_role_replacement_topology,
    propose_generation_sibling_topology, propose_provider_replacement_topology,
    propose_store_install_topology, require_matching_replacement_topologies,
    stage_replacement_consumer_bootstrap,
};
use orchestrator_control_plane::JobKind;
use orchestrator_control_plane::PlanOperation;
use orchestrator_control_plane::PlannedJob;
use orchestrator_control_plane::PlannedJobCondition;
use orchestrator_core::ApiBinding;
use orchestrator_core::ApiBindingState;
use orchestrator_legacy::OrchestratorActionConsole;
use orchestrator_manager::catalog_v2::ReleaseChannel;
use orchestrator_manager::store::composition::release_contract_from_document;
use orchestrator_manager::store::deployment_id;
use orchestrator_manager::store::validation::InstallBindingSelection;
use orchestrator_runtime::HealthGatePolicy;
use orchestrator_runtime::OciImageReference;
use orchestrator_runtime::ReleaseProviderRevision;
use orchestrator_runtime::ReleaseReplacementPayload;
use orchestrator_runtime::ReplacementProviderSaga;
use orchestrator_runtime::RuntimeInstallPayload;
use orchestrator_runtime::RuntimeObservedState;
use orchestrator_runtime::stable_container_name;
use serde_json::Value;
use serde_json::json;
use std::collections::BTreeMap;
use std::collections::BTreeSet;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ReplacementAction {
    Upgrade,
    Rollback,
}

impl ReplacementAction {
    pub(crate) fn action_id(self) -> &'static str {
        match self {
            Self::Upgrade => "release.upgrade",
            Self::Rollback => "release.rollback",
        }
    }

    pub(crate) fn operation_prefix(self) -> &'static str {
        match self {
            Self::Upgrade => "store-upgrade",
            Self::Rollback => "store-rollback",
        }
    }

    pub(crate) fn job_kind(self) -> JobKind {
        match self {
            Self::Upgrade => JobKind::Upgrade,
            Self::Rollback => JobKind::Rollback,
        }
    }

    pub(crate) fn lifecycle(self) -> &'static str {
        match self {
            Self::Upgrade => "Upgrading",
            Self::Rollback => "RollingBack",
        }
    }
}

pub(crate) fn replace_release(
    console: &mut OrchestratorActionConsole,
    storage: &DurableStore,
    catalog_registry: &CatalogRegistry,
    artifact_store: Option<&ArtifactStore>,
    mut input: ReplaceReleaseRequest,
    request: &MutationContext,
    action: ReplacementAction,
) -> Result<Value, StoreError> {
    input.topology = normalize_store_topology_selection(
        &input.topology_id,
        &input.topology_etag,
        input.topology.as_ref(),
    )?;
    let replacement_topologies =
        normalize_replacement_topologies(input.topology.as_ref(), &input.topologies)?;
    let current_deployment_id = required_text(&input.deployment_id, "deployment_id")?;
    let current = storage
        .runtime_instance(current_deployment_id)
        .map_err(storage_error)?
        .ok_or_else(|| {
            StoreError::new(
                404,
                "STORE_DEPLOYMENT_NOT_FOUND",
                format!("deployment {current_deployment_id} was not found"),
            )
        })?;
    if current.instance.container_id.trim().is_empty()
        || current.instance.observed_state != RuntimeObservedState::Running
    {
        return Err(StoreError::new(
            409,
            "STORE_REPLACEMENT_SOURCE_NOT_RUNNING",
            format!(
                "deployment {current_deployment_id} must have a proven container and RUNNING observed state"
            ),
        ));
    }
    let node = storage
        .get_node(&current.node_id)
        .map_err(storage_error)?
        .ok_or_else(|| {
            StoreError::new(
                409,
                "STORE_DEPLOYMENT_NODE_MISSING",
                format!(
                    "deployment {current_deployment_id} references missing Node {}",
                    current.node_id
                ),
            )
        })?;
    ensure_ready_docker_node(storage, &node)?;
    let platform = target_platform(storage, &node)?;
    let history = release_history(storage, &current.instance.service_id)?;
    let current_proof = history
        .iter()
        .find(|proof| {
            proof.deployment_id == current.instance.deployment_id
                && artifact_matches(&proof.image, &current.instance.artifact_digest)
        })
        .cloned()
        .ok_or_else(|| {
            StoreError::new(
                422,
                "STORE_CURRENT_RELEASE_UNPROVEN",
                format!(
                    "deployment {current_deployment_id} has no successful trusted Catalog Operation proving its version and digest"
                ),
            )
        })?;
    let requested_channel = input
        .channel
        .as_deref()
        .map(parse_release_channel)
        .transpose()?;
    let requested_source = non_empty(&input.catalog_source_id);

    let rollback_proof = if action == ReplacementAction::Rollback {
        let requested_version = non_empty(&input.version)
            .map(semver::Version::parse)
            .transpose()
            .map_err(|error| {
                StoreError::new(
                    422,
                    "STORE_ROLLBACK_VERSION_INVALID",
                    format!("rollback version is not semver: {error}"),
                )
            })?;
        Some(
            history
                .iter()
                .find(|proof| {
                    proof.deployment_id != current.instance.deployment_id
                        && !artifact_matches(&proof.image, &current.instance.artifact_digest)
                        && requested_version
                            .as_ref()
                            .is_none_or(|version| version == &proof.version)
                        && requested_source
                            .is_none_or(|source| source == proof.catalog_source_id)
                        && requested_channel
                            .as_ref()
                            .is_none_or(|channel| channel == &proof.channel)
                })
                .cloned()
                .ok_or_else(|| {
                    StoreError::new(
                        422,
                        "STORE_ROLLBACK_HISTORY_UNPROVEN",
                        requested_version.map_or_else(
                            || {
                                format!(
                                    "service {} has no prior successful trusted Catalog release distinct from the current deployment",
                                    current.instance.service_id
                                )
                            },
                            |version| {
                                format!(
                                    "service {} has no successful trusted Catalog Operation proving rollback target {version}",
                                    current.instance.service_id
                                )
                            },
                        ),
                    )
                })?,
        )
    } else {
        None
    };
    let channel = match action {
        ReplacementAction::Upgrade => requested_channel.unwrap_or(ReleaseChannel::Stable),
        ReplacementAction::Rollback => requested_channel.unwrap_or_else(|| {
            rollback_proof
                .as_ref()
                .map(|proof| proof.channel)
                .unwrap_or(ReleaseChannel::Stable)
        }),
    };

    let target_version = match action {
        ReplacementAction::Upgrade => non_empty(&input.version).map(str::to_string),
        ReplacementAction::Rollback => rollback_proof
            .as_ref()
            .map(|proof| proof.version.to_string()),
    };
    let source_id = requested_source.or_else(|| {
        rollback_proof
            .as_ref()
            .map(|proof| proof.catalog_source_id.as_str())
            .or(Some(current_proof.catalog_source_id.as_str()))
    });
    let resolved = catalog_registry
        .resolve_install_plan(
            storage,
            source_id,
            &current.instance.service_id,
            target_version.as_deref(),
            channel,
            platform.clone(),
        )
        .map_err(catalog_registry_error)?;
    let root_release = resolved
        .plan
        .releases
        .last()
        .filter(|release| release.module_id == current.instance.service_id)
        .cloned()
        .ok_or_else(|| {
            StoreError::new(
                500,
                "CATALOG_PLAN_INVALID",
                "resolved replacement plan does not end with its requested root",
            )
        })?;
    if action == ReplacementAction::Upgrade && root_release.release.version <= current_proof.version
    {
        return Err(StoreError::new(
            422,
            "STORE_UPGRADE_VERSION_NOT_NEWER",
            format!(
                "upgrade target {} must be newer than proven current version {}; use rollback for an older release",
                root_release.release.version, current_proof.version
            ),
        ));
    }
    if artifact_matches(
        root_release.release.oci_image.as_str(),
        &current.instance.artifact_digest,
    ) {
        return Err(StoreError::new(
            409,
            "STORE_RELEASE_ALREADY_INSTALLED",
            format!(
                "deployment {current_deployment_id} already uses {}",
                root_release.release.oci_image
            ),
        ));
    }
    if let Some(proof) = &rollback_proof
        && proof.image != root_release.release.oci_image.as_str()
    {
        return Err(StoreError::new(
            422,
            "STORE_ROLLBACK_HISTORY_MISMATCH",
            format!(
                "trusted catalog now resolves {}@{} to {}, but historical Operation {} proves {}; rollback refuses a changed artifact",
                proof.service_id,
                proof.version,
                root_release.release.oci_image,
                proof.operation_id,
                proof.image
            ),
        ));
    }

    let documents = catalog_registry
        .fetch_release_documents(storage, &resolved)
        .map_err(catalog_registry_error)?;
    let missing_dependencies =
        missing_resolved_dependencies(storage, &resolved, &current.instance.service_id)?;
    for document in &documents {
        let deployed_by_operation = document.selection.module_id == current.instance.service_id
            || missing_dependencies.iter().any(|dependency| {
                dependency.module_id == document.selection.module_id
                    && dependency.release.version == document.selection.release.version
            });
        if deployed_by_operation {
            ensure_release_runtime_supported(
                storage,
                &node,
                &release_contract_from_document(document)?,
                document.selection.release.oci_image.as_str(),
            )?;
        }
    }
    let mut imported = Vec::with_capacity(documents.len());
    for document in &documents {
        imported.push(
            console
                .register_external_release_document(
                    &document.bytes,
                    &document.source_url,
                    &document.checksum,
                )
                .map_err(core_error)?,
        );
    }
    let selected = select_catalog_document_release(
        console,
        &documents,
        &current.instance.service_id,
        &root_release.release.version,
    )?;
    ensure_release_checksum(&selected.record)?;
    let image =
        OciImageReference::parse(root_release.release.oci_image.as_str()).map_err(|error| {
            StoreError::new(
                422,
                "STORE_IMMUTABLE_IMAGE_REQUIRED",
                format!("catalog replacement image is invalid: {error}"),
            )
        })?;
    let new_deployment_id = deployment_id(
        &current.instance.service_id,
        &root_release.release.version,
        &node.node_id,
    );
    let operation_target = format!("{}->{}", current.instance.deployment_id, new_deployment_id);
    let operation_id = operation_id(action.operation_prefix(), &operation_target, request)?;
    let staged_contribution = stage_replacement_contribution(
        storage,
        &operation_id,
        &current.instance.deployment_id,
        &new_deployment_id,
        &selected.contract,
        root_release.release.oci_image.digest().as_str(),
    )?;
    let requested_replacement_endpoint = if input.endpoint.trim().is_empty() {
        current.endpoint.as_str()
    } else {
        input.endpoint.trim()
    };
    let replacement_endpoint = if !current.endpoint.trim().is_empty()
        && endpoint_socket(&current.endpoint) == endpoint_socket(requested_replacement_endpoint)
    {
        allocate_replacement_endpoint(
            storage,
            &current.endpoint,
            &current.instance.service_id,
            &new_deployment_id,
        )?
    } else {
        requested_replacement_endpoint.to_string()
    };
    let published_endpoint = managed_published_endpoint(
        &replacement_endpoint,
        &current.instance.service_id,
        &node,
        &selected.manifest,
    )?;
    let mut spec = container_spec(
        &new_deployment_id,
        &current.instance.service_id,
        &root_release.release.version,
        &selected.record.checksum,
        &node,
        image,
        release_runtime_contract(&selected.contract)?,
        &selected.manifest,
        published_endpoint,
    );
    spec.labels.insert(
        "ojos.service_contract_version".to_string(),
        selected.contract.contract_version.to_string(),
    );
    attach_release_runtime_volume(&mut spec, &selected.contract)?;
    let current_consumer_bindings = active_consumer_bindings(storage, current_deployment_id)?;
    let current_provider_bindings = active_provider_bindings(storage, current_deployment_id)?;
    let is_topology_consumer =
        !current_consumer_bindings.is_empty() || !selected.contract.requirements().is_empty();
    let is_topology_provider = !current_provider_bindings.is_empty();
    let mut replacement_bindings = Vec::new();
    let mut topology_applies = Vec::new();
    let mut topology_bootstrap_bindings = BTreeMap::<String, Vec<ApiBinding>>::new();
    let mut binding_context_transitions = Vec::new();
    if is_topology_consumer || is_topology_provider {
        let provider_consumers = current_provider_bindings
            .iter()
            .map(|binding| binding.consumer_deployment_id.clone())
            .collect::<BTreeSet<_>>();
        let mut replacement_scope_bindings = current_consumer_bindings.clone();
        replacement_scope_bindings.extend(current_provider_bindings.iter().cloned());
        for consumer in &provider_consumers {
            replacement_scope_bindings.extend(active_consumer_bindings(storage, consumer)?);
        }
        replacement_scope_bindings.sort_by(|left, right| left.binding_id.cmp(&right.binding_id));
        replacement_scope_bindings.dedup_by(|left, right| left.binding_id == right.binding_id);
        let topologies = require_matching_replacement_topologies(
            &replacement_topologies,
            &replacement_scope_bindings,
        )?;
        for topology in &topologies {
            selected_topology_spec(storage, topology)?;
        }

        let mut consumer_bindings_by_topology = BTreeMap::<String, Vec<ApiBinding>>::new();
        if is_topology_consumer {
            // Existing requirement/provider choices are the deterministic
            // defaults; explicit request mappings override them. This avoids
            // silently choosing another healthy provider during replacement.
            let mut effective_selections = current_consumer_bindings
                .iter()
                .map(|binding| {
                    (
                        binding.requirement_name.clone(),
                        binding.provider_deployment_id.clone(),
                    )
                })
                .collect::<BTreeMap<_, _>>();
            for binding in &input.bindings {
                effective_selections
                    .insert(binding.name.clone(), binding.provider_deployment_id.clone());
            }
            let effective_selections = effective_selections
                .into_iter()
                .map(|(name, provider_deployment_id)| InstallBindingSelection {
                    name,
                    provider_deployment_id,
                })
                .collect::<Vec<_>>();
            replacement_bindings = resolve_install_api_bindings(
                console,
                storage,
                &selected.contract,
                &new_deployment_id,
                &node.node_id,
                &replacement_endpoint,
                &effective_selections,
                None,
                true,
            )?;
            for binding in replacement_bindings.iter_mut().filter(|binding| {
                binding.desired_state == "ACTIVE"
                    && matches!(
                        binding.state,
                        ApiBindingState::Resolved | ApiBindingState::Active
                    )
            }) {
                let topology_id = current_consumer_bindings
                    .iter()
                    .find(|current| current.requirement_name == binding.requirement_name)
                    .map(|current| current.topology_id.clone())
                    .or_else(|| {
                        (topologies.len() == 1).then(|| topologies[0].topology_id.clone())
                    })
                    .ok_or_else(|| {
                        StoreError::new(
                            422,
                            "STORE_REPLACEMENT_BINDING_TOPOLOGY_AMBIGUOUS",
                            format!(
                                "new requirement {} needs an explicit topology, but replacement spans multiple topologies",
                                binding.requirement_name
                            ),
                        )
                    })?;
                binding.topology_id = topology_id.clone();
                binding.link_source_endpoint = replacement_endpoint.clone();
                consumer_bindings_by_topology
                    .entry(topology_id)
                    .or_default()
                    .push(binding.clone());
            }
        }

        for topology in topologies {
            let consumer_bindings = consumer_bindings_by_topology
                .get(&topology.topology_id)
                .map(Vec::as_slice)
                .unwrap_or(&[]);
            let provider_affected = current_provider_bindings
                .iter()
                .any(|binding| binding.topology_id == topology.topology_id);
            let planned = match (!consumer_bindings.is_empty(), provider_affected) {
                (true, true) => propose_dual_role_replacement_topology(
                    storage,
                    topology,
                    &selected.contract,
                    current_deployment_id,
                    &new_deployment_id,
                    &node.node_id,
                    &replacement_endpoint,
                    consumer_bindings,
                    &operation_id,
                )?,
                (true, false) => propose_store_install_topology(
                    storage,
                    topology,
                    &selected.contract,
                    &new_deployment_id,
                    &node.node_id,
                    &replacement_endpoint,
                    consumer_bindings,
                    &operation_id,
                    Some(current_deployment_id),
                )?,
                (false, true) => propose_provider_replacement_topology(
                    storage,
                    topology,
                    &selected.contract,
                    current_deployment_id,
                    &new_deployment_id,
                    &node.node_id,
                    &replacement_endpoint,
                    &operation_id,
                )?,
                (false, false) => {
                    propose_generation_sibling_topology(storage, topology, &operation_id)?
                }
            };
            if !consumer_bindings.is_empty() && provider_affected {
                let bootstrap = stage_replacement_consumer_bootstrap(
                    storage,
                    &planned,
                    current_deployment_id,
                    &new_deployment_id,
                    &replacement_endpoint,
                    consumer_bindings,
                    &operation_id,
                )?;
                topology_bootstrap_bindings.insert(planned.topology_id.clone(), bootstrap);
            }
            topology_applies.push(planned);
        }
        let mut generation_consumers = provider_consumers.clone();
        if is_topology_consumer {
            generation_consumers.insert(new_deployment_id.clone());
        }
        align_group_binding_generations(storage, &mut topology_applies, &generation_consumers)?;
        if is_topology_consumer {
            replacement_bindings = production_binding_plan(
                topology_applies
                    .iter()
                    .flat_map(|topology| topology.staged_bindings.iter())
                    .filter(|binding| {
                        binding.consumer_deployment_id == new_deployment_id
                            && binding.desired_state == "ACTIVE"
                    }),
            );
            replacement_bindings
                .sort_by(|left, right| left.requirement_name.cmp(&right.requirement_name));
        }
        let existing_context_consumers = provider_consumers
            .into_iter()
            .filter(|consumer| consumer != current_deployment_id)
            .collect::<BTreeSet<_>>();
        if !existing_context_consumers.is_empty() {
            binding_context_transitions = binding_context_transition_plans(
                storage,
                &topology_applies,
                &existing_context_consumers,
            )?;
        }
    } else if !replacement_topologies.is_empty() {
        return Err(StoreError::new(
            422,
            "STORE_REPLACEMENT_TOPOLOGY_UNUSED",
            "replacement supplied topology concurrency fields but the deployment has no API Binding role",
        ));
    }
    if is_topology_consumer
        || !selected.contract.events.publishes.is_empty()
        || !selected.contract.events.subscribes.is_empty()
        || contract_has_retained_runtime_volume(&selected.contract)
    {
        spec.managed_service_context = managed_service_context_spec(
            storage,
            &selected.contract,
            &node.node_id,
            &replacement_bindings,
            true,
        )?;
    }
    let replacement_health_gate = HealthGatePolicy::for_runtime_contract(&spec.runtime_contract);
    let replacement_install = RuntimeInstallPayload {
        spec: spec.clone(),
        start: true,
        health_gate: replacement_health_gate.clone(),
        offline_oci_artifact: offline_artifact_for_release(
            storage,
            artifact_store,
            &documents,
            &current.instance.service_id,
            &root_release.release.version,
        )?,
    };
    let desired_pipeline = release_pipeline_payload(
        &selected.manifest,
        &selected.contract,
        &replacement_install,
        &replacement_bindings,
        &node,
        &operation_id,
        &input.migration_policy,
        &input.gateway_node_id,
        &input.config,
        &input.secret_refs,
    )?;
    let previous_provider_revision =
        provider_revision_from_operation(storage, &current_proof.operation_id)?;
    let (
        materialization,
        resource_claims,
        migrations,
        desired_auth,
        desired_provisioners,
        desired_gateway,
    ) = desired_pipeline.map_or_else(
        || (None, Vec::new(), Vec::new(), None, Vec::new(), None),
        |pipeline| {
            (
                pipeline.materialization,
                pipeline.resource_claims,
                pipeline.migrations,
                pipeline.auth,
                pipeline.provisioners,
                pipeline.gateway,
            )
        },
    );
    let desired_provider_revision = ReleaseProviderRevision {
        revision_id: operation_id.clone(),
        auth: desired_auth,
        provisioners: desired_provisioners,
        gateway: desired_gateway,
    };
    let provider_saga = (previous_provider_revision.has_managed_state()
        || desired_provider_revision.has_managed_state())
    .then_some(ReplacementProviderSaga {
        previous: previous_provider_revision,
        desired: desired_provider_revision,
    });
    let payload = ReleaseReplacementPayload {
        old_deployment_id: current.instance.deployment_id.clone(),
        old_container_id: current.instance.container_id.clone(),
        new_spec: spec.clone(),
        start: true,
        health_gate: replacement_health_gate,
        offline_oci_artifact: replacement_install.offline_oci_artifact,
        materialization,
        resource_claims,
        migrations,
        provider_saga,
        preserve_old_until_topology_cutover: !topology_applies.is_empty(),
        exclusive_retained_volume_cutover: spec.retained_volume.is_some(),
    };
    payload.validate().map_err(|error| {
        StoreError::new(
            422,
            "STORE_REPLACEMENT_INVALID",
            format!("replacement plan is invalid: {error}"),
        )
    })?;
    let rollback_proof_operation_id = rollback_proof
        .as_ref()
        .map(|proof| proof.operation_id.clone());
    let install_steps = missing_dependencies
        .iter()
        .map(|selection| {
            (
                selection.module_id.clone(),
                format!(
                    "install-{}-{}",
                    selection.module_id, selection.release.version
                ),
            )
        })
        .collect::<BTreeMap<_, _>>();
    let mut jobs = Vec::new();
    let mut planned_dependency_deployments = Vec::new();
    let empty_secret_refs = BTreeMap::new();
    for dependency in &missing_dependencies {
        let release = select_catalog_document_release(
            console,
            &documents,
            &dependency.module_id,
            &dependency.release.version,
        )?;
        ensure_release_checksum(&release.record)?;
        let dependency_image = OciImageReference::parse(dependency.release.oci_image.as_str())
            .map_err(|error| {
                StoreError::new(
                    422,
                    "STORE_IMMUTABLE_IMAGE_REQUIRED",
                    format!(
                        "dependency release {}@{} has invalid immutable image: {error}",
                        dependency.module_id, dependency.release.version
                    ),
                )
            })?;
        let dependency_deployment_id = deployment_id(
            &dependency.module_id,
            &dependency.release.version,
            &node.node_id,
        );
        ensure_deployment_available(
            storage,
            &dependency_deployment_id,
            dependency_image.digest(),
            Some(&operation_id),
        )?;
        let mut dependency_spec = container_spec(
            &dependency_deployment_id,
            &dependency.module_id,
            &dependency.release.version,
            &release.record.checksum,
            &node,
            dependency_image,
            release_runtime_contract(&release.contract)?,
            &release.manifest,
            None,
        );
        dependency_spec.labels.insert(
            "ojos.service_contract_version".to_string(),
            release.contract.contract_version.to_string(),
        );
        attach_release_runtime_volume(&mut dependency_spec, &release.contract)?;
        if !release.contract.events.publishes.is_empty()
            || !release.contract.events.subscribes.is_empty()
            || contract_has_retained_runtime_volume(&release.contract)
        {
            dependency_spec.managed_service_context =
                managed_service_context_spec(storage, &release.contract, &node.node_id, &[], true)?;
        }
        let health_gate = HealthGatePolicy::for_runtime_contract(&dependency_spec.runtime_contract);
        let install = RuntimeInstallPayload {
            spec: dependency_spec,
            start: true,
            health_gate,
            offline_oci_artifact: offline_artifact_for_release(
                storage,
                artifact_store,
                &documents,
                &dependency.module_id,
                &dependency.release.version,
            )?,
        };
        let pipeline = release_pipeline_payload(
            &release.manifest,
            &release.contract,
            &install,
            &[],
            &node,
            &operation_id,
            &input.migration_policy,
            &input.gateway_node_id,
            &Value::Null,
            &empty_secret_refs,
        )?;
        let (kind, payload, max_attempts) = if let Some(pipeline) = pipeline {
            (
                JobKind::ReleasePipeline,
                serde_json::to_value(pipeline).map_err(|error| {
                    StoreError::new(
                        500,
                        "STORE_PIPELINE_INVALID",
                        format!("serialize replacement dependency pipeline: {error}"),
                    )
                })?,
                1,
            )
        } else {
            (
                JobKind::Install,
                serde_json::to_value(install).map_err(|error| {
                    StoreError::new(
                        500,
                        "STORE_INSTALL_INVALID",
                        format!("serialize replacement dependency install: {error}"),
                    )
                })?,
                3,
            )
        };
        jobs.push(PlannedJob {
            step_id: install_steps
                .get(&dependency.module_id)
                .expect("missing replacement dependency was indexed")
                .clone(),
            node_id: node.node_id.clone(),
            kind,
            depends_on: dependency
                .release
                .dependencies
                .iter()
                .filter_map(|nested| install_steps.get(&nested.module_id).cloned())
                .collect(),
            condition: PlannedJobCondition::OnSuccess,
            payload,
            max_attempts,
        });
        planned_dependency_deployments.push((
            dependency_deployment_id,
            dependency.release.oci_image.digest().as_str().to_string(),
        ));
    }
    let dependency_steps = jobs
        .iter()
        .map(|job| job.step_id.clone())
        .collect::<Vec<_>>();
    let runtime_step = format!(
        "runtime-{}",
        match action {
            ReplacementAction::Upgrade => "upgrade",
            ReplacementAction::Rollback => "rollback",
        }
    );
    let prepare_steps = topology_applies
        .iter()
        .enumerate()
        .map(|(index, _)| format!("topology-binding-prepare-{index}"))
        .collect::<Vec<_>>();
    let finalize_step = "topology-binding-finalize-group".to_string();
    let abort_steps = topology_applies
        .iter()
        .enumerate()
        .map(|(index, _)| format!("topology-binding-abort-{index}"))
        .collect::<Vec<_>>();
    let context_apply_steps = binding_context_transitions
        .iter()
        .enumerate()
        .map(|(index, _)| format!("binding-context-apply-{index}"))
        .collect::<Vec<_>>();
    let context_health_steps = binding_context_transitions
        .iter()
        .enumerate()
        .map(|(index, _)| format!("binding-context-health-{index}"))
        .collect::<Vec<_>>();
    let mut bootstrap_steps = Vec::new();
    if is_topology_consumer {
        for (index, topology) in topology_applies.iter().enumerate() {
            if !topology.staged_bindings.iter().any(|binding| {
                binding.consumer_deployment_id == new_deployment_id
                    && binding.desired_state == "ACTIVE"
            }) {
                continue;
            }
            let step_id = if is_topology_provider {
                format!("topology-binding-bootstrap-{index}")
            } else {
                prepare_steps[index].clone()
            };
            let bindings = topology_bootstrap_bindings
                .get(&topology.topology_id)
                .unwrap_or(&topology.staged_bindings);
            jobs.push(PlannedJob {
                step_id: step_id.clone(),
                node_id: CONTROL_PLANE_NODE_ID.to_string(),
                kind: JobKind::TopologyApply,
                depends_on: dependency_steps.clone(),
                condition: PlannedJobCondition::OnSuccess,
                payload: json!({
                    "topology_id": topology.topology_id.clone(),
                    "revision_id": topology.revision_id,
                    "phase": "PREPARE",
                    "bindings": bindings,
                    "previous_bindings": topology.previous_bindings,
                }),
                max_attempts: 1,
            });
            bootstrap_steps.push(step_id);
        }
    }
    jobs.push(PlannedJob {
        step_id: runtime_step.clone(),
        node_id: node.node_id.clone(),
        kind: action.job_kind(),
        depends_on: if !bootstrap_steps.is_empty() {
            bootstrap_steps.clone()
        } else {
            dependency_steps
        },
        condition: PlannedJobCondition::OnSuccess,
        payload: serde_json::to_value(&payload).map_err(|error| {
            StoreError::new(
                500,
                "STORE_REPLACEMENT_INVALID",
                format!("serialize replacement payload: {error}"),
            )
        })?,
        max_attempts: 3,
    });
    if !topology_applies.is_empty() {
        if is_topology_provider {
            for (index, topology) in topology_applies.iter().enumerate() {
                jobs.push(PlannedJob {
                    step_id: prepare_steps[index].clone(),
                    node_id: CONTROL_PLANE_NODE_ID.to_string(),
                    kind: JobKind::TopologyApply,
                    depends_on: vec![runtime_step.clone()],
                    condition: PlannedJobCondition::OnSuccess,
                    payload: json!({
                        "topology_id": topology.topology_id,
                        "revision_id": topology.revision_id,
                        "phase": "PREPARE",
                        "bindings": topology.staged_bindings,
                        "previous_bindings": topology.previous_bindings,
                    }),
                    max_attempts: 1,
                });
            }
        }
        for (index, transition) in binding_context_transitions.iter().enumerate() {
            jobs.push(PlannedJob {
                step_id: context_apply_steps[index].clone(),
                node_id: transition.node_id.clone(),
                kind: JobKind::BindingContextApply,
                depends_on: prepare_steps.clone(),
                condition: PlannedJobCondition::OnSuccess,
                payload: serde_json::to_value(&transition.forward).map_err(|error| {
                    StoreError::new(
                        500,
                        "STORE_BINDING_CONTEXT_INVALID",
                        format!("serialize binding context apply: {error}"),
                    )
                })?,
                max_attempts: 1,
            });
            jobs.push(PlannedJob {
                step_id: context_health_steps[index].clone(),
                node_id: transition.node_id.clone(),
                kind: JobKind::Health,
                depends_on: vec![context_apply_steps[index].clone()],
                condition: PlannedJobCondition::OnSuccess,
                payload: json!({"container_id": transition.container_id}),
                max_attempts: 3,
            });
        }
        let mut finalize_dependencies = vec![runtime_step.clone()];
        finalize_dependencies.extend(prepare_steps.iter().cloned());
        finalize_dependencies.extend(context_health_steps.iter().cloned());
        jobs.push(PlannedJob {
            step_id: finalize_step.clone(),
            node_id: CONTROL_PLANE_NODE_ID.to_string(),
            kind: JobKind::TopologyApply,
            depends_on: finalize_dependencies.clone(),
            condition: PlannedJobCondition::OnSuccess,
            payload: json!({
                "phase": "FINALIZE_GROUP",
                "group": topology_applies.iter().map(|topology| json!({
                    "topology_id": topology.topology_id,
                    "revision_id": topology.revision_id,
                })).collect::<Vec<_>>(),
            }),
            max_attempts: 1,
        });
        // A consumer-only replacement deliberately reuses its topology
        // PREPARE as the bootstrap gate. Build this compensation fan-in as a
        // set so the shared step cannot be emitted twice and make the durable
        // Operation graph invalid.
        let abort_dependencies = finalize_dependencies
            .into_iter()
            .chain(bootstrap_steps.iter().cloned())
            .chain(std::iter::once(finalize_step.clone()))
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();
        for (index, topology) in topology_applies.iter().enumerate() {
            jobs.push(PlannedJob {
                step_id: abort_steps[index].clone(),
                node_id: CONTROL_PLANE_NODE_ID.to_string(),
                kind: JobKind::TopologyApply,
                depends_on: abort_dependencies.clone(),
                condition: PlannedJobCondition::OnFailure,
                payload: json!({
                    "topology_id": topology.topology_id,
                    "revision_id": topology.revision_id,
                    "phase": "ABORT",
                    "bindings": topology.staged_bindings,
                    "previous_bindings": topology.previous_bindings,
                }),
                max_attempts: 1,
            });
        }
        for (index, transition) in binding_context_transitions.iter().enumerate() {
            let mut depends_on = abort_steps.clone();
            depends_on.push(context_apply_steps[index].clone());
            jobs.push(PlannedJob {
                step_id: format!("binding-context-rollback-{index}"),
                node_id: transition.node_id.clone(),
                kind: JobKind::BindingContextApply,
                depends_on,
                condition: PlannedJobCondition::OnSuccess,
                payload: serde_json::to_value(&transition.rollback).map_err(|error| {
                    StoreError::new(
                        500,
                        "STORE_BINDING_CONTEXT_INVALID",
                        format!("serialize binding context rollback: {error}"),
                    )
                })?,
                max_attempts: 1,
            });
        }
        jobs.push(PlannedJob {
            step_id: "remove-old-after-topology-cutover".to_string(),
            node_id: node.node_id.clone(),
            kind: JobKind::Uninstall,
            depends_on: vec![finalize_step.clone()],
            condition: PlannedJobCondition::OnSuccess,
            payload: json!({
                "deployment_id": current.instance.deployment_id,
                "container_id": current.instance.container_id,
                "force": false,
            }),
            max_attempts: 3,
        });
        jobs.push(PlannedJob {
            step_id: "remove-new-after-topology-abort".to_string(),
            node_id: node.node_id.clone(),
            kind: JobKind::Uninstall,
            depends_on: abort_steps.clone(),
            condition: PlannedJobCondition::OnSuccess,
            payload: json!({
                "deployment_id": new_deployment_id,
                "container_id": stable_container_name(&new_deployment_id),
                "force": true,
            }),
            max_attempts: 3,
        });
    }
    if let Some(contribution) = &staged_contribution {
        let mut commit_dependencies = prepare_steps.clone();
        commit_dependencies.extend(context_health_steps.iter().cloned());
        let has_topology = !topology_applies.is_empty();
        append_contribution_replacement_job_fragment(
            &mut jobs,
            contribution,
            ContributionReplacementDagV1 {
                prepare_depends_on: bootstrap_steps,
                runtime_step_id: runtime_step,
                commit_depends_on: commit_dependencies,
                topology_finalize_step_ids: has_topology
                    .then_some(finalize_step)
                    .into_iter()
                    .collect(),
                topology_abort_step_ids: abort_steps,
                success_cleanup_step_ids: has_topology
                    .then_some("remove-old-after-topology-cutover".to_string())
                    .into_iter()
                    .collect(),
                failure_cleanup_step_ids: has_topology
                    .then_some("remove-new-after-topology-abort".to_string())
                    .into_iter()
                    .collect(),
            },
        )
        .map_err(contribution_controller_error)?;
    }
    let plan = PlanOperation {
        operation_id: operation_id.clone(),
        action: action.action_id().to_string(),
        target_type: "Release".to_string(),
        target_id: format!(
            "{}@{}",
            current.instance.service_id, root_release.release.version
        ),
        request: json!({
            "service_id": current.instance.service_id,
            "version": root_release.release.version,
            "image": root_release.release.oci_image,
            "deployment_id": new_deployment_id,
            "endpoint": spec
                .published_endpoint
                .as_ref()
                .map(|endpoint| endpoint.endpoint.as_str()),
            "planned_deployment_ids": std::iter::once(&new_deployment_id)
                .chain(planned_dependency_deployments.iter().map(|(deployment_id, _)| deployment_id))
                .collect::<Vec<_>>(),
            "replaces_deployment_id": current.instance.deployment_id,
            "previous_version": current_proof.version,
            "previous_image": current_proof.image,
            "previous_operation_id": current_proof.operation_id,
            "previous_catalog_id": current_proof.catalog_id,
            "previous_catalog_verified_key_ids": current_proof.verified_key_ids,
            "rollback_proof_operation_id": rollback_proof_operation_id,
            "target_node_id": node.node_id,
            "target_platform": platform,
            "start": true,
            "channel": channel,
            "migration_policy": input.migration_policy.to_ascii_uppercase(),
            "release_checksum": selected.record.checksum,
            "catalog_source_id": resolved.source_id,
            "catalog_id": resolved.catalog_id,
            "catalog_verified_key_ids": resolved.verified_key_ids,
            "catalog_plan": resolved.plan,
            "bindings": replacement_bindings,
            "topologies": topology_applies.iter().map(|topology| {
                let selected_revision_id = replacement_topologies
                    .iter()
                    .find(|selection| selection.topology_id == topology.topology_id)
                    .map(|selection| selection.revision_id.as_str());
                json!({
                    "topology_id": topology.topology_id,
                    "selected_revision_id": selected_revision_id,
                    "proposed_revision_id": topology.revision_id,
                })
            }).collect::<Vec<_>>(),
            "auto_enqueue": true,
        }),
        jobs,
    };
    let admission = StoreAdmission::acquire(storage)?;
    ensure_no_active_deployment_mutation(storage, current_deployment_id, Some(&operation_id))?;
    ensure_no_active_replacement(storage, current_deployment_id, Some(&operation_id))?;
    ensure_deployment_available(
        storage,
        &new_deployment_id,
        root_release.release.oci_image.digest().as_str(),
        Some(&operation_id),
    )?;
    if let Some(endpoint) = spec.published_endpoint.as_ref() {
        ensure_endpoint_available(
            storage,
            endpoint,
            Some(current_deployment_id),
            Some(&operation_id),
        )?;
    }
    for (dependency_deployment_id, digest) in &planned_dependency_deployments {
        ensure_deployment_available(
            storage,
            dependency_deployment_id,
            digest,
            Some(&operation_id),
        )?;
    }
    let operation = admission.enqueue(
        plan,
        topology_applies.iter().map(|topology| TopologyReservation {
            topology_id: &topology.topology_id,
            revision_id: &topology.revision_id,
        }),
    )?;
    Ok(json!({
        "operation_id": operation_id,
        "operation": operation,
        "deployment_id": new_deployment_id,
        "replaces_deployment_id": current_deployment_id,
        "endpoint": spec
            .published_endpoint
            .as_ref()
            .map(|endpoint| endpoint.endpoint.as_str()),
        "release": {
            "service_id": current.instance.service_id,
            "version": root_release.release.version,
            "checksum": selected.record.checksum,
            "image": root_release.release.oci_image,
            "target_platform": platform,
        },
        "imported": imported,
        "lifecycle": action.lifecycle(),
        "installed": false,
    }))
}
