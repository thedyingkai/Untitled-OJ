//! Store history responsibilities.
use crate::durable::DurableStore;
use crate::store::error::StoreError;
use orchestrator_control_plane::DurableOperation;
use orchestrator_control_plane::DurableOperationStatus;
use orchestrator_control_plane::JobKind;
use orchestrator_control_plane::OperationRepository;
use orchestrator_manager::catalog_v2::ReleaseChannel;
use orchestrator_runtime::OciImageReference;
use orchestrator_runtime::ReleasePipelinePayload;
use orchestrator_runtime::ReleaseProviderRevision;
use orchestrator_runtime::ReleaseReplacementPayload;
use serde_json::Value;

#[derive(Debug, Clone)]
pub(crate) struct ReleaseHistoryProof {
    pub(crate) operation_id: String,
    pub(crate) deployment_id: String,
    pub(crate) service_id: String,
    pub(crate) version: semver::Version,
    pub(crate) image: String,
    pub(crate) channel: ReleaseChannel,
    pub(crate) catalog_source_id: String,
    pub(crate) catalog_id: String,
    pub(crate) verified_key_ids: Vec<String>,
    pub(crate) updated_at_ms: i64,
}

pub(crate) fn release_history(
    storage: &DurableStore,
    service_id: &str,
) -> Result<Vec<ReleaseHistoryProof>, StoreError> {
    let mut history = storage
        .operation_store()
        .list()
        .map_err(|error| StoreError::new(500, "STORE_OPERATION_ERROR", error.to_string()))?
        .into_iter()
        .filter_map(release_history_proof)
        .filter(|proof| proof.service_id == service_id)
        .collect::<Vec<_>>();
    history.sort_by(|left, right| right.updated_at_ms.cmp(&left.updated_at_ms));
    Ok(history)
}

pub(crate) fn provider_revision_from_operation(
    storage: &DurableStore,
    operation_id: &str,
) -> Result<ReleaseProviderRevision, StoreError> {
    let operation = storage
        .operation_store()
        .get(operation_id)
        .map_err(|error| StoreError::new(500, "STORE_OPERATION_ERROR", error.to_string()))?
        .ok_or_else(|| {
            StoreError::new(
                409,
                "STORE_PROVIDER_REVISION_MISSING",
                format!("proven release Operation {operation_id} no longer exists"),
            )
        })?;
    let service_id = operation
        .request
        .get("service_id")
        .and_then(Value::as_str)
        .unwrap_or_default();
    for planned in operation.planned_jobs.iter().rev() {
        match planned.kind {
            JobKind::ReleasePipeline => {
                let pipeline: ReleasePipelinePayload =
                    serde_json::from_value(planned.payload.clone()).map_err(|error| {
                        StoreError::new(
                            500,
                            "STORE_PROVIDER_REVISION_INVALID",
                            format!(
                                "decode provider revision from Operation {operation_id}: {error}"
                            ),
                        )
                    })?;
                if pipeline.install.spec.service_id == service_id {
                    return Ok(ReleaseProviderRevision {
                        revision_id: operation_id.to_string(),
                        auth: pipeline.auth,
                        provisioners: pipeline.provisioners,
                        gateway: pipeline.gateway,
                    });
                }
            }
            JobKind::Upgrade | JobKind::Rollback => {
                let replacement: ReleaseReplacementPayload =
                    serde_json::from_value(planned.payload.clone()).map_err(|error| {
                        StoreError::new(
                            500,
                            "STORE_PROVIDER_REVISION_INVALID",
                            format!(
                                "decode replacement provider revision from Operation {operation_id}: {error}"
                            ),
                        )
                    })?;
                if replacement.new_spec.service_id == service_id {
                    if let Some(saga) = replacement.provider_saga {
                        return Ok(saga.desired);
                    }
                    break;
                }
            }
            _ => {}
        }
    }
    Ok(ReleaseProviderRevision {
        revision_id: operation_id.to_string(),
        ..ReleaseProviderRevision::default()
    })
}

pub(crate) fn release_history_proof(operation: DurableOperation) -> Option<ReleaseHistoryProof> {
    if operation.status != DurableOperationStatus::Succeeded
        || !matches!(
            operation.action.as_str(),
            "release.install" | "release.upgrade" | "release.rollback"
        )
        || operation.request.get("start").and_then(Value::as_bool) != Some(true)
    {
        return None;
    }
    let request = operation.request.as_object()?;
    let service_id = request.get("service_id")?.as_str()?.trim();
    let version = semver::Version::parse(request.get("version")?.as_str()?).ok()?;
    let image = request.get("image")?.as_str()?.trim();
    OciImageReference::parse(image).ok()?;
    let deployment_id = request.get("deployment_id")?.as_str()?.trim();
    let catalog_source_id = request.get("catalog_source_id")?.as_str()?.trim();
    let catalog_id = request.get("catalog_id")?.as_str()?.trim();
    let verified_key_ids = request
        .get("catalog_verified_key_ids")?
        .as_array()?
        .iter()
        .map(|value| value.as_str().map(str::to_string))
        .collect::<Option<Vec<_>>>()?;
    let channel = match request.get("channel").and_then(Value::as_str) {
        Some(value) => history_release_channel(value)?,
        None => ReleaseChannel::Stable,
    };
    if service_id.is_empty()
        || deployment_id.is_empty()
        || catalog_source_id.is_empty()
        || catalog_id.is_empty()
        || verified_key_ids.is_empty()
        || verified_key_ids
            .iter()
            .any(|key_id| key_id.trim().is_empty())
    {
        return None;
    }
    Some(ReleaseHistoryProof {
        operation_id: operation.operation_id,
        deployment_id: deployment_id.to_string(),
        service_id: service_id.to_string(),
        version,
        image: image.to_string(),
        channel,
        catalog_source_id: catalog_source_id.to_string(),
        catalog_id: catalog_id.to_string(),
        verified_key_ids,
        updated_at_ms: operation.updated_at_ms,
    })
}

pub(crate) fn history_release_channel(value: &str) -> Option<ReleaseChannel> {
    match value.trim().to_ascii_lowercase().as_str() {
        "stable" => Some(ReleaseChannel::Stable),
        "beta" => Some(ReleaseChannel::Beta),
        "nightly" => Some(ReleaseChannel::Nightly),
        _ => None,
    }
}
