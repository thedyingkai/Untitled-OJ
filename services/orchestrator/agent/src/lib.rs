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
    MigrationRun, ProviderRevisionRun, RuntimeContextRun, StoredCompletion,
};
pub use pipeline::{
    BuiltInPipelineProviderConfig, BuiltInReleasePipelineProvider, FrontendAssetStoreConfig,
    HttpReleasePipelineProvider, PipelineProviderConfig, PipelineProviderError,
    PipelineProviderMode, RedisConnectionConfig, ReleasePipelineProvider, StorageConnectionConfig,
    event_connection_urls_from_env,
};
pub use runtime_policy::{
    CredentialRefreshStatus, LocalRuntimeContextProvider, NodeRuntimeFactsPublisher,
    NodeRuntimeFactsV1, RuntimeContextProvider, RuntimePolicyError,
    WorkloadCredentialExchangeRequest, WorkloadCredentialExchanger, WorkloadCredentialSupervisor,
    recover_pending_runtime_contexts,
};
pub use transport::{
    AgentClaimRequest, AgentTransport, ArtifactFetcher, ClaimResponse, DownloadedArtifact,
    EnrollmentClient, HeartbeatAck, HttpArtifactFetcher, HttpMtlsTransport,
    HttpNodeRuntimeFactsPublisher, HttpWorkloadCredentialExchanger, LeasedJob,
    LoopbackHttpTransport, NodeCertificateBundle, TransportError,
};
pub use worker::{AgentWorker, PollOutcome, WorkerConfig, WorkerError};

pub(crate) fn now_ms() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};

    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(i64::MAX as u128) as i64
}
