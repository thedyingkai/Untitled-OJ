//! Store contribution responsibilities.
use crate::contribution_controller::SignedContributionSuccessorV1;
use crate::contribution_controller::stage_contribution;
use crate::contribution_controller::stage_signed_contribution_successor;
use crate::durable::DurableStore;
use crate::store::error::{StoreError, contribution_controller_error, contribution_storage_error};
use orchestrator_control_plane::PlannedJob;
use orchestrator_legacy::ContributionRevisionV1;
use orchestrator_legacy::ServiceReleaseContract;
use orchestrator_storage::ContributionRepository;

pub(crate) fn stage_release_contribution(
    storage: &DurableStore,
    operation_id: &str,
    deployment_id: &str,
    contract: &ServiceReleaseContract,
    runtime_digest: &str,
) -> Result<Option<crate::contribution_controller::StagedContributionV1>, StoreError> {
    let Some(platform) = contract.platform.as_ref() else {
        return Ok(None);
    };
    let contribution = &platform.contribution;
    let head = storage
        .contribution_head("default", &contract.release.service_name)
        .map_err(contribution_storage_error)?;
    if head.is_none()
        && contribution.api_surfaces.is_empty()
        && contribution.operation_routes.is_empty()
        && contribution.permission_definitions.is_empty()
        && contribution.user_frontend_modules.is_empty()
        && contribution.admin_frontend_modules.is_empty()
    {
        return Ok(None);
    }
    let generation = crate::contribution_controller::next_contribution_generation(
        storage,
        "default",
        &contract.release.service_name,
        head.as_ref().map_or(0, |head| head.generation()),
    )
    .map_err(contribution_controller_error)?;
    let previous_revision_id = head
        .as_ref()
        .map(|head| head.active_revision_id().to_string());
    let revision = ContributionRevisionV1::stage(
        "default",
        deployment_id,
        contract.release.service_name.clone(),
        runtime_digest,
        platform.contract_digest.clone(),
        generation,
        previous_revision_id,
        contribution.api_surfaces.clone(),
        contribution.operation_routes.clone(),
        contribution.permission_definitions.clone(),
        contribution.user_frontend_modules.clone(),
        contribution.admin_frontend_modules.clone(),
    )
    .map_err(|error| {
        StoreError::new(
            422,
            "STORE_CONTRIBUTION_INVALID",
            format!("compile signed contribution revision: {error}"),
        )
    })?;
    stage_contribution(storage, operation_id, &revision)
        .map(Some)
        .map_err(contribution_controller_error)
}

pub(crate) fn stage_replacement_contribution(
    storage: &DurableStore,
    operation_id: &str,
    replaces_deployment_id: &str,
    deployment_id: &str,
    contract: &ServiceReleaseContract,
    runtime_digest: &str,
) -> Result<Option<crate::contribution_controller::StagedContributionV1>, StoreError> {
    let head = storage
        .contribution_head("default", &contract.release.service_name)
        .map_err(contribution_storage_error)?;
    let Some(head) = head else {
        return stage_release_contribution(
            storage,
            operation_id,
            deployment_id,
            contract,
            runtime_digest,
        );
    };
    let platform = contract.platform.as_ref().ok_or_else(|| {
        StoreError::new(
            422,
            "STORE_CONTRIBUTION_SUCCESSOR_REQUIRED",
            format!(
                "service {} has active Contribution head {}; a replacement release must carry a signed platform Contribution projection, including an explicit empty successor when withdrawing all contributions",
                contract.release.service_name,
                head.etag()
            ),
        )
    })?;
    let contribution = &platform.contribution;
    stage_signed_contribution_successor(
        storage,
        operation_id,
        SignedContributionSuccessorV1 {
            scope_id: "default".to_string(),
            replaces_deployment_id: replaces_deployment_id.to_string(),
            deployment_id: deployment_id.to_string(),
            service_id: contract.release.service_name.clone(),
            release_digest: runtime_digest.to_string(),
            contract_digest: platform.contract_digest.clone(),
            api_surfaces: contribution.api_surfaces.clone(),
            operation_routes: contribution.operation_routes.clone(),
            permission_definitions: contribution.permission_definitions.clone(),
            user_frontend_modules: contribution.user_frontend_modules.clone(),
            admin_frontend_modules: contribution.admin_frontend_modules.clone(),
        },
    )
    .map(Some)
    .map_err(contribution_controller_error)
}

pub(crate) fn add_job_dependency(
    jobs: &mut [PlannedJob],
    step_id: &str,
    dependency: &str,
) -> Result<(), StoreError> {
    let job = jobs
        .iter_mut()
        .find(|job| job.step_id == step_id)
        .ok_or_else(|| {
            StoreError::new(
                500,
                "STORE_CONTRIBUTION_DAG_INVALID",
                format!("Contribution integration could not find step {step_id}"),
            )
        })?;
    if !job.depends_on.iter().any(|value| value == dependency) {
        job.depends_on.push(dependency.to_string());
    }
    Ok(())
}
