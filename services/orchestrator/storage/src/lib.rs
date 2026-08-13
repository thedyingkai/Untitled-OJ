//! Durable database adapters for the orchestrator domain and control plane.
//!
//! Database drivers, migrations and process ownership are kept out of
//! `orchestrator-core`. SQLite writes use short transactions and never rebuild
//! a shadow in-memory store after a mutation.

mod api_bindings;
mod audit;
mod contribution;
mod idempotency;
mod jobs;
mod legacy_import;
mod node_identity;
mod operations;
mod postgres;
mod postgres_api_bindings;
mod postgres_audit;
mod postgres_contribution;
mod postgres_idempotency;
mod postgres_jobs;
mod postgres_operations;
mod postgres_runtime_facts;
mod postgres_runtime_instances;
mod postgres_store;
mod postgres_topology;
mod retention;
mod runtime_facts;
mod runtime_instances;
mod sqlite;
mod sqlite_contribution;
mod topology;

pub use audit::{AuditOutcome, AuditRecord, NewAuditRecord};
pub use contribution::{
    ContributionRepository, ContributionRepositoryError, ContributionRepositoryResult,
    MemoryContributionStore,
};
pub use idempotency::{IdempotencyBegin, StoredIdempotentResponse};
pub use jobs::{JobMetricsSnapshot, SqliteJobStore};
pub use legacy_import::LegacyImportReport;
pub use node_identity::{
    CERTIFICATE_LIFETIME_MS, CERTIFICATE_RENEWAL_WINDOW_MS, CertificateActivation,
    CertificateRotation, EnrollmentLookup, EnrollmentRedemption, MAX_REMOTE_NODES,
    NewNodeCertificate, NodeCertificateRecord, NodeEnrollmentCode, classify_enrollment_replay,
};
pub use operations::{ControlPlaneAnomalyCounters, SqliteOperationStore};
pub use orchestrator_legacy::{
    ApiBinding, ApiBindingDesiredState, ApiBindingHealth, ApiBindingObservedState, ApiBindingState,
};
pub use postgres::{
    AdvisoryLockGuard, DEFAULT_CONTROL_PLANE_LOCK_KEY, PooledPostgresConnection, PostgresError,
    PostgresOptions, PostgresPool, PostgresPreflight, PostgresReadinessReport, PostgresResult,
    PostgresTlsTrust,
};
pub use postgres_jobs::PostgresJobStore;
pub use postgres_operations::PostgresOperationStore;
pub use postgres_store::PostgresOrchestratorStore;
pub use retention::HistoryRetentionReport;
pub use runtime_facts::StoredNodeRuntimeFacts;
pub use runtime_instances::{RuntimeManagementMode, StoredRuntimeInstance};
pub use sqlite::{
    AppliedMigration, ReadinessReport, SqliteOptions, SqliteOrchestratorStore, StorageError,
    StorageResult, SynchronousMode,
};
pub use topology::{TopologyApplyGroupMember, TopologyApplyOutcome, TopologyHeads};
