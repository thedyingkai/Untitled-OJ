//! Read and preview capabilities for the Store validation use case.
//! No import, enqueue, transaction commit or container execution is exposed by this adapter.
use crate::catalog_registry::{CatalogRegistry, ResolvedCatalogPlan, VerifiedReleaseDocument};
use crate::durable::DurableStore;
use crate::store::bindings::preview_install_api_bindings;
use crate::store::bindings::resolve_install_api_bindings;
use crate::store::bindings::selected_topology_spec;
use crate::store::commands::non_empty;
use crate::store::commands::parse_release_channel;
use crate::store::composition::store_composition_providers;
use crate::store::error::StoreError;
use crate::store::error::catalog_registry_error;
use crate::store::error::core_error;
use crate::store::error::storage_error;
use crate::store::node::ensure_ready_docker_node;
use crate::store::node::ensure_release_runtime_supported;
use crate::store::node::node_runtime_facts;
use crate::store::node::target_platform;
use crate::store::placement::container_spec;
use crate::store::placement::effective_managed_endpoint;
use crate::store::placement::managed_published_endpoint;
use crate::store::runtime_plan::release_pipeline_payload;
use crate::store::service_context::attach_release_runtime_volume;
use crate::store::service_context::contract_has_retained_runtime_volume;
use crate::store::service_context::managed_service_context_spec;
use crate::store::topology::preview_store_install_topology_spec;
use orchestrator_legacy::composition::ProviderCandidateV1;
use orchestrator_legacy::topology_v1::TopologyDiff;
use orchestrator_legacy::{
    ApiBinding, NodeRecord, OrchestratorActionConsole, ServiceReleaseContract, diff_topology_specs,
};
use orchestrator_manager::catalog_v2::TargetPlatform;
use orchestrator_manager::store::validation::{
    ReleaseValidationReadPort, ValidateRelease, ValidationContext, ValidationTarget,
};
use orchestrator_protocol::{NodeRuntimeFactsV1, RuntimeContract};
use orchestrator_runtime::{HealthGatePolicy, OciImageReference, RuntimeInstallPayload};
use serde_json::Value;
use std::collections::BTreeMap;

pub(crate) struct StoreValidationReader<'a> {
    pub(crate) console: &'a OrchestratorActionConsole,
    pub(crate) storage: &'a DurableStore,
    pub(crate) registry: &'a CatalogRegistry,
}

impl ReleaseValidationReadPort for StoreValidationReader<'_> {
    type Error = StoreError;

    fn target(&self, node_id: &str) -> Result<ValidationTarget, Self::Error> {
        let node = self
            .storage
            .get_node(node_id)
            .map_err(storage_error)?
            .ok_or_else(|| {
                StoreError::new(
                    404,
                    "STORE_TARGET_NODE_NOT_FOUND",
                    format!("target Node {node_id} was not found"),
                )
            })?;
        ensure_ready_docker_node(self.storage, &node)?;
        let platform = target_platform(self.storage, &node)?;
        Ok(ValidationTarget { node, platform })
    }

    fn catalog(
        &self,
        input: &ValidateRelease,
        platform: &TargetPlatform,
    ) -> Result<(ResolvedCatalogPlan, Vec<VerifiedReleaseDocument>), Self::Error> {
        let resolved = self
            .registry
            .resolve_install_plan(
                self.storage,
                non_empty(&input.catalog_source_id),
                &input.service_id,
                non_empty(&input.version),
                parse_release_channel(&input.channel)?,
                platform.clone(),
            )
            .map_err(catalog_registry_error)?;
        let documents = self
            .registry
            .fetch_release_documents(self.storage, &resolved)
            .map_err(catalog_registry_error)?;
        Ok((resolved, documents))
    }

    fn composition_providers(
        &self,
        documents: &[VerifiedReleaseDocument],
        node: &NodeRecord,
    ) -> Result<Vec<ProviderCandidateV1>, Self::Error> {
        store_composition_providers(self.storage, documents, node)
    }

    fn runtime_support(
        &self,
        node: &NodeRecord,
        contract: &ServiceReleaseContract,
        image: &str,
    ) -> Result<(RuntimeContract, NodeRuntimeFactsV1), Self::Error> {
        let runtime_contract =
            ensure_release_runtime_supported(self.storage, node, contract, image)?;
        let facts = node_runtime_facts(self.storage, &node.node_id)?;
        Ok((runtime_contract, facts))
    }

    fn endpoint(
        &self,
        input: &ValidateRelease,
        node: &NodeRecord,
        contract: &ServiceReleaseContract,
    ) -> Result<String, Self::Error> {
        effective_managed_endpoint(&input.endpoint, node, &contract.release)
    }

    fn preview_bindings(
        &self,
        context: &ValidationContext<'_>,
    ) -> Result<(bool, Vec<Value>), Self::Error> {
        preview_install_api_bindings(
            self.console,
            self.storage,
            context.contract,
            &context.node.node_id,
            context.endpoint,
            &context.input.bindings,
            context.input.topology.as_ref(),
        )
    }

    fn resolve_bindings(
        &self,
        context: &ValidationContext<'_>,
    ) -> Result<Vec<ApiBinding>, Self::Error> {
        resolve_install_api_bindings(
            self.console,
            self.storage,
            context.contract,
            context.deployment_id,
            &context.node.node_id,
            context.endpoint,
            &context.input.bindings,
            context.input.topology.as_ref(),
            false,
        )
    }

    fn validate_runtime_plan(
        &self,
        context: &ValidationContext<'_>,
        binding_plan: &[ApiBinding],
        bindings_valid: bool,
        config: &Value,
        secret_refs: &BTreeMap<String, String>,
    ) -> Result<(), Self::Error> {
        let input = context.input;
        let node = context.node;
        let root_document = context.document;
        let contract = context.contract;
        let planned_deployment_id = context.deployment_id;
        let service_id = input.service_id.as_str();
        let effective_endpoint = context.endpoint;
        let runtime_contract = context.runtime_contract.clone();
        let storage = self.storage;
        let image = OciImageReference::parse(root_document.selection.release.oci_image.as_str())
            .map_err(|error| {
                StoreError::new(
                    422,
                    "STORE_IMMUTABLE_IMAGE_REQUIRED",
                    format!("validation release image is not immutable: {error}"),
                )
            })?;
        let mut preview_spec = container_spec(
            &planned_deployment_id,
            service_id,
            &root_document.selection.release.version,
            &root_document.checksum,
            &node,
            image,
            runtime_contract.clone(),
            &contract.release,
            managed_published_endpoint(&effective_endpoint, service_id, &node, &contract.release)?,
        );
        preview_spec.labels.insert(
            "ojos.service_contract_version".to_string(),
            contract.contract_version.to_string(),
        );
        attach_release_runtime_volume(&mut preview_spec, &contract)?;
        if bindings_valid
            && (!contract.requirements().is_empty()
                || !contract.events.publishes.is_empty()
                || !contract.events.subscribes.is_empty()
                || contract_has_retained_runtime_volume(&contract))
        {
            preview_spec.managed_service_context = managed_service_context_spec(
                storage,
                &contract,
                &node.node_id,
                &binding_plan,
                true,
            )?;
        }
        let preview_health_gate =
            HealthGatePolicy::for_runtime_contract(&preview_spec.runtime_contract);
        let preview_install = RuntimeInstallPayload {
            spec: preview_spec,
            start: input.start,
            health_gate: preview_health_gate,
            offline_oci_artifact: None,
        };

        release_pipeline_payload(
            &contract.release,
            contract,
            &preview_install,
            binding_plan,
            node,
            &input.validation_id,
            &input.migration_policy,
            &input.gateway_node_id,
            config,
            secret_refs,
        )?;
        Ok(())
    }

    fn topology_diff(
        &self,
        context: &ValidationContext<'_>,
        bindings: &[ApiBinding],
    ) -> Result<Option<TopologyDiff>, Self::Error> {
        context
            .input
            .topology
            .as_ref()
            .map(|selection| {
                let (current, _) = selected_topology_spec(self.storage, selection)?;
                let proposed = preview_store_install_topology_spec(
                    current.clone(),
                    context.contract,
                    context.deployment_id,
                    &context.node.node_id,
                    context.endpoint,
                    bindings,
                )?;
                diff_topology_specs(Some(&current), &proposed).map_err(core_error)
            })
            .transpose()
    }
}
