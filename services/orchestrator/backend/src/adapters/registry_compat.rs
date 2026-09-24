//! Explicit adapters for repository files and historical public projections.
//! No Console/action dispatcher or runtime/provider execution is permitted here.
use orchestrator_core::{Result, ServiceReleaseManifest, SharedSchemas};
pub(crate) use orchestrator_legacy::{
    DiagnosticExport, ExternalReleaseImport, RegistryBootstrap, build_diagnostic_report,
    export_diagnostic_report, external_release_import_from_yaml, load_durable_registry,
    load_ephemeral_registry, register_external_release_into_store,
};
use orchestrator_storage::OrchestratorStore;
use std::collections::BTreeMap;

pub(crate) fn release_manifest(value: serde_json::Value) -> Result<ServiceReleaseManifest> {
    orchestrator_legacy::project_release_manifest(value)
}
pub(crate) fn installed_services(
    schemas: SharedSchemas,
    store: &impl OrchestratorStore,
) -> anyhow::Result<BTreeMap<String, orchestrator_manager::InstalledServiceView>> {
    let view = orchestrator_legacy::load_orchestrator_view_from_store(schemas, store)?;
    orchestrator_manager::installed_services_from_deployments(view.deployments)
}
