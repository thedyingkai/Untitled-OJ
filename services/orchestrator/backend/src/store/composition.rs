//! Store composition responsibilities.
use crate::catalog_registry::VerifiedReleaseDocument;
use crate::durable::DurableStore;
use crate::store::context::now_ms;
use crate::store::error::{StoreError, storage_error};
use crate::store::node::node_provider_label;
use orchestrator_core::NodeRecord;
use orchestrator_core::composition::CompositionPlanV1;
use orchestrator_core::composition::ProviderCandidateV1;
use orchestrator_core::composition::ProviderKindV1;
use orchestrator_manager::store::composition::normalize_composition_version;
use orchestrator_manager::store::composition::plan_composition;
use orchestrator_manager::store::composition::release_contract_from_document;
use orchestrator_manager::store::composition::release_graph;
use orchestrator_protocol::RuntimeObservedState;
use serde_json::Value;

pub(crate) fn build_store_composition_plan(
    storage: &DurableStore,
    documents: &[VerifiedReleaseDocument],
    root_service_id: &str,
    node: &NodeRecord,
) -> Result<CompositionPlanV1, StoreError> {
    // Preserve failure order: signed metadata is validated before provider I/O.
    let graph = release_graph(documents, root_service_id)?;
    let providers = store_composition_providers(storage, documents, node)?;
    plan_composition(graph, &providers).map_err(Into::into)
}

pub(crate) fn store_composition_providers(
    storage: &DurableStore,
    documents: &[VerifiedReleaseDocument],
    node: &NodeRecord,
) -> Result<Vec<ProviderCandidateV1>, StoreError> {
    let mut providers = Vec::new();
    for document in documents {
        let contract = release_contract_from_document(document)?;
        for api in &contract.release.apis {
            let Some(version) = normalize_composition_version(&api.version) else {
                continue;
            };
            providers.push(ProviderCandidateV1 {
                provider_id: format!("package:{}:{}", contract.release.service_name, api.api_id),
                capability: api.api_id.clone(),
                version,
                kind: ProviderKindV1::Package,
                service_id: Some(contract.release.service_name.clone()),
            });
        }
    }
    let evidence_at_ms = now_ms();
    for stored in storage.runtime_instances(None).map_err(storage_error)? {
        let stored = storage
            .runtime_with_current_evidence(stored, evidence_at_ms)
            .map_err(storage_error)?;
        let managed_evidence_ready =
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
        if stored.instance.observed_state != RuntimeObservedState::Running
            || !stored.instance.health.eq_ignore_ascii_case("HEALTHY")
            || !managed_evidence_ready
            || stored.endpoint.trim().is_empty()
        {
            continue;
        }
        // Runtime API providers are independent of package dependencies in
        // the release graph. Resolve their exact, already-registered Service
        // Contract from durable identity rather than requiring the provider's
        // release metadata to be repeated in this install's Catalog plan.
        let Some(contract) = storage
            .service_release_contract(
                &stored.instance.service_id,
                &stored.instance.release_version,
            )
            .map_err(storage_error)?
        else {
            continue;
        };
        if contract.release.service_name != stored.instance.service_id
            || contract.release.version != stored.instance.release_version
        {
            return Err(StoreError::new(
                409,
                "STORE_COMPOSITION_PROVIDER_RELEASE_MISMATCH",
                format!(
                    "provider deployment {} runtime identity {}@{} does not match its registered contract {}@{}",
                    stored.instance.deployment_id,
                    stored.instance.service_id,
                    stored.instance.release_version,
                    contract.release.service_name,
                    contract.release.version,
                ),
            ));
        }
        for api in &contract.release.apis {
            if let Some(version) = normalize_composition_version(&api.version) {
                providers.push(ProviderCandidateV1 {
                    provider_id: stored.instance.deployment_id.clone(),
                    capability: api.api_id.clone(),
                    version,
                    kind: match stored.management_mode {
                        orchestrator_storage::RuntimeManagementMode::Managed => {
                            ProviderKindV1::Managed
                        }
                        orchestrator_storage::RuntimeManagementMode::External => {
                            ProviderKindV1::External
                        }
                    },
                    service_id: Some(stored.instance.service_id.clone()),
                });
            }
        }
    }
    if let Some(Value::Object(postgresql)) = node_provider_label(node, "postgresql") {
        let enabled = postgresql
            .get("enabled")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let provider_id = postgresql
            .get("provider_id")
            .or_else(|| postgresql.get("connection_id"))
            .and_then(Value::as_str)
            .map(str::trim)
            .unwrap_or_default();
        if enabled && !provider_id.is_empty() {
            providers.push(ProviderCandidateV1 {
                provider_id: provider_id.to_string(),
                capability: "postgresql.database".to_string(),
                version: semver::Version::parse("1.0.0").expect("static semver"),
                kind: ProviderKindV1::Managed,
                service_id: None,
            });
        }
    }
    providers.sort_by(|left, right| {
        left.capability
            .cmp(&right.capability)
            .then(left.provider_id.cmp(&right.provider_id))
    });
    providers.dedup_by(|left, right| {
        left.capability == right.capability && left.provider_id == right.provider_id
    });
    Ok(providers)
}
