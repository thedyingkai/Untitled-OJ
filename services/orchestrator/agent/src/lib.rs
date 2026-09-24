//! Pull-based node agent for the orchestrator control plane.
//!
//! Runtime mutations are executed through [`orchestrator_runtime::ContainerRuntime`]
//! only. The local SQLite ledger is the source of truth for replay decisions: a
//! completed job is reported again without re-execution, while a job interrupted
//! during a mutation is surfaced as `NEEDS_ATTENTION`.

mod executor;
mod identity;
mod ledger;
mod pipeline;
mod runtime_policy;
mod transport;
mod worker;

pub use executor::{ExecutionOutcome, JobExecutor};
pub use identity::{
    EnrollmentAttempt, EnrollmentSessionGuard, GeneratedCertificateRequest, IdentityError,
    IdentityStore, StoredNodeIdentity, generate_certificate_request,
    validate_enrollment_bundle_fresh,
};
pub use ledger::{
    AgentLedger, BeginDecision, JobRun, JobStep, LedgerError, LedgerRunState, MigrationDecision,
    MigrationRegistration, MigrationRun, ProviderRevisionRun, RuntimeContextRun, StoredCompletion,
};
pub use pipeline::{
    BuiltInPipelineProviderConfig, BuiltInReleasePipelineProvider, FrontendAssetStoreConfig,
    HttpReleasePipelineProvider, PipelineBootstrapConfig, PipelineProviderConfig,
    PipelineProviderError, PipelineProviderMode, RedisConnectionConfig, ReleasePipelineProvider,
    StorageConnectionConfig, event_connection_urls_from_env,
    pipeline_internal_state_roots_from_env,
};
pub use runtime_policy::{
    CredentialRefreshStatus, LocalRuntimeContextProvider, NodeRuntimeFactsPublisher,
    NodeRuntimeFactsV1, RuntimeContextProvider, RuntimePolicyError,
    WorkloadCredentialExchangeRequest, WorkloadCredentialExchanger, WorkloadCredentialSupervisor,
    recover_pending_runtime_contexts, validate_agent_workload_file_ownership,
    validate_isolated_workload_roots,
};
pub use transport::{
    AgentClaimRequest, AgentTransport, ArtifactFetcher, ClaimResponse, DownloadedArtifact,
    EnrollmentClient, HeartbeatAck, HttpArtifactFetcher, HttpMtlsTransport,
    HttpNodeRuntimeFactsPublisher, HttpWorkloadCredentialExchanger, LeasedJob,
    LoopbackHttpTransport, NodeCertificateBundle, TransportError,
};
pub use worker::{AgentWorker, PollOutcome, WorkerConfig, WorkerError};
pub mod resource_claim;

use orchestrator_protocol::{MigrationContainerInventoryV1, MigrationContainerObservationV1};
use orchestrator_runtime::ContainerRuntime;

const MAX_MIGRATION_RECONCILIATION_WARNINGS: usize = 64;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MigrationReconciliationV1 {
    pub inspected: usize,
    pub tombstoned: usize,
    pub removed: usize,
    pub safe_to_start_worker: bool,
    pub warnings: Vec<String>,
}

/// Reconciles the closed Docker migration inventory before the Agent can
/// claim work. Missing or corrupt inventory is fail-closed. A valid stopped
/// orphan is first tombstoned as `NEEDS_ATTENTION`, then only its container is
/// removed; this ordering permanently prevents blind migration replay.
pub async fn reconcile_migration_containers<R: ContainerRuntime>(
    ledger: &mut AgentLedger,
    runtime: &R,
    inventory: MigrationContainerInventoryV1,
) -> Result<MigrationReconciliationV1, LedgerError> {
    let mut result = MigrationReconciliationV1 {
        inspected: inventory.containers.len(),
        tombstoned: 0,
        removed: 0,
        safe_to_start_worker: inventory.inventory_complete,
        warnings: Vec::new(),
    };
    if !inventory.inventory_complete {
        push_migration_warning(
            &mut result,
            format!(
                "migration inventory incomplete; worker startup blocked: {}",
                bounded_warning(&inventory.inventory_error)
            ),
        );
    }
    for observation in inventory.containers {
        reconcile_migration_observation(ledger, runtime, &mut result, observation).await?;
    }
    Ok(result)
}

async fn reconcile_migration_observation<R: ContainerRuntime>(
    ledger: &mut AgentLedger,
    runtime: &R,
    result: &mut MigrationReconciliationV1,
    observation: MigrationContainerObservationV1,
) -> Result<(), LedgerError> {
    let Some(identity) = observation.identity.as_ref() else {
        result.safe_to_start_worker = false;
        push_migration_warning(
            result,
            format!(
                "migration container {} has invalid identity labels and was not removed: {}",
                bounded_warning(&observation.container_id),
                bounded_warning(&observation.validation_error)
            ),
        );
        return Ok(());
    };
    match ledger.migration_registration(identity)? {
        MigrationRegistration::Exact(run) => {
            push_migration_warning(
                result,
                format!(
                    "registered migration {}@{} ({}) remains {:?}; ledger state {} was preserved",
                    identity.service_name,
                    identity.version,
                    bounded_warning(&observation.container_id),
                    observation.observed_state,
                    run.state
                ),
            );
        }
        MigrationRegistration::Conflict(_) => {
            result.safe_to_start_worker = false;
            push_migration_warning(
                result,
                format!(
                    "migration container {} identity conflicts with the durable {}@{} record; it was not removed",
                    bounded_warning(&observation.container_id),
                    identity.service_name,
                    identity.version,
                ),
            );
        }
        MigrationRegistration::Missing => {
            let evidence = format!(
                "unregistered migration container observed after restart in {:?}; database outcome is unknown and automatic replay is forbidden",
                observation.observed_state
            );
            match ledger.tombstone_unregistered_migration(
                identity,
                &observation.container_id,
                &evidence,
                now_ms(),
            )? {
                MigrationRegistration::Exact(_) => result.tombstoned += 1,
                MigrationRegistration::Missing | MigrationRegistration::Conflict(_) => {
                    result.safe_to_start_worker = false;
                    push_migration_warning(
                        result,
                        format!(
                            "migration {}@{} could not be durably tombstoned; container {} was not removed",
                            identity.service_name,
                            identity.version,
                            bounded_warning(&observation.container_id)
                        ),
                    );
                    return Ok(());
                }
            }
            if !observation.observed_state.is_proven_inactive() {
                result.safe_to_start_worker = false;
                push_migration_warning(
                    result,
                    format!(
                        "unregistered migration {}@{} was tombstoned but container {} is {:?}; it was not removed",
                        identity.service_name,
                        identity.version,
                        bounded_warning(&observation.container_id),
                        observation.observed_state
                    ),
                );
                return Ok(());
            }
            if let Err(error) = runtime
                .remove_container(&observation.container_id, false)
                .await
            {
                result.safe_to_start_worker = false;
                push_migration_warning(
                    result,
                    format!(
                        "migration orphan {} was tombstoned but container cleanup failed: {}",
                        bounded_warning(&observation.container_id),
                        bounded_warning(&error.to_string())
                    ),
                );
            } else {
                result.removed += 1;
                push_migration_warning(
                    result,
                    format!(
                        "migration orphan {} was tombstoned as NEEDS_ATTENTION and its inactive container was removed",
                        bounded_warning(&observation.container_id)
                    ),
                );
            }
        }
    }
    Ok(())
}

fn push_migration_warning(result: &mut MigrationReconciliationV1, warning: String) {
    if result.warnings.len() < MAX_MIGRATION_RECONCILIATION_WARNINGS {
        result.warnings.push(bounded_warning(&warning));
    }
}

fn bounded_warning(value: &str) -> String {
    value
        .chars()
        .filter(|character| !character.is_control())
        .take(512)
        .collect()
}

pub(crate) fn now_ms() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};

    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(i64::MAX as u128) as i64
}
