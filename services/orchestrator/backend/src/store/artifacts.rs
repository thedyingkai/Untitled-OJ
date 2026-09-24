//! Store artifacts responsibilities.
use crate::artifact_store::ArtifactRetentionPolicy;
use crate::artifact_store::ArtifactStore;
use crate::artifact_store::MAX_ARTIFACT_BYTES;
use crate::catalog_registry::ResolvedCatalogPlan;
use crate::catalog_registry::VerifiedReleaseDocument;
use crate::durable::DurableStore;
use crate::store::error::{StoreError, storage_error};
use orchestrator_control_plane::JobStore;
use orchestrator_runtime::ArtifactReference;
use orchestrator_runtime::RuntimeObservedState;
use std::fs;
use std::path::Path;
use std::time::SystemTime;

pub(crate) fn image_digest(image: &str) -> Result<&str, StoreError> {
    image
        .split_once('@')
        .map(|(_, digest)| digest)
        .ok_or_else(|| {
            StoreError::new(
                500,
                "STORE_IMMUTABLE_IMAGE_REQUIRED",
                "validated OCI image unexpectedly has no digest",
            )
        })
}

pub(crate) fn artifact_matches(expected: &str, observed: &str) -> bool {
    expected == observed
        || artifact_digest(expected)
            .zip(artifact_digest(observed))
            .is_some_and(|(expected, observed)| expected == observed)
}

pub(crate) fn artifact_digest(value: &str) -> Option<&str> {
    value
        .strip_prefix("sha256:")
        .or_else(|| value.split_once("@sha256:").map(|(_, digest)| digest))
}

pub(crate) fn missing_resolved_dependencies<'a>(
    storage: &DurableStore,
    resolved: &'a ResolvedCatalogPlan,
    root_service_id: &str,
) -> Result<Vec<&'a orchestrator_manager::catalog_v2::ResolvedReleaseV2>, StoreError> {
    let deployments = storage.runtime_instances(None).map_err(storage_error)?;
    Ok(resolved
        .plan
        .releases
        .iter()
        .filter(|selection| selection.module_id != root_service_id)
        .filter(|selection| {
            let expected_digest = selection.release.oci_image.digest().as_str();
            !deployments.iter().any(|deployment| {
                deployment.instance.service_id == selection.module_id
                    && deployment.instance.observed_state == RuntimeObservedState::Running
                    && deployment.instance.health.eq_ignore_ascii_case("HEALTHY")
                    && (deployment.instance.artifact_digest == expected_digest
                        || deployment
                            .instance
                            .artifact_digest
                            .ends_with(expected_digest))
            })
        })
        .collect())
}

pub(crate) fn offline_artifact_for_release(
    storage: &DurableStore,
    artifact_store: Option<&ArtifactStore>,
    documents: &[VerifiedReleaseDocument],
    service_id: &str,
    version: &semver::Version,
) -> Result<Option<ArtifactReference>, StoreError> {
    let layout = documents
        .iter()
        .find(|document| {
            document.selection.module_id == service_id
                && &document.selection.release.version == version
        })
        .and_then(|document| document.offline_oci_layout.as_deref());
    let Some(layout) = layout else {
        return Ok(None);
    };
    let store = artifact_store.ok_or_else(|| {
        StoreError::new(
            503,
            "STORE_ARTIFACT_STORAGE_UNAVAILABLE",
            "offline OCI install requires configured durable artifact storage",
        )
    })?;
    let protected = storage
        .job_store()
        .list()
        .map_err(|error| {
            StoreError::new(
                500,
                "STORE_ARTIFACT_RETENTION_FAILED",
                format!("list durable Jobs before artifact retention: {error}"),
            )
        })?
        .into_iter()
        .filter(|job| !job.status.is_terminal())
        .filter_map(|job| {
            ["/offline_oci_artifact", "/install/offline_oci_artifact"]
                .iter()
                .filter_map(|pointer| job.payload.pointer(pointer))
                .find_map(|value| serde_json::from_value::<ArtifactReference>(value.clone()).ok())
        })
        .map(|reference| reference.artifact_id)
        .collect();
    let policy = ArtifactRetentionPolicy::from_env().map_err(|error| {
        StoreError::new(500, "STORE_ARTIFACT_RETENTION_FAILED", error.to_string())
    })?;
    store
        .collect_garbage(&protected, policy, SystemTime::now())
        .map_err(|error| {
            StoreError::new(500, "STORE_ARTIFACT_RETENTION_FAILED", error.to_string())
        })?;
    build_offline_oci_artifact(store, layout).map(Some)
}

pub(crate) fn build_offline_oci_artifact(
    store: &ArtifactStore,
    root: &Path,
) -> Result<ArtifactReference, StoreError> {
    let source_bytes = checked_layout_size(root)?;
    if source_bytes > MAX_ARTIFACT_BYTES {
        return Err(StoreError::new(
            422,
            "CATALOG_OFFLINE_OCI_TOO_LARGE",
            format!(
                "offline OCI layout {} is {} bytes; v1 Agent transfer limit is {} bytes",
                root.display(),
                source_bytes,
                MAX_ARTIFACT_BYTES
            ),
        ));
    }
    store.create_oci_archive(root).map_err(|error| {
        StoreError::new(
            422,
            "CATALOG_OFFLINE_OCI_ARCHIVE_FAILED",
            format!("persist offline OCI archive {}: {error}", root.display()),
        )
    })
}

pub(crate) fn checked_layout_size(path: &Path) -> Result<u64, StoreError> {
    let metadata = fs::symlink_metadata(path).map_err(|error| {
        StoreError::new(
            422,
            "CATALOG_OFFLINE_OCI_ARCHIVE_FAILED",
            format!("inspect {}: {error}", path.display()),
        )
    })?;
    if metadata.file_type().is_symlink() {
        return Err(StoreError::new(
            422,
            "CATALOG_OFFLINE_OCI_ARCHIVE_FAILED",
            format!("offline OCI layout contains symlink {}", path.display()),
        ));
    }
    if metadata.is_file() {
        return Ok(metadata.len());
    }
    if !metadata.is_dir() {
        return Err(StoreError::new(
            422,
            "CATALOG_OFFLINE_OCI_ARCHIVE_FAILED",
            format!(
                "offline OCI entry is neither file nor directory: {}",
                path.display()
            ),
        ));
    }
    let mut total = 0_u64;
    for entry in fs::read_dir(path).map_err(|error| {
        StoreError::new(
            422,
            "CATALOG_OFFLINE_OCI_ARCHIVE_FAILED",
            format!("read {}: {error}", path.display()),
        )
    })? {
        let entry = entry.map_err(|error| {
            StoreError::new(
                422,
                "CATALOG_OFFLINE_OCI_ARCHIVE_FAILED",
                format!("read {} entry: {error}", path.display()),
            )
        })?;
        total = total
            .checked_add(checked_layout_size(&entry.path())?)
            .ok_or_else(|| {
                StoreError::new(
                    422,
                    "CATALOG_OFFLINE_OCI_TOO_LARGE",
                    "offline OCI layout size overflow",
                )
            })?;
        if total > MAX_ARTIFACT_BYTES {
            break;
        }
    }
    Ok(total)
}

pub(crate) fn is_sha256(value: &str) -> bool {
    value.strip_prefix("sha256:").is_some_and(|hex| {
        hex.len() == 64
            && hex
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    })
}
