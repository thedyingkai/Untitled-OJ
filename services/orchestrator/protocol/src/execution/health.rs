//! Deterministic health contracts; no environment reads or I/O.

use crate::{HealthGatePolicy, MissingHealthcheckPolicy, RuntimeInstance, RuntimeObservedState};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HealthGateDecision {
    Ready,
    Pending(String),
    Failed(String),
}

/// Evaluates one Docker inspection result without performing I/O. `NONE`
/// explicitly means Docker reported no healthcheck; `UNKNOWN` remains
/// unproven and is allowed to wait until the bounded deadline.
pub fn evaluate_health_gate(
    instance: &RuntimeInstance,
    policy: &HealthGatePolicy,
) -> HealthGateDecision {
    if instance.observed_state != RuntimeObservedState::Running {
        return match instance.observed_state {
            RuntimeObservedState::Exited
            | RuntimeObservedState::Stopped
            | RuntimeObservedState::Missing => HealthGateDecision::Failed(format!(
                "container is {:?}, not RUNNING",
                instance.observed_state
            )),
            RuntimeObservedState::Created | RuntimeObservedState::Unknown => {
                HealthGateDecision::Pending(format!(
                    "container is {:?}, waiting for RUNNING",
                    instance.observed_state
                ))
            }
            RuntimeObservedState::Running => unreachable!("RUNNING was handled above"),
        };
    }

    match instance.health.trim().to_ascii_uppercase().as_str() {
        "HEALTHY" => HealthGateDecision::Ready,
        "STARTING" | "UNKNOWN" | "" => HealthGateDecision::Pending(format!(
            "container health is {}",
            normalized_health(&instance.health)
        )),
        "NONE" if policy.missing_healthcheck == MissingHealthcheckPolicy::AllowRunning => {
            HealthGateDecision::Ready
        }
        "NONE" => HealthGateDecision::Failed(
            "image has no Docker HEALTHCHECK and policy requires one".to_string(),
        ),
        "UNHEALTHY" => HealthGateDecision::Failed("Docker health status is UNHEALTHY".to_string()),
        other => HealthGateDecision::Pending(format!(
            "Docker returned unrecognized health status {other:?}"
        )),
    }
}

fn normalized_health(value: &str) -> &str {
    if value.trim().is_empty() {
        "UNKNOWN"
    } else {
        value
    }
}
