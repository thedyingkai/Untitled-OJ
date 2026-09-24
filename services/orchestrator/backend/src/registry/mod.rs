//! Request-local application context over one shared repository.
mod operations;
use crate::adapters::registry_compat;
use orchestrator_core::{
    ActionDispatchResult, DiagnosticReport, OrchestratorError, Result, ServiceRelease,
    SharedSchemas,
};
use orchestrator_storage::{OrchestratorStore, SharedOrchestratorStore};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::path::Path;

#[derive(Debug, Clone)]
pub(crate) struct RegistryContext {
    schemas: SharedSchemas,
    store: SharedOrchestratorStore,
    warnings: Vec<String>,
}
impl RegistryContext {
    pub(crate) fn load_ephemeral(repo_root: impl AsRef<Path>) -> Result<Self> {
        registry_compat::load_ephemeral_registry(repo_root.as_ref()).map(Self::from_bootstrap)
    }
    pub(crate) fn load_with_store(
        repo_root: impl AsRef<Path>,
        kind: &str,
        store: impl OrchestratorStore + Send + 'static,
    ) -> Result<Self> {
        registry_compat::load_durable_registry(repo_root.as_ref(), kind, store)
            .map(Self::from_bootstrap)
    }
    pub(crate) fn from_bootstrap(value: registry_compat::RegistryBootstrap) -> Self {
        Self {
            schemas: value.schemas,
            store: value.store,
            warnings: value.warnings,
        }
    }
    pub(crate) fn bootstrap_snapshot(&self) -> registry_compat::RegistryBootstrap {
        registry_compat::RegistryBootstrap {
            schemas: self.schemas.clone(),
            store: self.store.clone(),
            warnings: self.warnings.clone(),
        }
    }
    pub(crate) fn uses_persistent_store(&self) -> bool {
        self.store.kind() != "memory"
    }
    pub(crate) fn store_kind(&self) -> &str {
        self.store.kind()
    }
    pub(crate) fn warnings(&self) -> &[String] {
        &self.warnings
    }
    pub(crate) fn service_releases(&self) -> Result<Vec<ServiceRelease>> {
        self.store.list_service_releases()
    }
    pub(crate) fn installed_services(
        &self,
    ) -> anyhow::Result<BTreeMap<String, orchestrator_manager::InstalledServiceView>> {
        registry_compat::installed_services(self.schemas.clone(), &self.store)
    }
    pub(crate) fn delete_release(
        &mut self,
        operation_id: String,
        service_id: &str,
        version: &str,
    ) -> Result<ActionDispatchResult> {
        operations::delete_release(&mut self.store, operation_id, service_id, version)
    }
    pub(crate) fn create_diagnostic(&mut self) -> Result<ActionDispatchResult> {
        operations::create_diagnostic(&mut self.store)
    }
    pub(crate) fn diagnostic_reports(&self) -> Result<Vec<DiagnosticReport>> {
        self.store.list_diagnostic_reports()
    }
    pub(crate) fn diagnostic_report(&self, report_id: &str) -> Result<Option<DiagnosticReport>> {
        self.store.get_diagnostic_report(report_id)
    }
    pub(crate) fn diagnostic_export(
        &self,
        report_id: &str,
        format: &str,
    ) -> Result<registry_compat::DiagnosticExport> {
        let report = self.diagnostic_report(report_id)?.ok_or_else(|| {
            OrchestratorError::Dependency(format!("diagnostic report {report_id} not found"))
        })?;
        registry_compat::export_diagnostic_report(&report, format)
    }
    /// Registers an already-fetched release document without performing any
    /// network I/O. The exact raw bytes are hashed again before parsing, and
    /// Service + Release publication uses the store's atomic write primitive.
    pub fn register_external_release_document(
        &mut self,
        document: &[u8],
        source_url: &str,
        expected_checksum: &str,
    ) -> Result<registry_compat::ExternalReleaseImport> {
        let actual_checksum = format!("sha256:{:x}", Sha256::digest(document));
        if expected_checksum.trim() != actual_checksum {
            return Err(OrchestratorError::InvalidManifest(format!(
                "release metadata checksum mismatch: expected {}, got {actual_checksum}",
                expected_checksum.trim()
            )));
        }
        let text = std::str::from_utf8(document).map_err(|error| {
            OrchestratorError::InvalidManifest(format!(
                "release metadata must be UTF-8 YAML or JSON: {error}"
            ))
        })?;
        let mut import =
            registry_compat::external_release_import_from_yaml(text, source_url, &actual_checksum)?;
        registry_compat::register_external_release_into_store(&mut self.store, &mut import)?;
        Ok(import)
    }
}
