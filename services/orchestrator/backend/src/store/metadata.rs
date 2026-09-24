//! Store metadata responsibilities.
use crate::catalog_registry::CatalogRegistry;
use crate::catalog_registry::VerifiedReleaseDocument;
use crate::durable::DurableStore;
use crate::store::artifacts::is_sha256;
use crate::store::commands::{
    ImportReleaseRequest, ReleaseDeleteRequest, non_empty, parse_release_channel, required_text,
};
use crate::store::context::{MutationContext, operation_id};
use crate::store::error::{StoreError, catalog_registry_error, core_error, storage_error};
use crate::store::history::release_history;
use crate::store::node::target_platform;
use orchestrator_legacy::ActionRequest;
use orchestrator_legacy::OrchestratorActionConsole;
use orchestrator_legacy::ServiceRelease;
use orchestrator_legacy::ServiceReleaseContract;
use orchestrator_legacy::ServiceReleaseManifest;
use orchestrator_legacy::validate_service_release;
use orchestrator_manager::store::composition::release_contract_from_document;
use serde_json::Value;
use serde_json::json;
use std::collections::BTreeMap;

pub(crate) fn import_release(
    console: &mut OrchestratorActionConsole,
    storage: &DurableStore,
    registry: &CatalogRegistry,
    input: ImportReleaseRequest,
) -> Result<Value, StoreError> {
    let service_id = required_text(&input.service_id, "service_id")?;
    let node_id = required_text(&input.target_node_id, "target_node_id")?;
    let node = storage
        .get_node(node_id)
        .map_err(storage_error)?
        .ok_or_else(|| {
            StoreError::new(
                404,
                "STORE_TARGET_NODE_NOT_FOUND",
                format!("target Node {node_id} was not found"),
            )
        })?;
    let platform = target_platform(storage, &node)?;
    let resolved = registry
        .resolve_install_plan(
            storage,
            non_empty(&input.catalog_source_id),
            service_id,
            non_empty(&input.version),
            parse_release_channel(&input.channel)?,
            platform.clone(),
        )
        .map_err(catalog_registry_error)?;
    // Catalog signatures, dependency resolution, metadata checksums and the
    // immutable OCI references are all verified before the first publication.
    // Import is metadata-only: it never creates an Operation or calls an Agent.
    let documents = registry
        .fetch_release_documents(storage, &resolved)
        .map_err(catalog_registry_error)?;
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
    Ok(json!({
        "imported": imported,
        "catalog_source_id": resolved.source_id,
        "catalog_id": resolved.catalog_id,
        "verified_key_ids": resolved.verified_key_ids,
        "target_platform": platform,
        "side_effects": {
            "operations": 0,
            "jobs": 0,
            "runtime_calls": 0,
        },
    }))
}

pub(crate) fn delete_release_metadata(
    console: &mut OrchestratorActionConsole,
    storage: &DurableStore,
    input: ReleaseDeleteRequest,
    request: &MutationContext,
) -> Result<Value, StoreError> {
    let service_id = required_text(&input.service_id, "service_id")?;
    let version = required_text(&input.version, "version")?;
    let selected = select_release(console, service_id, Some(version))?;

    // RuntimeInstance does not duplicate mutable version labels. The trusted
    // successful Store Operation is the proof tying a deployment to its
    // immutable Catalog version. If that proof is missing, deletion fails
    // closed rather than orphaning metadata that may still be in use.
    let history = release_history(storage, service_id)?;
    for deployment in storage
        .runtime_instances(None)
        .map_err(storage_error)?
        .into_iter()
        .filter(|deployment| deployment.instance.service_id == service_id)
    {
        let proof = history
            .iter()
            .find(|proof| proof.deployment_id == deployment.instance.deployment_id)
            .ok_or_else(|| {
                StoreError::new(
                    409,
                    "STORE_RELEASE_REFERENCE_UNKNOWN",
                    format!(
                        "deployment {} may reference {service_id}@{} but has no successful trusted Store Operation proving its version",
                        deployment.instance.deployment_id, selected.version
                    ),
                )
            })?;
        if proof.version == selected.version {
            return Err(StoreError::new(
                409,
                "STORE_RELEASE_IN_USE",
                format!(
                    "release {service_id}@{} is referenced by deployment {}; uninstall or upgrade that Deployment first",
                    selected.version, deployment.instance.deployment_id
                ),
            ));
        }
    }

    let target = format!("{service_id}@{}", selected.version);
    let operation_id = operation_id("release-delete", &target, request)?;
    let result = console
        .dispatch(ActionRequest::new(
            operation_id,
            "release.delete",
            BTreeMap::from([
                ("service_id".to_string(), service_id.to_string()),
                ("version".to_string(), selected.version.to_string()),
                ("confirm".to_string(), "true".to_string()),
            ]),
        ))
        .map_err(core_error)?;
    if !result.status.eq_ignore_ascii_case("SUCCEEDED") {
        return Err(StoreError::new(
            409,
            "STORE_RELEASE_DELETE_REJECTED",
            format!("release {target} deletion ended in {}", result.status),
        ));
    }
    Ok(json!({
        "service_id": service_id,
        "version": selected.version,
        "deleted": true,
        "action_result": result,
    }))
}

pub(crate) struct SelectedRelease {
    pub(crate) record: ServiceRelease,
    pub(crate) manifest: ServiceReleaseManifest,
    pub(crate) contract: ServiceReleaseContract,
    pub(crate) version: semver::Version,
}

pub(crate) fn select_release(
    console: &OrchestratorActionConsole,
    service_id: &str,
    requested_version: Option<&str>,
) -> Result<SelectedRelease, StoreError> {
    let requested = requested_version
        .map(semver::Version::parse)
        .transpose()
        .map_err(|error| {
            StoreError::new(
                422,
                "STORE_VERSION_INVALID",
                format!("requested version is not semver: {error}"),
            )
        })?;
    let mut candidates = Vec::new();
    for record in console.service_releases().map_err(core_error)? {
        if record.service_name != service_id {
            continue;
        }
        let version = semver::Version::parse(record.version.trim()).map_err(|error| {
            StoreError::new(
                422,
                "STORE_RELEASE_INVALID",
                format!(
                    "registered release {service_id}@{} is not semver: {error}",
                    record.version
                ),
            )
        })?;
        if requested
            .as_ref()
            .is_some_and(|requested| requested != &version)
            || (requested.is_none() && !version.pre.is_empty())
        {
            continue;
        }
        let contract =
            ServiceReleaseContract::from_json_value(record.manifest.clone()).map_err(|error| {
                StoreError::new(
                    422,
                    "STORE_RELEASE_INVALID",
                    format!(
                        "registered release {service_id}@{version} has an invalid manifest: {error}"
                    ),
                )
            })?;
        let manifest = contract.release.clone();
        validate_service_release(&manifest).map_err(core_error)?;
        if manifest.service_name != record.service_name || manifest.version != record.version {
            return Err(StoreError::new(
                409,
                "STORE_RELEASE_IDENTITY_MISMATCH",
                format!(
                    "release record {}@{} does not match its manifest {}@{}",
                    record.service_name, record.version, manifest.service_name, manifest.version
                ),
            ));
        }
        candidates.push(SelectedRelease {
            record,
            manifest,
            contract,
            version,
        });
    }
    candidates.sort_by(|left, right| left.version.cmp(&right.version));
    candidates.pop().ok_or_else(|| {
        StoreError::new(
            404,
            "STORE_RELEASE_NOT_FOUND",
            requested.map_or_else(
                || format!("service {service_id} has no stable registered release"),
                |version| format!("release {service_id}@{version} was not found"),
            ),
        )
    })
}

/// Select a release for a trusted Catalog operation from the documents verified

/// for this exact request. New imports persist the full versioned contract, but

/// pre-migration v1 records can still exist and durable state must never replace

/// the current Catalog's signed runtime, event, Auth, or Gateway semantics.

pub(crate) fn select_catalog_document_release(
    console: &OrchestratorActionConsole,
    documents: &[VerifiedReleaseDocument],
    service_id: &str,
    version: &semver::Version,
) -> Result<SelectedRelease, StoreError> {
    let document = documents
        .iter()
        .find(|document| {
            document.selection.module_id == service_id
                && document.selection.release.version == *version
        })
        .ok_or_else(|| {
            StoreError::new(
                500,
                "CATALOG_PLAN_INVALID",
                format!(
                    "resolved Catalog plan has no verified metadata for {service_id}@{version}"
                ),
            )
        })?;
    let mut selected = select_release(console, service_id, Some(&version.to_string()))?;
    let contract = release_contract_from_document(document)?;
    if contract.release.service_name != service_id
        || contract.release.version != version.to_string()
    {
        return Err(StoreError::new(
            500,
            "CATALOG_PLAN_INVALID",
            format!(
                "verified metadata identity {}@{} does not match Catalog selection {service_id}@{version}",
                contract.release.service_name, contract.release.version
            ),
        ));
    }
    selected.manifest = contract.release.clone();
    selected.record.manifest = contract.to_json_value().map_err(core_error)?;
    selected.record.release_url = document.source_url.clone();
    selected.record.checksum = document.checksum.clone();
    selected.contract = contract;
    Ok(selected)
}

pub(crate) fn ensure_release_checksum(record: &ServiceRelease) -> Result<(), StoreError> {
    let checksum = record.checksum.trim();
    if is_sha256(checksum) {
        Ok(())
    } else {
        Err(StoreError::new(
            422,
            "STORE_RELEASE_CHECKSUM_REQUIRED",
            format!(
                "release {}@{} must have a verified sha256:<64 lowercase hex> metadata checksum",
                record.service_name, record.version
            ),
        ))
    }
}
