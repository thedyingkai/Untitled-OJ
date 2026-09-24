//! Pure translation from runtime observations to topology presentation states.
use orchestrator_core::{
    TopologyDesiredDeploymentState, TopologyHealth, TopologyObservedDeploymentState,
};
use orchestrator_protocol::{RuntimeDesiredState, RuntimeObservedState};

pub(super) fn desired_deployment_state(
    state: &RuntimeDesiredState,
) -> TopologyDesiredDeploymentState {
    match state {
        RuntimeDesiredState::Running => TopologyDesiredDeploymentState::Running,
        RuntimeDesiredState::Stopped => TopologyDesiredDeploymentState::Stopped,
        RuntimeDesiredState::Removed => TopologyDesiredDeploymentState::Absent,
    }
}

pub(super) fn observed_deployment_state(
    state: &RuntimeObservedState,
) -> TopologyObservedDeploymentState {
    match state {
        RuntimeObservedState::Created => TopologyObservedDeploymentState::Pending,
        RuntimeObservedState::Running => TopologyObservedDeploymentState::Running,
        RuntimeObservedState::Stopped => TopologyObservedDeploymentState::Stopped,
        RuntimeObservedState::Exited => TopologyObservedDeploymentState::Failed,
        RuntimeObservedState::Missing | RuntimeObservedState::Unknown => {
            TopologyObservedDeploymentState::Unknown
        }
    }
}

pub(super) fn runtime_health(value: &str) -> TopologyHealth {
    if value.eq_ignore_ascii_case("healthy") {
        TopologyHealth::Healthy
    } else if value.eq_ignore_ascii_case("unhealthy") {
        TopologyHealth::Unhealthy
    } else {
        TopologyHealth::Unknown
    }
}

pub(super) fn runtime_states_match(
    desired: &RuntimeDesiredState,
    observed: &RuntimeObservedState,
) -> bool {
    matches!(
        (desired, observed),
        (RuntimeDesiredState::Running, RuntimeObservedState::Running)
            | (RuntimeDesiredState::Stopped, RuntimeObservedState::Stopped)
            | (RuntimeDesiredState::Removed, RuntimeObservedState::Missing)
    )
}
