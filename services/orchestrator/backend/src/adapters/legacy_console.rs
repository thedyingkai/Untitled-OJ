//! Explicit 0.2 host/route compatibility over the same live repository.
use crate::registry::RegistryContext;
use orchestrator_core::Result;
use orchestrator_legacy::OrchestratorActionConsole;

pub(crate) fn from_legacy_console(console: OrchestratorActionConsole) -> Result<RegistryContext> {
    console.into_registry().map(RegistryContext::from_bootstrap)
}

pub(crate) fn legacy_console(registry: &RegistryContext) -> OrchestratorActionConsole {
    OrchestratorActionConsole::from_registry(registry.bootstrap_snapshot())
}
