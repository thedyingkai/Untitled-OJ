//! Background binding health responsibilities.
use crate::durable::DurableStore;
use crate::topology_worker::context::now_ms;
use orchestrator_core::ApiBinding;
use orchestrator_runtime::RuntimeDesiredState;
use orchestrator_runtime::RuntimeObservedState;
use orchestrator_storage::RuntimeManagementMode;
use std::collections::BTreeSet;

pub(super) fn topology_binding_consumers_healthy(
    storage: &DurableStore,
    bindings: &[ApiBinding],
) -> Result<(), String> {
    let deployments = bindings
        .iter()
        .filter(|binding| binding.desired_state == "ACTIVE")
        .map(|binding| binding.consumer_deployment_id.as_str())
        .collect::<BTreeSet<_>>();
    topology_binding_role_healthy(storage, deployments, "consumer")
}

pub(super) fn topology_binding_providers_healthy(
    storage: &DurableStore,
    bindings: &[ApiBinding],
) -> Result<(), String> {
    let deployments = bindings
        .iter()
        .filter(|binding| binding.desired_state == "ACTIVE")
        .map(|binding| binding.provider_deployment_id.as_str())
        .collect::<BTreeSet<_>>();
    topology_binding_role_healthy(storage, deployments, "provider")
}

pub(super) fn topology_binding_role_healthy(
    storage: &DurableStore,
    deployments: BTreeSet<&str>,
    role: &str,
) -> Result<(), String> {
    if deployments.is_empty() {
        return Ok(());
    }
    let runtimes = storage
        .runtime_instances(None)
        .map_err(|error| error.to_string())?;
    let evidence_at_ms = now_ms();
    for deployment_id in deployments {
        let matching = runtimes
            .iter()
            .filter(|runtime| runtime.instance.deployment_id == deployment_id)
            .collect::<Vec<_>>();
        let [runtime] = matching.as_slice() else {
            return Err(format!(
                "{role} deployment {deployment_id} no longer has one exact runtime projection"
            ));
        };
        let runtime = storage
            .runtime_with_current_evidence((*runtime).clone(), evidence_at_ms)
            .map_err(|error| error.to_string())?;
        if runtime.instance.desired_state != RuntimeDesiredState::Running
            || runtime.instance.observed_state != RuntimeObservedState::Running
            || !runtime.instance.health.eq_ignore_ascii_case("HEALTHY")
            || !runtime.drift_reason.trim().is_empty()
            || (runtime.management_mode == RuntimeManagementMode::Managed
                && !runtime.instance.runtime_attested)
        {
            let evidence = if runtime.drift_reason.trim().is_empty() {
                "runtime is not desired Running, observed Running/Healthy".to_string()
            } else {
                runtime.drift_reason
            };
            return Err(format!(
                "{role} deployment {deployment_id} is unavailable: {evidence}"
            ));
        }
    }
    Ok(())
}
