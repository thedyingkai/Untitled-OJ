//! Read-only Release validation. The host supplies facts and preview capabilities;
//! this use case has no release-import, transaction, job-publication or runtime-execution port.

use super::composition::{
    composition_inputs_for_service, legacy_composition_inputs, plan_composition,
    release_contract_from_document, release_graph, validate_store_composition_inputs,
};
use super::{ResolvedCatalogPlan, StoreRuleError, VerifiedReleaseDocument};
use crate::catalog_v2::TargetPlatform;
use orchestrator_core::composition::ProviderCandidateV1;
use orchestrator_core::topology_v1::TopologyDiff;
use orchestrator_core::{ApiBinding, NodeRecord, ServiceReleaseContract};
use orchestrator_protocol::{NodeRuntimeFactsV1, RuntimeContract};
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InstallBindingSelection {
    pub name: String,
    pub provider_deployment_id: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InstallTopologySelection {
    pub topology_id: String,
    #[serde(default)]
    pub revision_id: String,
}

/// Normalized application input. HTTP compatibility fields and ETags are resolved by the adapter.
pub struct ValidateRelease {
    pub service_id: String,
    pub target_node_id: String,
    pub catalog_source_id: String,
    pub version: String,
    pub channel: String,
    pub endpoint: String,
    pub bindings: Vec<InstallBindingSelection>,
    pub topology: Option<InstallTopologySelection>,
    pub start: bool,
    pub migration_policy: String,
    pub gateway_node_id: String,
    pub config: Value,
    pub secret_refs: BTreeMap<String, String>,
    pub inputs: BTreeMap<String, BTreeMap<String, Value>>,
    pub validation_id: String,
}

pub struct ValidationTarget {
    pub node: NodeRecord,
    pub platform: TargetPlatform,
}

/// All planning inputs have explicit owners and are borrowed for one validation call.
pub struct ValidationContext<'a> {
    pub input: &'a ValidateRelease,
    pub node: &'a NodeRecord,
    pub document: &'a VerifiedReleaseDocument,
    pub contract: &'a ServiceReleaseContract,
    pub deployment_id: &'a str,
    pub endpoint: &'a str,
    pub runtime_contract: &'a RuntimeContract,
}

pub trait ReleaseValidationReadPort {
    type Error;

    fn target(&self, node_id: &str) -> Result<ValidationTarget, Self::Error>;
    fn catalog(
        &self,
        input: &ValidateRelease,
        platform: &TargetPlatform,
    ) -> Result<(ResolvedCatalogPlan, Vec<VerifiedReleaseDocument>), Self::Error>;
    fn composition_providers(
        &self,
        documents: &[VerifiedReleaseDocument],
        node: &NodeRecord,
    ) -> Result<Vec<ProviderCandidateV1>, Self::Error>;
    fn runtime_support(
        &self,
        node: &NodeRecord,
        contract: &ServiceReleaseContract,
        image: &str,
    ) -> Result<(RuntimeContract, NodeRuntimeFactsV1), Self::Error>;
    fn endpoint(
        &self,
        input: &ValidateRelease,
        node: &NodeRecord,
        contract: &ServiceReleaseContract,
    ) -> Result<String, Self::Error>;
    fn preview_bindings(
        &self,
        context: &ValidationContext<'_>,
    ) -> Result<(bool, Vec<Value>), Self::Error>;
    fn resolve_bindings(
        &self,
        context: &ValidationContext<'_>,
    ) -> Result<Vec<ApiBinding>, Self::Error>;
    fn validate_runtime_plan(
        &self,
        context: &ValidationContext<'_>,
        bindings: &[ApiBinding],
        bindings_valid: bool,
        config: &Value,
        secret_refs: &BTreeMap<String, String>,
    ) -> Result<(), Self::Error>;
    fn topology_diff(
        &self,
        context: &ValidationContext<'_>,
        bindings: &[ApiBinding],
    ) -> Result<Option<TopologyDiff>, Self::Error>;
}

#[derive(Debug)]
pub enum ReleaseValidationError<E> {
    Read(E),
    Rule(StoreRuleError),
    MissingRootMetadata,
}

impl<E> From<StoreRuleError> for ReleaseValidationError<E> {
    fn from(error: StoreRuleError) -> Self {
        Self::Rule(error)
    }
}

pub fn validate_release<P: ReleaseValidationReadPort>(
    port: &P,
    input: &ValidateRelease,
) -> Result<Value, ReleaseValidationError<P::Error>> {
    use ReleaseValidationError::Read;

    let ValidationTarget { node, platform } = port.target(&input.target_node_id).map_err(Read)?;
    let (resolved, documents) = port.catalog(input, &platform).map_err(Read)?;
    let root_document = documents
        .iter()
        .find(|document| {
            document.selection.module_id == input.service_id
                && document.selection.release.version == resolved.plan.root.version
        })
        .ok_or(ReleaseValidationError::MissingRootMetadata)?;
    // Keep metadata validation ahead of provider reads, preserving the established failure order.
    let graph = release_graph(&documents, &input.service_id)?;
    let providers = port
        .composition_providers(&documents, &node)
        .map_err(Read)?;
    let composition_plan = plan_composition(graph, &providers)?;
    let contract = release_contract_from_document(root_document)?;
    let composition_validation = validate_store_composition_inputs(
        &composition_plan,
        &composition_plan.plan_digest,
        &composition_plan.release_graph_digest,
        &input.inputs,
        (!input.config.is_null()).then(|| input.config.clone()),
        input.secret_refs.clone(),
    );
    let composition_validation = if contract.platform.is_none() {
        legacy_composition_inputs(&composition_plan, &input.config, &input.secret_refs)
    } else {
        composition_validation
    };
    let composition_error_detail = composition_validation
        .as_ref()
        .err()
        .map(|error| error.detail.clone());
    let (runtime_contract, runtime_facts) = port
        .runtime_support(
            &node,
            &contract,
            root_document.selection.release.oci_image.as_str(),
        )
        .map_err(Read)?;
    let deployment_id = super::deployment_id(
        &input.service_id,
        &resolved.plan.root.version,
        &node.node_id,
    );
    let endpoint = port.endpoint(input, &node, &contract).map_err(Read)?;
    let context = ValidationContext {
        input,
        node: &node,
        document: root_document,
        contract: &contract,
        deployment_id: &deployment_id,
        endpoint: &endpoint,
        runtime_contract: &runtime_contract,
    };
    let (bindings_resolvable, requirements) = port.preview_bindings(&context).map_err(Read)?;
    let topology_confirmation_required =
        !contract.requirements().is_empty() && input.topology.is_none();
    let bindings_valid = bindings_resolvable && !topology_confirmation_required;
    let binding_plan = if bindings_valid {
        port.resolve_bindings(&context).map_err(Read)?
    } else {
        Vec::new()
    };
    if contract.contract_version >= 2
        && (contract.platform.is_none() || composition_validation.is_ok())
    {
        let (config, secret_refs) = composition_validation
            .as_ref()
            .ok()
            .map(|validated| {
                composition_inputs_for_service(&composition_plan, validated, &input.service_id)
            })
            .unwrap_or_else(|| (input.config.clone(), input.secret_refs.clone()));
        port.validate_runtime_plan(
            &context,
            &binding_plan,
            bindings_valid,
            &config,
            &secret_refs,
        )
        .map_err(Read)?;
    }
    let topology_diff = if bindings_valid {
        port.topology_diff(&context, &binding_plan).map_err(Read)?
    } else {
        None
    };
    let metadata = documents.iter().map(|document| json!({
        "module_id": document.selection.module_id,
        "version": document.selection.release.version,
        "metadata_url": document.source_url,
        "metadata_sha256": document.checksum,
        "oci_image": document.selection.release.oci_image,
        "offline_oci_layout_verified": document.offline_oci_layout.as_ref().map(|path| path.display().to_string()),
    })).collect::<Vec<_>>();
    Ok(json!({
        "valid": bindings_valid && (contract.platform.is_none() || composition_validation.is_ok()),
        "catalog_source_id": resolved.source_id,
        "catalog_id": resolved.catalog_id,
        "verified_key_ids": resolved.verified_key_ids,
        "target_platform": platform,
        "plan": resolved.plan,
        "metadata": metadata,
        "bindings": binding_plan,
        "requirements": requirements,
        "composition_plan": composition_plan,
        "composition_inputs_valid": composition_validation.is_ok(),
        "composition_input_error": composition_error_detail,
        "topology_confirmation_required": topology_confirmation_required,
        "runtime": { "node_id": node.node_id, "contract": runtime_contract, "facts": runtime_facts },
        "topology": input.topology.as_ref().map(|selection| json!({
            "topology_id": selection.topology_id, "revision_id": selection.revision_id,
        })),
        "topology_diff": topology_diff,
        "side_effects": { "release_imports": 0, "operations": 0, "jobs": 0, "runtime_calls": 0 },
    }))
}
