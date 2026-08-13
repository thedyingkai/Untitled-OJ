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

use orchestrator_runtime::{
    ContainerRuntime, MigrationContainerInventoryV1, MigrationContainerObservationV1,
};

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

#[cfg(test)]
mod migration_reconciliation_tests {
    use super::*;
    use async_trait::async_trait;
    use orchestrator_runtime::{
        ContainerSpec, MigrationContainerIdentityV1, MigrationContainerStateV1, OciImageReference,
        RuntimeError, RuntimeInstance,
    };
    use std::sync::Mutex;

    #[derive(Default)]
    struct CleanupRuntime {
        removed: Mutex<Vec<(String, bool)>>,
    }

    #[async_trait]
    impl ContainerRuntime for CleanupRuntime {
        async fn pull_image(&self, _: &OciImageReference) -> Result<(), RuntimeError> {
            unreachable!()
        }
        async fn create_container(
            &self,
            _: &ContainerSpec,
        ) -> Result<RuntimeInstance, RuntimeError> {
            unreachable!()
        }
        async fn start_container(&self, _: &str) -> Result<(), RuntimeError> {
            unreachable!()
        }
        async fn stop_container(&self, _: &str, _: i32) -> Result<(), RuntimeError> {
            unreachable!()
        }
        async fn restart_container(&self, _: &str, _: i32) -> Result<(), RuntimeError> {
            unreachable!()
        }
        async fn remove_container(&self, id: &str, force: bool) -> Result<(), RuntimeError> {
            self.removed.lock().unwrap().push((id.to_string(), force));
            Ok(())
        }
        async fn inspect_container(&self, _: &str) -> Result<RuntimeInstance, RuntimeError> {
            unreachable!()
        }
    }

    fn identity() -> MigrationContainerIdentityV1 {
        let image =
            OciImageReference::parse(&format!("ghcr.io/ojos/migrate@sha256:{}", "b".repeat(64)))
                .unwrap();
        let claims =
            orchestrator_runtime::migration_resource_claims_sha256(&["database".to_string()])
                .unwrap();
        let digest = orchestrator_runtime::migration_identity_sha256(
            "contest-service",
            "0001",
            &format!("sha256:{}", "a".repeat(64)),
            &image,
            &claims,
        )
        .unwrap();
        MigrationContainerIdentityV1 {
            job_id: "job-migrate-1".to_string(),
            service_name: "contest-service".to_string(),
            version: "0001".to_string(),
            checksum: format!("sha256:{}", "a".repeat(64)),
            image: image.to_string(),
            resource_claims_sha256: claims,
            identity_sha256: digest,
        }
    }

    fn inventory(
        identity: Option<MigrationContainerIdentityV1>,
        state: MigrationContainerStateV1,
    ) -> MigrationContainerInventoryV1 {
        MigrationContainerInventoryV1 {
            inventory_complete: true,
            inventory_error: String::new(),
            containers: vec![MigrationContainerObservationV1 {
                container_id: "container-orphan-1".to_string(),
                observed_state: state,
                identity,
                validation_error: "invalid labels".to_string(),
            }],
        }
    }

    #[tokio::test]
    async fn stopped_unregistered_orphan_is_tombstoned_before_cleanup_and_never_replays() {
        let mut ledger = AgentLedger::open_in_memory().unwrap();
        let runtime = CleanupRuntime::default();
        let identity = identity();
        let result = reconcile_migration_containers(
            &mut ledger,
            &runtime,
            inventory(Some(identity.clone()), MigrationContainerStateV1::Exited),
        )
        .await
        .unwrap();
        assert!(result.safe_to_start_worker);
        assert_eq!((result.tombstoned, result.removed), (1, 1));
        assert_eq!(
            runtime.removed.lock().unwrap().as_slice(),
            &[("container-orphan-1".to_string(), false)]
        );
        let run = ledger
            .migration(&identity.service_name, &identity.version)
            .unwrap()
            .unwrap();
        assert_eq!(run.state, "NEEDS_ATTENTION");
        assert!(matches!(
            ledger.begin_migration(
                &identity.service_name,
                &identity.version,
                &identity.checksum,
                &identity.image,
                &identity.resource_claims_sha256,
                &identity.identity_sha256,
                "job-after-restart",
                now_ms(),
            ),
            Err(LedgerError::MigrationNeedsAttention { .. })
        ));
    }

    #[tokio::test]
    async fn running_orphan_is_tombstoned_but_never_removed() {
        let mut ledger = AgentLedger::open_in_memory().unwrap();
        let runtime = CleanupRuntime::default();
        let result = reconcile_migration_containers(
            &mut ledger,
            &runtime,
            inventory(Some(identity()), MigrationContainerStateV1::Running),
        )
        .await
        .unwrap();
        assert!(!result.safe_to_start_worker);
        assert_eq!((result.tombstoned, result.removed), (1, 0));
        assert!(runtime.removed.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn bad_labels_are_not_removed_and_block_worker_start() {
        let mut ledger = AgentLedger::open_in_memory().unwrap();
        let runtime = CleanupRuntime::default();
        let result = reconcile_migration_containers(
            &mut ledger,
            &runtime,
            inventory(None, MigrationContainerStateV1::Exited),
        )
        .await
        .unwrap();
        assert!(!result.safe_to_start_worker);
        assert_eq!((result.tombstoned, result.removed), (0, 0));
        assert!(runtime.removed.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn reconciliation_restart_is_idempotent_and_preserves_registered_tombstone() {
        let mut ledger = AgentLedger::open_in_memory().unwrap();
        let runtime = CleanupRuntime::default();
        let identity = identity();
        let first = reconcile_migration_containers(
            &mut ledger,
            &runtime,
            inventory(Some(identity.clone()), MigrationContainerStateV1::Exited),
        )
        .await
        .unwrap();
        assert_eq!(first.removed, 1);
        let second = reconcile_migration_containers(
            &mut ledger,
            &runtime,
            inventory(Some(identity), MigrationContainerStateV1::Exited),
        )
        .await
        .unwrap();
        assert_eq!((second.tombstoned, second.removed), (0, 0));
        assert_eq!(runtime.removed.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn registered_needs_attention_is_never_removed() {
        let mut ledger = AgentLedger::open_in_memory().unwrap();
        let runtime = CleanupRuntime::default();
        let identity = identity();
        assert_eq!(
            ledger
                .begin_migration(
                    &identity.service_name,
                    &identity.version,
                    &identity.checksum,
                    &identity.image,
                    &identity.resource_claims_sha256,
                    &identity.identity_sha256,
                    &identity.job_id,
                    1,
                )
                .unwrap(),
            MigrationDecision::Execute
        );
        ledger
            .mark_migration_needs_attention(
                &identity.service_name,
                &identity.version,
                &identity.job_id,
                "lost wait response",
                2,
            )
            .unwrap();
        let result = reconcile_migration_containers(
            &mut ledger,
            &runtime,
            inventory(Some(identity), MigrationContainerStateV1::Exited),
        )
        .await
        .unwrap();
        assert_eq!((result.tombstoned, result.removed), (0, 0));
        assert!(runtime.removed.lock().unwrap().is_empty());
    }
}
