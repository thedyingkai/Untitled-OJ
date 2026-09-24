//! Explicit repository-file compatibility bootstrap; never dispatches actions.
use crate::{Result, SharedSchemas};
use orchestrator_storage::{OrchestratorStore, SharedOrchestratorStore};
use std::path::Path;

#[derive(Debug, Clone)]
pub struct RegistryBootstrap {
    pub schemas: SharedSchemas,
    pub store: SharedOrchestratorStore,
    pub warnings: Vec<String>,
}

pub fn load_ephemeral_registry(repo_root: &Path) -> Result<RegistryBootstrap> {
    let context = crate::workbench::load_operation_workbench_context_from_repo(repo_root)?;
    let store = crate::dispatcher::memory_store_from_context(&context)?;
    Ok(RegistryBootstrap {
        schemas: context.schemas,
        store: SharedOrchestratorStore::new("memory", store),
        warnings: context.warnings,
    })
}

pub fn load_durable_registry(
    repo_root: &Path,
    kind: &str,
    store: impl OrchestratorStore + Send + 'static,
) -> Result<RegistryBootstrap> {
    let context = crate::workbench::load_operation_workbench_context_from_repo(repo_root)?;
    // Preserve the original seed validation before publishing repository rows.
    let _ = crate::dispatcher::memory_store_from_context(&context)?;
    let mut store = SharedOrchestratorStore::new(kind, store);
    crate::dispatcher::sync_repo_manifest_registry_to_store(&mut store, &context)?;
    Ok(RegistryBootstrap {
        schemas: context.schemas,
        store,
        warnings: context.warnings,
    })
}
