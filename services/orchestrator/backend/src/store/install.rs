//! Managed/external install coordination. Validation, publication, plan admission and compensation order are preserved.
use crate::artifact_store::ArtifactStore;
use crate::catalog_registry::CatalogRegistry;
use crate::catalog_registry::ResolvedCatalogPlan;
use crate::catalog_registry::VerifiedReleaseDocument;
use crate::contribution_controller::append_contribution_job_fragment;
use crate::contribution_controller::contribution_job_steps;
use crate::durable::DurableStore;
use crate::store::admission::{StoreAdmission, TopologyReservation};
use crate::store::artifacts::{
    artifact_matches, image_digest, missing_resolved_dependencies, offline_artifact_for_release,
};
use crate::store::bindings::{
    ensure_managed_api_bindings_ready, production_binding_plan, resolve_install_api_bindings,
};
use crate::store::commands::{
    InstallReleaseRequest, non_empty, normalize_store_topology_selection, parse_release_channel,
    required_text,
};
use crate::store::composition::build_store_composition_plan;
use crate::store::context::{CONTROL_PLANE_NODE_ID, MutationContext, operation_id};
use crate::store::contribution::{add_job_dependency, stage_release_contribution};
use crate::store::error::{StoreError, catalog_registry_error, core_error, storage_error};
use crate::store::metadata::{
    SelectedRelease, ensure_release_checksum, select_catalog_document_release,
};
use crate::store::node::{
    ensure_ready_docker_node, ensure_release_runtime_supported, host_platform,
    release_runtime_contract, target_platform,
};
use crate::store::placement::{
    container_spec, effective_managed_endpoint, ensure_deployment_available,
    ensure_endpoint_available, managed_published_endpoint,
};
use crate::store::runtime_plan::release_pipeline_payload;
use crate::store::service_context::{
    attach_release_runtime_volume, contract_has_retained_runtime_volume,
    managed_service_context_spec,
};
use crate::store::topology::{StoreTopologyApplyPlan, propose_store_install_topology};
use orchestrator_control_plane::JobKind;
use orchestrator_control_plane::PlanOperation;
use orchestrator_control_plane::PlannedJob;
use orchestrator_control_plane::PlannedJobCondition;
use orchestrator_legacy::ApiBindingState;
use orchestrator_legacy::NodeRecord;
use orchestrator_legacy::OrchestratorActionConsole;
use orchestrator_legacy::composition::CompositionPlanV1;
use orchestrator_legacy::composition::ValidatedInstallInputsV1;
use orchestrator_manager::catalog_v2::TargetPlatform;
use orchestrator_manager::store::composition::composition_inputs_for_service;
use orchestrator_manager::store::composition::legacy_composition_inputs;
use orchestrator_manager::store::composition::release_contract_from_document;
use orchestrator_manager::store::composition::validate_store_composition_inputs;
use orchestrator_manager::store::deployment_id;
use orchestrator_runtime::HealthGatePolicy;
use orchestrator_runtime::OciImageReference;
use orchestrator_runtime::RuntimeInstallPayload;
use orchestrator_runtime::RuntimeObservedState;
use orchestrator_runtime::RuntimeProfile;
use orchestrator_runtime::stable_container_name;
use serde_json::Value;
use serde_json::json;
use std::collections::BTreeMap;

pub(crate) fn install_release(
    console: &mut OrchestratorActionConsole,
    storage: &DurableStore,
    catalog_registry: &CatalogRegistry,
    artifact_store: Option<&ArtifactStore>,
    mut input: InstallReleaseRequest,
    request: &MutationContext,
) -> Result<Value, StoreError> {
    input.topology = normalize_store_topology_selection(
        &input.topology_id,
        &input.topology_etag,
        input.topology.as_ref(),
    )?;
    if !input.source_url.trim().is_empty() || !input.checksum.trim().is_empty() {
        return Err(StoreError::new(
            422,
            "CATALOG_INSTALL_REQUIRED",
            "release.install resolves only trusted Catalog v2 content; use releases:import for an explicit metadata-only import",
        ));
    }
    let external = if input.mode.eq_ignore_ascii_case("MANAGED") {
        false
    } else if input.mode.eq_ignore_ascii_case("EXTERNAL") {
        true
    } else {
        return Err(StoreError::new(
            422,
            "STORE_INSTALL_MODE_INVALID",
            "mode must be MANAGED or EXTERNAL",
        ));
    };
    if !external && input.target_node_id.trim().is_empty() {
        return Err(StoreError::new(
            422,
            "STORE_TARGET_NODE_REQUIRED",
            "target_node_id is required for a Managed install",
        ));
    }
    if external && input.endpoint.trim().is_empty() {
        return Err(StoreError::new(
            422,
            "STORE_EXTERNAL_ENDPOINT_REQUIRED",
            "endpoint is required for an External install",
        ));
    }
    if external && !input.start {
        return Err(StoreError::new(
            422,
            "STORE_EXTERNAL_START_REQUIRED",
            "External install can only register an endpoint after it is healthy and running",
        ));
    }
    let node = non_empty(&input.target_node_id)
        .map(|node_id| {
            storage
                .get_node(node_id)
                .map_err(storage_error)?
                .ok_or_else(|| {
                    StoreError::new(
                        404,
                        "STORE_TARGET_NODE_NOT_FOUND",
                        format!("target Node {node_id} was not found"),
                    )
                })
        })
        .transpose()?;
    if !external {
        ensure_ready_docker_node(storage, node.as_ref().expect("managed node was required"))?;
    }
    let platform = node
        .as_ref()
        .map(|node| target_platform(storage, node))
        .transpose()?
        .unwrap_or_else(host_platform);
    let service_id = required_text(&input.service_id, "service_id")?.to_string();
    let channel = parse_release_channel(&input.channel)?;
    let resolved = catalog_registry
        .resolve_install_plan(
            storage,
            non_empty(&input.catalog_source_id),
            &service_id,
            non_empty(&input.version),
            channel,
            platform.clone(),
        )
        .map_err(catalog_registry_error)?;
    let documents = catalog_registry
        .fetch_release_documents(storage, &resolved)
        .map_err(catalog_registry_error)?;
    let composition_node = node.as_ref().ok_or_else(|| {
        StoreError::new(
            422,
            "STORE_TARGET_NODE_REQUIRED",
            "CompositionPlanV1 requires a target Node provider snapshot",
        )
    })?;
    let composition_plan =
        build_store_composition_plan(storage, &documents, &service_id, composition_node)?;
    let has_platform_contract = documents
        .iter()
        .map(release_contract_from_document)
        .collect::<Result<Vec<_>, _>>()?
        .iter()
        .any(|contract| contract.platform.is_some());
    let supplied_plan_digest = if input.plan_digest.trim().is_empty() && !has_platform_contract {
        composition_plan.plan_digest.as_str()
    } else {
        required_text(&input.plan_digest, "plan_digest")?
    };
    let supplied_graph_digest =
        if input.release_graph_digest.trim().is_empty() && !has_platform_contract {
            composition_plan.release_graph_digest.as_str()
        } else {
            required_text(&input.release_graph_digest, "release_graph_digest")?
        };
    let validated_composition = if has_platform_contract {
        validate_store_composition_inputs(
            &composition_plan,
            supplied_plan_digest,
            supplied_graph_digest,
            &input.inputs,
            (!input.config.is_null()).then(|| input.config.clone()),
            input.secret_refs.clone(),
        )?
    } else {
        legacy_composition_inputs(&composition_plan, &input.config, &input.secret_refs)?
    };
    let external_missing_dependencies = if external {
        missing_resolved_dependencies(storage, &resolved, &service_id)?
    } else {
        Vec::new()
    };
    if external && !external_missing_dependencies.is_empty() {
        let dependency_node = node.as_ref().ok_or_else(|| {
            StoreError::new(
                422,
                "STORE_TARGET_NODE_REQUIRED",
                "target_node_id is required when an External release has managed dependencies to install",
            )
        })?;
        ensure_ready_docker_node(storage, dependency_node)?;
    }
    if let Some(node) = node.as_ref() {
        for document in &documents {
            let managed_on_target = !external
                || external_missing_dependencies.iter().any(|dependency| {
                    dependency.module_id == document.selection.module_id
                        && dependency.release.version == document.selection.release.version
                });
            if managed_on_target {
                let contract = release_contract_from_document(document)?;
                ensure_release_runtime_supported(
                    storage,
                    node,
                    &contract,
                    document.selection.release.oci_image.as_str(),
                )?;
            }
        }
    }

    // A Managed install must prove its complete outbound contract before even
    // publishing the release metadata. Legacy required_apis are normalized to
    // stable names by ServiceReleaseContract, but they do not receive a
    // PENDING compatibility escape hatch: the exact healthy provider must
    // already be represented by the selected applied Topology.
    let managed_binding_preflight = if external {
        None
    } else {
        let root_document = documents
            .iter()
            .find(|document| {
                document.selection.module_id == service_id
                    && document.selection.release.version == resolved.plan.root.version
            })
            .ok_or_else(|| {
                StoreError::new(
                    500,
                    "CATALOG_PLAN_INVALID",
                    "resolved install plan does not contain its requested root metadata",
                )
            })?;
        let contract = release_contract_from_document(root_document)?;
        let node = node
            .as_ref()
            .expect("managed install requires a target Node");
        input.endpoint = effective_managed_endpoint(&input.endpoint, node, &contract.release)?;
        let deployment_id = deployment_id(&service_id, &resolved.plan.root.version, &node.node_id);
        let bindings = resolve_install_api_bindings(
            console,
            storage,
            &contract,
            &deployment_id,
            &node.node_id,
            &input.endpoint,
            &input.bindings,
            input.topology.as_ref(),
            false,
        )?;
        ensure_managed_api_bindings_ready(storage, &contract, &bindings, input.topology.as_ref())?;
        Some((deployment_id, bindings))
    };

    // All catalog, signature, dependency, metadata, checksum, and OCI checks
    // have completed before durable publication begins. Publication is atomic
    // per Service+Release and has no runtime side effect.
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
        &service_id,
        &resolved.plan.root.version,
    )?;
    ensure_release_checksum(&selected.record)?;
    let root_release = resolved
        .plan
        .releases
        .last()
        .filter(|release| release.module_id == service_id)
        .ok_or_else(|| {
            StoreError::new(
                500,
                "CATALOG_PLAN_INVALID",
                "resolved dependency plan does not end with its requested root",
            )
        })?;
    let image =
        OciImageReference::parse(root_release.release.oci_image.as_str()).map_err(|error| {
            StoreError::new(
                422,
                "STORE_IMMUTABLE_IMAGE_REQUIRED",
                format!(
                    "release {}@{} must use repository@sha256:<64 lowercase hex>: {error}",
                    service_id, selected.version
                ),
            )
        })?;
    if external {
        validate_external_install_endpoint(
            &service_id,
            input.endpoint.trim(),
            &selected.manifest.backend.protocol,
        )?;
        return enqueue_external_install(
            console,
            storage,
            request,
            &input,
            &service_id,
            &selected,
            &resolved,
            root_release,
            image,
            platform,
            imported,
            &documents,
            node.as_ref(),
            &external_missing_dependencies,
            artifact_store,
            &composition_plan,
            &validated_composition,
        );
    }
    let node = node.expect("managed install requires a target Node");
    let (root_deployment_id, mut binding_plan) = managed_binding_preflight
        .expect("managed binding preflight must run before release publication");
    let operation_id = operation_id("store-install", &root_deployment_id, request)?;
    let staged_contribution = stage_release_contribution(
        storage,
        &operation_id,
        &root_deployment_id,
        &selected.contract,
        root_release.release.oci_image.digest().as_str(),
    )?;
    if staged_contribution.is_some() && !input.start {
        return Err(StoreError::new(
            422,
            "STORE_CONTRIBUTION_START_REQUIRED",
            "a release with an active Contribution must start and pass its runtime health gate before routes, permissions, or frontend modules can be activated",
        ));
    }
    let topology_apply = input
        .topology
        .as_ref()
        .map(|selection| {
            propose_store_install_topology(
                storage,
                selection,
                &selected.contract,
                &root_deployment_id,
                &node.node_id,
                &input.endpoint,
                &binding_plan,
                &operation_id,
                None,
            )
        })
        .transpose()?;
    if let Some(topology) = &topology_apply {
        binding_plan = production_binding_plan(topology.staged_bindings.iter().filter(|binding| {
            binding.consumer_deployment_id == root_deployment_id
                && binding.desired_state == "ACTIVE"
        }));
    }
    let existing = storage.runtime_instances(None).map_err(storage_error)?;
    let missing = resolved
        .plan
        .releases
        .iter()
        .filter(|selection| {
            selection.module_id == service_id
                || !existing.iter().any(|deployment| {
                    deployment.instance.service_id == selection.module_id
                        && deployment.instance.observed_state == RuntimeObservedState::Running
                        && deployment.instance.health.eq_ignore_ascii_case("HEALTHY")
                        && artifact_matches(
                            selection.release.oci_image.as_str(),
                            &deployment.instance.artifact_digest,
                        )
                })
        })
        .collect::<Vec<_>>();
    let install_steps = missing
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
    if let Some(topology) = &topology_apply {
        jobs.push(PlannedJob {
            step_id: "topology-binding-prepare".to_string(),
            node_id: CONTROL_PLANE_NODE_ID.to_string(),
            kind: JobKind::TopologyApply,
            depends_on: vec![],
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
    let mut planned_deployments = Vec::new();
    let mut root_spec = None;
    for selection in &missing {
        let release = select_catalog_document_release(
            console,
            &documents,
            &selection.module_id,
            &selection.release.version,
        )?;
        ensure_release_checksum(&release.record)?;
        let release_image = OciImageReference::parse(selection.release.oci_image.as_str())
            .map_err(|error| {
                StoreError::new(
                    422,
                    "STORE_IMMUTABLE_IMAGE_REQUIRED",
                    format!(
                        "release {}@{} has invalid immutable image: {error}",
                        selection.module_id, selection.release.version
                    ),
                )
            })?;
        let release_deployment_id = deployment_id(
            &selection.module_id,
            &selection.release.version,
            &node.node_id,
        );
        ensure_deployment_available(
            storage,
            &release_deployment_id,
            release_image.digest(),
            Some(&operation_id),
        )?;
        let mut spec = container_spec(
            &release_deployment_id,
            &selection.module_id,
            &selection.release.version,
            &release.record.checksum,
            &node,
            release_image,
            release_runtime_contract(&release.contract)?,
            &release.manifest,
            if selection.module_id == service_id {
                managed_published_endpoint(
                    &input.endpoint,
                    &selection.module_id,
                    &node,
                    &release.manifest,
                )?
            } else {
                None
            },
        );
        spec.labels.insert(
            "ojos.service_contract_version".to_string(),
            release.contract.contract_version.to_string(),
        );
        attach_release_runtime_volume(&mut spec, &release.contract)?;
        let deployment_bindings = if selection.module_id == service_id {
            binding_plan.as_slice()
        } else {
            &[]
        };
        if selection.module_id == service_id
            || !release.contract.events.publishes.is_empty()
            || !release.contract.events.subscribes.is_empty()
            || contract_has_retained_runtime_volume(&release.contract)
        {
            spec.managed_service_context = managed_service_context_spec(
                storage,
                &release.contract,
                &node.node_id,
                deployment_bindings,
                true,
            )?;
        }
        if selection.module_id == service_id {
            root_spec = Some(spec.clone());
        }
        let health_gate = HealthGatePolicy::for_runtime_contract(&spec.runtime_contract);
        let runtime_install = RuntimeInstallPayload {
            spec,
            start: if selection.module_id == service_id {
                input.start
            } else {
                true
            },
            health_gate,
            offline_oci_artifact: offline_artifact_for_release(
                storage,
                artifact_store,
                &documents,
                &selection.module_id,
                &selection.release.version,
            )?,
        };
        let (release_config, release_secret_refs) = composition_inputs_for_service(
            &composition_plan,
            &validated_composition,
            &selection.module_id,
        );
        let pipeline = release_pipeline_payload(
            &release.manifest,
            &release.contract,
            &runtime_install,
            deployment_bindings,
            &node,
            &operation_id,
            &input.migration_policy,
            &input.gateway_node_id,
            &release_config,
            &release_secret_refs,
        )?;
        let (kind, payload, max_attempts) = if let Some(pipeline) = pipeline {
            (
                JobKind::ReleasePipeline,
                serde_json::to_value(pipeline).map_err(|error| {
                    StoreError::new(
                        500,
                        "STORE_PIPELINE_INVALID",
                        format!("serialize release pipeline: {error}"),
                    )
                })?,
                1,
            )
        } else {
            (
                JobKind::Install,
                serde_json::to_value(runtime_install).map_err(|error| {
                    StoreError::new(
                        500,
                        "STORE_INSTALL_INVALID",
                        format!("serialize runtime install: {error}"),
                    )
                })?,
                3,
            )
        };
        let mut depends_on: Vec<String> = selection
            .release
            .dependencies
            .iter()
            .filter_map(|dependency| install_steps.get(&dependency.module_id).cloned())
            .collect();
        if selection.module_id == service_id {
            if topology_apply.is_some() {
                depends_on.push("topology-binding-prepare".to_string());
            }
            if let Some(contribution) = &staged_contribution {
                // The contribution fragment is appended after runtime jobs;
                // its deterministic PREPARE id is safe to reference now.
                depends_on.push(contribution_job_steps(contribution).prepare_step_id);
            }
        }
        let step_id = install_steps
            .get(&selection.module_id)
            .expect("missing releases were indexed")
            .clone();
        jobs.push(PlannedJob {
            step_id,
            node_id: node.node_id.clone(),
            kind,
            depends_on,
            condition: PlannedJobCondition::OnSuccess,
            payload,
            max_attempts,
        });
        planned_deployments.push((
            release_deployment_id,
            selection.release.oci_image.digest().as_str().to_string(),
        ));
    }
    if let Some(topology) = &topology_apply {
        let root_step = install_steps.get(&service_id).cloned().ok_or_else(|| {
            StoreError::new(500, "CATALOG_PLAN_INVALID", "root install step is missing")
        })?;
        append_install_topology_jobs(
            &mut jobs,
            topology,
            &root_step,
            &node.node_id,
            &root_deployment_id,
        );
    }
    if let Some(contribution) = &staged_contribution {
        let root_step = install_steps.get(&service_id).cloned().ok_or_else(|| {
            StoreError::new(500, "CATALOG_PLAN_INVALID", "root install step is missing")
        })?;
        let prepare_dependencies = topology_apply
            .as_ref()
            .map(|_| vec!["topology-binding-prepare".to_string()])
            .unwrap_or_default();
        let finalize_step = topology_apply
            .as_ref()
            .map(|_| "topology-binding-finalize-success".to_string());
        let commit_dependencies = vec![root_step];
        let contribution_steps = append_contribution_job_fragment(
            &mut jobs,
            contribution,
            prepare_dependencies,
            commit_dependencies,
            finalize_step.clone().into_iter().collect(),
        );
        if let Some(finalize_step) = finalize_step {
            add_job_dependency(
                &mut jobs,
                &finalize_step,
                &contribution_steps.commit_step_id,
            )?;
            // A failed Contribution COMMIT leaves FINALIZE intentionally
            // unmaterialized. Topology ABORT therefore needs the COMMIT as a
            // direct failure witness; depending only on the unbound FINALIZE
            // node would strand the PREPARE projection forever.
            add_job_dependency(
                &mut jobs,
                "topology-binding-finalize-failure",
                &contribution_steps.commit_step_id,
            )?;
            // Runtime cleanup is safe only after both projections have restored
            // their previous active state.  Otherwise topology ABORT and
            // Contribution ABORT can race with container removal.
            add_job_dependency(
                &mut jobs,
                "remove-root-after-topology-abort",
                &contribution_steps.abort_step_id,
            )?;
        }
    }
    // Every install/pipeline step compensates its own partially-created runtime
    // resources.  Successfully installed dependencies are intentionally retained:
    // another operation may have started referencing the shared deployment after
    // this plan was accepted, so an operation-local unconditional uninstall would
    // be unsafe.
    let spec = root_spec.ok_or_else(|| {
        StoreError::new(
            500,
            "CATALOG_PLAN_INVALID",
            "managed dependency plan did not include its requested root",
        )
    })?;
    let plan = PlanOperation {
        operation_id: operation_id.clone(),
        action: "release.install".to_string(),
        target_type: "Release".to_string(),
        target_id: format!("{service_id}@{}", selected.version),
        request: json!({
            "service_id": service_id,
            "version": selected.version,
            "target_node_id": node.node_id,
            "target_platform": platform,
            "mode": "MANAGED",
            "channel": channel,
            "deployment_id": root_deployment_id,
            "endpoint": spec
                .published_endpoint
                .as_ref()
                .map(|endpoint| endpoint.endpoint.as_str()),
            "planned_deployment_ids": planned_deployments
                .iter()
                .map(|(deployment_id, _)| deployment_id)
                .collect::<Vec<_>>(),
            "start": input.start,
            "migration_policy": input.migration_policy.to_ascii_uppercase(),
            "release_checksum": selected.record.checksum,
            "image": spec.image.to_string(),
            "catalog_source_id": resolved.source_id,
            "catalog_id": resolved.catalog_id,
            "catalog_verified_key_ids": resolved.verified_key_ids,
            "catalog_plan": resolved.plan,
            "composition_plan_digest": composition_plan.plan_digest,
            "composition_release_graph_digest": composition_plan.release_graph_digest,
            "composition_inputs": validated_composition,
            "bindings": binding_plan,
            "topology": input.topology.as_ref().map(|selection| json!({
                "topology_id": selection.topology_id,
                "selected_revision_id": selection.revision_id,
                "proposed_revision_id": topology_apply.as_ref().map(|topology| topology.revision_id.as_str()),
            })),
            "auto_enqueue": true,
        }),
        jobs,
    };
    let admission = StoreAdmission::acquire(storage)?;
    if let Some(endpoint) = spec.published_endpoint.as_ref() {
        ensure_endpoint_available(storage, endpoint, None, Some(&operation_id))?;
    }
    for (planned_deployment_id, digest) in &planned_deployments {
        ensure_deployment_available(storage, planned_deployment_id, digest, Some(&operation_id))?;
    }
    let operation = admission.enqueue(
        plan,
        topology_apply.iter().map(|topology| TopologyReservation {
            topology_id: &topology.topology_id,
            revision_id: &topology.revision_id,
        }),
    )?;
    Ok(json!({
        "operation_id": operation_id,
        "operation": operation,
        "deployment_id": root_deployment_id,
        "bindings": binding_plan,
        "endpoint": spec
            .published_endpoint
            .as_ref()
            .map(|endpoint| endpoint.endpoint.as_str()),
        "release": {
            "service_id": service_id,
            "version": selected.version,
            "checksum": selected.record.checksum,
            "image": root_release.release.oci_image,
            "target_platform": platform,
        },
        "imported": imported,
        "lifecycle": "Deploying",
        "installed": false,
    }))
}

pub(crate) fn append_install_topology_jobs(
    jobs: &mut Vec<PlannedJob>,
    topology: &StoreTopologyApplyPlan,
    root_step: &str,
    node_id: &str,
    root_deployment_id: &str,
) {
    jobs.push(PlannedJob {
        step_id: "topology-binding-finalize-success".to_string(),
        node_id: CONTROL_PLANE_NODE_ID.to_string(),
        kind: JobKind::TopologyApply,
        depends_on: vec![root_step.to_string()],
        condition: PlannedJobCondition::OnSuccess,
        payload: json!({
            "topology_id": topology.topology_id,
            "revision_id": topology.revision_id,
            "phase": "FINALIZE",
            "bindings": topology.staged_bindings,
            "previous_bindings": topology.previous_bindings,
        }),
        max_attempts: 1,
    });
    jobs.push(PlannedJob {
        step_id: "topology-binding-finalize-failure".to_string(),
        node_id: CONTROL_PLANE_NODE_ID.to_string(),
        kind: JobKind::TopologyApply,
        // ABORT is needed both when the root install fails and when FINALIZE
        // itself fails. An unbound successful branch is treated as skipped by
        // OnFailure, so this one dependency set covers both paths.
        depends_on: vec![
            root_step.to_string(),
            "topology-binding-finalize-success".to_string(),
        ],
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
    jobs.push(PlannedJob {
        step_id: "remove-root-after-topology-abort".to_string(),
        node_id: node_id.to_string(),
        kind: JobKind::Uninstall,
        depends_on: vec![
            root_step.to_string(),
            "topology-binding-finalize-failure".to_string(),
        ],
        condition: PlannedJobCondition::OnSuccess,
        payload: json!({
            "deployment_id": root_deployment_id,
            "container_id": stable_container_name(root_deployment_id),
            "force": true,
        }),
        max_attempts: 3,
    });
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn enqueue_external_install(
    console: &OrchestratorActionConsole,
    storage: &DurableStore,
    request: &MutationContext,
    input: &InstallReleaseRequest,
    service_id: &str,
    selected: &SelectedRelease,
    resolved: &ResolvedCatalogPlan,
    root_release: &orchestrator_manager::catalog_v2::ResolvedReleaseV2,
    image: OciImageReference,
    platform: TargetPlatform,
    imported: Vec<orchestrator_legacy::ExternalReleaseImport>,
    documents: &[VerifiedReleaseDocument],
    node: Option<&NodeRecord>,
    missing_dependencies: &[&orchestrator_manager::catalog_v2::ResolvedReleaseV2],
    artifact_store: Option<&ArtifactStore>,
    composition_plan: &CompositionPlanV1,
    validated_composition: &ValidatedInstallInputsV1,
) -> Result<Value, StoreError> {
    let endpoint = input.endpoint.trim();
    if release_runtime_contract(&selected.contract)?.id == RuntimeProfile::JudgeSandboxV1 {
        return Err(StoreError::new(
            422,
            "STORE_EXTERNAL_RUNTIME_PROFILE_FORBIDDEN",
            "judge-sandbox-v1 requires a Managed Agent assignment so runtime policy, HostConfig attestation, context materialization, and compensation remain provable",
        ));
    }
    if !selected.contract.events.publishes.is_empty()
        || !selected.contract.events.subscribes.is_empty()
    {
        return Err(StoreError::new(
            422,
            "STORE_EXTERNAL_EVENT_CONTEXT_REQUIRED",
            "Event Contract v2 requires an Agent-materialized event context; install this release as Managed",
        ));
    }
    let root_external_deployment_id = deployment_id(
        service_id,
        &selected.version,
        &format!("external:{endpoint}"),
    );
    let consumer_node_id = node.map(|node| node.node_id.as_str()).unwrap_or("external");
    let binding_plan = resolve_install_api_bindings(
        console,
        storage,
        &selected.contract,
        &root_external_deployment_id,
        consumer_node_id,
        endpoint,
        &input.bindings,
        input.topology.as_ref(),
        false,
    )?;
    let operation_id = operation_id(
        "store-external-install",
        &root_external_deployment_id,
        request,
    )?;
    if binding_plan.iter().any(|binding| {
        matches!(
            binding.state,
            ApiBindingState::Resolved | ApiBindingState::Active
        )
    }) {
        return Err(StoreError::new(
            422,
            "STORE_EXTERNAL_BINDING_CONTEXT_REQUIRED",
            "External consumers cannot receive an Agent-materialized workload context; install this release as Managed or remove its API requirements",
        ));
    }
    let image = image.to_string();
    let channel = parse_release_channel(&input.channel)?;
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
    if !missing_dependencies.is_empty() {
        let node = node.expect("missing External dependencies require a validated Node");
        for selection in missing_dependencies {
            let release = select_catalog_document_release(
                console,
                documents,
                &selection.module_id,
                &selection.release.version,
            )?;
            ensure_release_checksum(&release.record)?;
            let release_image = OciImageReference::parse(selection.release.oci_image.as_str())
                .map_err(|error| {
                    StoreError::new(
                        422,
                        "STORE_IMMUTABLE_IMAGE_REQUIRED",
                        format!(
                            "dependency release {}@{} has invalid immutable image: {error}",
                            selection.module_id, selection.release.version
                        ),
                    )
                })?;
            let dependency_deployment_id = deployment_id(
                &selection.module_id,
                &selection.release.version,
                &node.node_id,
            );
            ensure_deployment_available(
                storage,
                &dependency_deployment_id,
                release_image.digest(),
                Some(&operation_id),
            )?;
            let mut spec = container_spec(
                &dependency_deployment_id,
                &selection.module_id,
                &selection.release.version,
                &release.record.checksum,
                node,
                release_image,
                release_runtime_contract(&release.contract)?,
                &release.manifest,
                None,
            );
            spec.labels.insert(
                "ojos.service_contract_version".to_string(),
                release.contract.contract_version.to_string(),
            );
            attach_release_runtime_volume(&mut spec, &release.contract)?;
            if !release.contract.events.publishes.is_empty()
                || !release.contract.events.subscribes.is_empty()
                || contract_has_retained_runtime_volume(&release.contract)
            {
                spec.managed_service_context = managed_service_context_spec(
                    storage,
                    &release.contract,
                    &node.node_id,
                    &[],
                    true,
                )?;
            }
            let health_gate = HealthGatePolicy::for_runtime_contract(&spec.runtime_contract);
            let runtime_install = RuntimeInstallPayload {
                spec,
                start: true,
                health_gate,
                offline_oci_artifact: offline_artifact_for_release(
                    storage,
                    artifact_store,
                    documents,
                    &selection.module_id,
                    &selection.release.version,
                )?,
            };
            let (release_config, release_secret_refs) = composition_inputs_for_service(
                composition_plan,
                validated_composition,
                &selection.module_id,
            );
            let pipeline = release_pipeline_payload(
                &release.manifest,
                &release.contract,
                &runtime_install,
                &[],
                node,
                &operation_id,
                &input.migration_policy,
                &input.gateway_node_id,
                &release_config,
                &release_secret_refs,
            )?;
            let (kind, payload, max_attempts) = if let Some(pipeline) = pipeline {
                (
                    JobKind::ReleasePipeline,
                    serde_json::to_value(pipeline).map_err(|error| {
                        StoreError::new(
                            500,
                            "STORE_PIPELINE_INVALID",
                            format!("serialize dependency release pipeline: {error}"),
                        )
                    })?,
                    1,
                )
            } else {
                (
                    JobKind::Install,
                    serde_json::to_value(runtime_install).map_err(|error| {
                        StoreError::new(
                            500,
                            "STORE_INSTALL_INVALID",
                            format!("serialize dependency install: {error}"),
                        )
                    })?,
                    3,
                )
            };
            jobs.push(PlannedJob {
                step_id: install_steps
                    .get(&selection.module_id)
                    .expect("missing dependency was indexed")
                    .clone(),
                node_id: node.node_id.clone(),
                kind,
                depends_on: selection
                    .release
                    .dependencies
                    .iter()
                    .filter_map(|dependency| install_steps.get(&dependency.module_id).cloned())
                    .collect(),
                condition: PlannedJobCondition::OnSuccess,
                payload,
                max_attempts,
            });
            planned_dependency_deployments.push((
                dependency_deployment_id,
                selection.release.oci_image.digest().as_str().to_string(),
            ));
        }
    }
    let install_step_ids = jobs
        .iter()
        .map(|job| job.step_id.clone())
        .collect::<Vec<_>>();
    jobs.push(PlannedJob {
        step_id: "external-health".to_string(),
        node_id: CONTROL_PLANE_NODE_ID.to_string(),
        kind: JobKind::ExternalHealth,
        depends_on: install_step_ids.clone(),
        condition: PlannedJobCondition::OnSuccess,
        payload: json!({
            "deployment_id": root_external_deployment_id,
            "service_id": service_id,
            "version": selected.version,
            "endpoint": endpoint,
            "protocol": selected.manifest.backend.protocol,
            "health_path": selected.manifest.backend.health_path,
            "artifact_digest": image,
        }),
        max_attempts: 3,
    });
    let plan = PlanOperation {
        operation_id: operation_id.clone(),
        action: "release.install".to_string(),
        target_type: "Release".to_string(),
        target_id: format!("{service_id}@{}", selected.version),
        request: json!({
            "service_id": service_id,
            "version": selected.version,
            "target_node_id": non_empty(&input.target_node_id),
            "target_platform": platform,
            "deployment_id": root_external_deployment_id,
            "planned_deployment_ids": std::iter::once(&root_external_deployment_id)
                .chain(planned_dependency_deployments.iter().map(|(deployment_id, _)| deployment_id))
                .collect::<Vec<_>>(),
            "endpoint": endpoint,
            "mode": "EXTERNAL",
            "start": true,
            "channel": channel,
            "release_checksum": selected.record.checksum,
            "image": image,
            "catalog_source_id": resolved.source_id,
            "catalog_id": resolved.catalog_id,
            "catalog_verified_key_ids": resolved.verified_key_ids,
            "catalog_plan": resolved.plan,
            "bindings": binding_plan,
            "topology": input.topology.as_ref().map(|selection| json!({
                "topology_id": selection.topology_id,
                "revision_id": selection.revision_id,
            })),
            "auto_enqueue": true,
        }),
        jobs,
    };
    let admission = StoreAdmission::acquire(storage)?;
    ensure_deployment_available(
        storage,
        &root_external_deployment_id,
        image_digest(&image)?,
        Some(&operation_id),
    )?;
    for (dependency_deployment_id, digest) in &planned_dependency_deployments {
        ensure_deployment_available(
            storage,
            dependency_deployment_id,
            digest,
            Some(&operation_id),
        )?;
    }
    let operation = admission.enqueue(plan, std::iter::empty())?;
    Ok(json!({
        "operation_id": operation_id,
        "operation": operation,
        "deployment_id": root_external_deployment_id,
        "bindings": binding_plan,
        "release": {
            "service_id": service_id,
            "version": selected.version,
            "checksum": selected.record.checksum,
            "image": root_release.release.oci_image,
            "target_platform": platform,
        },
        "endpoint": endpoint,
        "mode": "EXTERNAL",
        "imported": imported,
        "lifecycle": "Validating",
        "installed": false,
    }))
}

pub(crate) fn validate_external_install_endpoint(
    service_id: &str,
    endpoint: &str,
    protocol: &str,
) -> Result<(), StoreError> {
    if !matches!(protocol, "http" | "https" | "tcp" | "postgres" | "redis") {
        return Err(StoreError::new(
            422,
            "STORE_EXTERNAL_PROTOCOL_UNSUPPORTED",
            format!("External health provider does not support protocol {protocol}"),
        ));
    }
    if let Some((scheme, authority)) = endpoint.split_once("://") {
        if scheme != protocol
            || authority.trim().is_empty()
            || authority.chars().any(char::is_whitespace)
        {
            return Err(StoreError::new(
                422,
                "STORE_EXTERNAL_ENDPOINT_INVALID",
                format!("External endpoint must be a valid {protocol} URI"),
            ));
        }
        return Ok(());
    }
    orchestrator_legacy::validate_endpoint_service_name(endpoint, service_id).map_err(core_error)
}
