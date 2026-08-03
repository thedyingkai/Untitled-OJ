use fs2::FileExt;
use orchestrator_legacy::{
    DeployedServiceApi, DiagnosticReport, Endpoint, HostService, Link, LogView, NodeRecord,
    Operation, OperationLock, OperationLogRecord, OperationStatus, OrchestratorError,
    OrchestratorStore, RenderedServiceConfig, ServiceApiSurface, ServiceFrontendEntry,
    ServiceManifest, ServiceMigrationRecord, ServicePermissionRecord, ServiceRedisResource,
    ServiceRelease, ServiceRoute, ServiceStorageResource, Topology, TopologySnapshot,
    build_topology, validate_deployed_service_api, validate_endpoint, validate_endpoint_id,
    validate_host_service, validate_link, validate_log_view, validate_node_record,
    validate_rendered_service_config, validate_service_api_surface,
    validate_service_frontend_entry, validate_service_manifest, validate_service_migration_record,
    validate_service_permission_record, validate_service_redis_resource,
    validate_service_release_record, validate_service_route, validate_service_storage_resource,
    validate_topology,
};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::{Serialize, de::DeserializeOwned};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{File, OpenOptions},
    io::{Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};
use thiserror::Error;

const INITIAL_SCHEMA: &str = r#"
CREATE TABLE orchestrator_records (
    kind TEXT NOT NULL,
    record_key TEXT NOT NULL,
    scope TEXT NOT NULL DEFAULT '',
    payload TEXT NOT NULL CHECK (json_valid(payload)),
    created_at INTEGER NOT NULL DEFAULT (unixepoch()),
    updated_at INTEGER NOT NULL DEFAULT (unixepoch()),
    PRIMARY KEY (kind, record_key)
);
CREATE INDEX idx_orchestrator_records_kind_scope
    ON orchestrator_records(kind, scope, record_key);

CREATE TABLE orchestrator_operation_logs_v2 (
    sequence INTEGER PRIMARY KEY AUTOINCREMENT,
    operation_id TEXT NOT NULL,
    payload TEXT NOT NULL CHECK (json_valid(payload)),
    created_at INTEGER NOT NULL DEFAULT (unixepoch())
);
CREATE INDEX idx_orchestrator_operation_logs_v2_operation
    ON orchestrator_operation_logs_v2(operation_id, sequence);

CREATE TABLE orchestrator_state (
    namespace TEXT NOT NULL,
    state_key TEXT NOT NULL,
    payload TEXT NOT NULL CHECK (json_valid(payload)),
    created_at INTEGER NOT NULL DEFAULT (unixepoch()),
    updated_at INTEGER NOT NULL DEFAULT (unixepoch()),
    PRIMARY KEY (namespace, state_key)
);

CREATE TABLE orchestrator_jobs (
    job_id TEXT PRIMARY KEY,
    operation_id TEXT NOT NULL,
    node_id TEXT NOT NULL,
    idempotency_key TEXT NOT NULL,
    payload_sha256 TEXT NOT NULL,
    status TEXT NOT NULL,
    available_at_ms INTEGER NOT NULL,
    created_at_ms INTEGER NOT NULL,
    payload TEXT NOT NULL CHECK (json_valid(payload)),
    UNIQUE(node_id, idempotency_key)
);
CREATE INDEX idx_orchestrator_jobs_claim
    ON orchestrator_jobs(node_id, status, available_at_ms, created_at_ms, job_id);
CREATE INDEX idx_orchestrator_jobs_operation
    ON orchestrator_jobs(operation_id, created_at_ms, job_id);

CREATE TABLE orchestrator_job_events (
    job_id TEXT NOT NULL,
    sequence INTEGER NOT NULL,
    payload TEXT NOT NULL CHECK (json_valid(payload)),
    created_at_ms INTEGER NOT NULL,
    PRIMARY KEY(job_id, sequence),
    FOREIGN KEY(job_id) REFERENCES orchestrator_jobs(job_id) ON DELETE CASCADE
);
"#;

const TOPOLOGY_REVISION_SCHEMA: &str = r#"
CREATE TABLE orchestrator_topology_revisions (
    topology_id TEXT NOT NULL,
    revision_number INTEGER NOT NULL CHECK (revision_number > 0),
    revision_id TEXT NOT NULL UNIQUE,
    parent_revision_id TEXT,
    rollback_of_revision_id TEXT,
    content_sha256 TEXT NOT NULL,
    payload TEXT NOT NULL CHECK (json_valid(payload)),
    created_at INTEGER NOT NULL DEFAULT (unixepoch()),
    PRIMARY KEY (topology_id, revision_number),
    FOREIGN KEY (parent_revision_id) REFERENCES orchestrator_topology_revisions(revision_id),
    FOREIGN KEY (rollback_of_revision_id) REFERENCES orchestrator_topology_revisions(revision_id)
);
CREATE INDEX idx_orchestrator_topology_revisions_id
    ON orchestrator_topology_revisions(topology_id, revision_id);

CREATE TABLE orchestrator_topology_heads (
    topology_id TEXT PRIMARY KEY,
    draft_revision_id TEXT NOT NULL,
    applied_revision_id TEXT,
    applying_revision_id TEXT,
    applying_operation_id TEXT,
    last_operation_id TEXT,
    updated_at INTEGER NOT NULL DEFAULT (unixepoch()),
    FOREIGN KEY (draft_revision_id) REFERENCES orchestrator_topology_revisions(revision_id),
    FOREIGN KEY (applied_revision_id) REFERENCES orchestrator_topology_revisions(revision_id),
    FOREIGN KEY (applying_revision_id) REFERENCES orchestrator_topology_revisions(revision_id),
    CHECK ((applying_revision_id IS NULL) = (applying_operation_id IS NULL))
);

CREATE TABLE orchestrator_topology_status (
    topology_id TEXT PRIMARY KEY,
    desired_revision_id TEXT,
    observed_revision_id TEXT,
    payload TEXT NOT NULL CHECK (json_valid(payload)),
    updated_at INTEGER NOT NULL DEFAULT (unixepoch()),
    FOREIGN KEY (desired_revision_id) REFERENCES orchestrator_topology_revisions(revision_id),
    FOREIGN KEY (observed_revision_id) REFERENCES orchestrator_topology_revisions(revision_id)
);

CREATE TABLE orchestrator_runtime_instances (
    deployment_id TEXT PRIMARY KEY,
    node_id TEXT NOT NULL,
    service_id TEXT NOT NULL,
    desired_state TEXT NOT NULL,
    observed_state TEXT NOT NULL,
    payload TEXT NOT NULL CHECK (json_valid(payload)),
    created_at INTEGER NOT NULL DEFAULT (unixepoch()),
    updated_at INTEGER NOT NULL DEFAULT (unixepoch())
);
CREATE INDEX idx_orchestrator_runtime_instances_node
    ON orchestrator_runtime_instances(node_id, service_id, deployment_id);
"#;

const IDEMPOTENCY_SCHEMA: &str = r#"
CREATE TABLE orchestrator_idempotency (
    scope TEXT NOT NULL,
    idempotency_key TEXT NOT NULL,
    request_sha256 TEXT NOT NULL,
    state TEXT NOT NULL CHECK (state IN ('STARTED', 'COMPLETED')),
    response_status INTEGER,
    response_content_type TEXT,
    response_headers TEXT CHECK (response_headers IS NULL OR json_valid(response_headers)),
    response_body TEXT CHECK (response_body IS NULL OR json_valid(response_body)),
    started_at_ms INTEGER NOT NULL,
    completed_at_ms INTEGER,
    expires_at_ms INTEGER NOT NULL,
    PRIMARY KEY (scope, idempotency_key)
);
CREATE INDEX idx_orchestrator_idempotency_expiry
    ON orchestrator_idempotency(expires_at_ms);
"#;

const DURABLE_OPERATION_SCHEMA: &str = r#"
CREATE TABLE orchestrator_durable_operations (
    operation_id TEXT PRIMARY KEY,
    revision INTEGER NOT NULL CHECK (revision > 0),
    status TEXT NOT NULL,
    payload TEXT NOT NULL CHECK (json_valid(payload)),
    created_at_ms INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL
);
CREATE INDEX idx_orchestrator_durable_operations_recovery
    ON orchestrator_durable_operations(status, updated_at_ms, operation_id);
"#;

const APPEND_ONLY_AUDIT_SCHEMA: &str = r#"
CREATE TABLE orchestrator_audit_log (
    sequence INTEGER PRIMARY KEY AUTOINCREMENT,
    request_id TEXT NOT NULL,
    actor TEXT NOT NULL,
    action TEXT NOT NULL,
    resource TEXT NOT NULL,
    idempotency_key TEXT NOT NULL,
    request_digest TEXT NOT NULL,
    outcome TEXT NOT NULL CHECK (outcome IN ('INTENT', 'SUCCEEDED', 'REJECTED')),
    response_status INTEGER CHECK (response_status BETWEEN 100 AND 599),
    operation_id TEXT,
    timestamp_ms INTEGER NOT NULL CHECK (timestamp_ms >= 0),
    CHECK ((outcome = 'INTENT' AND response_status IS NULL) OR
           (outcome <> 'INTENT' AND response_status IS NOT NULL))
);
CREATE INDEX idx_orchestrator_audit_log_request
    ON orchestrator_audit_log(request_id, sequence);
CREATE INDEX idx_orchestrator_audit_log_operation
    ON orchestrator_audit_log(operation_id, sequence)
    WHERE operation_id IS NOT NULL;

CREATE TRIGGER orchestrator_audit_log_no_update
BEFORE UPDATE ON orchestrator_audit_log
BEGIN
    SELECT RAISE(ABORT, 'orchestrator audit log is append-only');
END;

CREATE TRIGGER orchestrator_audit_log_no_delete
BEFORE DELETE ON orchestrator_audit_log
BEGIN
    SELECT RAISE(ABORT, 'orchestrator audit log is append-only');
END;
"#;

const NODE_IDENTITY_SCHEMA: &str = r#"
CREATE TABLE orchestrator_node_enrollment_codes (
    code_id TEXT PRIMARY KEY,
    secret_sha256 TEXT NOT NULL UNIQUE,
    node_id TEXT NOT NULL,
    created_at_ms INTEGER NOT NULL CHECK (created_at_ms >= 0),
    expires_at_ms INTEGER NOT NULL CHECK (expires_at_ms > created_at_ms),
    redeemed_at_ms INTEGER CHECK (redeemed_at_ms IS NULL OR redeemed_at_ms >= created_at_ms)
);
CREATE INDEX idx_orchestrator_node_enrollment_expiry
    ON orchestrator_node_enrollment_codes(expires_at_ms, code_id);

CREATE TABLE orchestrator_node_certificates (
    serial_hex TEXT PRIMARY KEY,
    node_id TEXT NOT NULL,
    spiffe_id TEXT NOT NULL,
    certificate_pem TEXT NOT NULL,
    fingerprint_sha256 TEXT NOT NULL,
    issued_at_ms INTEGER NOT NULL CHECK (issued_at_ms >= 0),
    not_before_ms INTEGER NOT NULL,
    not_after_ms INTEGER NOT NULL CHECK (not_after_ms > not_before_ms),
    revoked_at_ms INTEGER,
    revoke_reason TEXT,
    replaced_by_serial TEXT,
    CHECK ((revoked_at_ms IS NULL) = (revoke_reason IS NULL)),
    FOREIGN KEY (replaced_by_serial) REFERENCES orchestrator_node_certificates(serial_hex)
);
CREATE INDEX idx_orchestrator_node_certificates_node
    ON orchestrator_node_certificates(node_id, not_after_ms, serial_hex);
CREATE UNIQUE INDEX idx_orchestrator_node_certificates_spiffe_serial
    ON orchestrator_node_certificates(spiffe_id, serial_hex);
"#;

const LEGACY_IMPORT_SCHEMA: &str = r#"
CREATE TABLE orchestrator_legacy_imports (
    import_id TEXT PRIMARY KEY,
    source_checksum TEXT NOT NULL,
    report TEXT NOT NULL CHECK (json_valid(report)),
    imported_at INTEGER NOT NULL DEFAULT (unixepoch())
);
"#;

const ENROLLMENT_REPLAY_SCHEMA: &str = r#"
ALTER TABLE orchestrator_node_enrollment_codes
    ADD COLUMN redeemed_csr_sha256 TEXT;
ALTER TABLE orchestrator_node_enrollment_codes
    ADD COLUMN issued_certificate_serial TEXT
    REFERENCES orchestrator_node_certificates(serial_hex);
CREATE INDEX idx_orchestrator_node_enrollment_certificate
    ON orchestrator_node_enrollment_codes(issued_certificate_serial);
"#;

const CONTROL_PLANE_EVIDENCE_SCHEMA: &str = r#"
ALTER TABLE orchestrator_jobs ADD COLUMN lease_expires_at_ms INTEGER;
ALTER TABLE orchestrator_jobs ADD COLUMN updated_at_ms INTEGER NOT NULL DEFAULT 0;
UPDATE orchestrator_jobs
SET lease_expires_at_ms = CAST(json_extract(payload, '$.lease_expires_at_ms') AS INTEGER),
    updated_at_ms = COALESCE(CAST(json_extract(payload, '$.updated_at_ms') AS INTEGER), 0);
CREATE INDEX idx_orchestrator_jobs_lease_recovery
    ON orchestrator_jobs(status, lease_expires_at_ms, job_id)
    WHERE status IN ('LEASED', 'CANCEL_REQUESTED') AND lease_expires_at_ms IS NOT NULL;

CREATE TABLE orchestrator_job_status_counts (
    status TEXT PRIMARY KEY,
    job_count INTEGER NOT NULL CHECK (job_count >= 0)
);
INSERT INTO orchestrator_job_status_counts(status, job_count) VALUES
    ('QUEUED', 0), ('LEASED', 0), ('RETRY_WAIT', 0), ('CANCEL_REQUESTED', 0),
    ('SUCCEEDED', 0), ('FAILED', 0), ('CANCELLED', 0), ('NEEDS_ATTENTION', 0);
UPDATE orchestrator_job_status_counts
SET job_count = (SELECT COUNT(*) FROM orchestrator_jobs WHERE orchestrator_jobs.status = orchestrator_job_status_counts.status);

CREATE TABLE orchestrator_control_plane_anomaly_counters (
    counter_key TEXT PRIMARY KEY,
    counter_value INTEGER NOT NULL CHECK (counter_value >= 0)
);
INSERT INTO orchestrator_control_plane_anomaly_counters(counter_key, counter_value) VALUES
    ('expired_job_lease_transitions', 0),
    ('operation_over_300_seconds_transitions', 0);

CREATE TABLE orchestrator_active_expired_lease_anomalies (
    job_id TEXT PRIMARY KEY,
    lease_identity TEXT NOT NULL,
    FOREIGN KEY(job_id) REFERENCES orchestrator_jobs(job_id)
        ON DELETE CASCADE
);

CREATE TABLE orchestrator_active_operation_anomalies (
    episode_id TEXT PRIMARY KEY,
    operation_id TEXT NOT NULL,
    generation INTEGER NOT NULL CHECK (generation >= 0),
    started_at_ms INTEGER,
    FOREIGN KEY(operation_id) REFERENCES orchestrator_durable_operations(operation_id)
        ON DELETE CASCADE
);
CREATE INDEX idx_orchestrator_active_operation_anomalies_operation
    ON orchestrator_active_operation_anomalies(operation_id, generation);
"#;

const MIGRATIONS: &[Migration] = &[
    Migration {
        version: 1,
        name: "initial-durable-storage",
        sql: INITIAL_SCHEMA,
    },
    Migration {
        version: 2,
        name: "topology-revisions-and-runtime-instances",
        sql: TOPOLOGY_REVISION_SCHEMA,
    },
    Migration {
        version: 3,
        name: "http-idempotency-ledger",
        sql: IDEMPOTENCY_SCHEMA,
    },
    Migration {
        version: 4,
        name: "durable-operation-coordinator",
        sql: DURABLE_OPERATION_SCHEMA,
    },
    Migration {
        version: 5,
        name: "append-only-audit-ledger",
        sql: APPEND_ONLY_AUDIT_SCHEMA,
    },
    Migration {
        version: 6,
        name: "node-enrollment-and-certificate-ledger",
        sql: NODE_IDENTITY_SCHEMA,
    },
    Migration {
        version: 7,
        name: "legacy-v0.2-import-ledger",
        sql: LEGACY_IMPORT_SCHEMA,
    },
    Migration {
        version: 8,
        name: "idempotent-node-enrollment-replay",
        sql: ENROLLMENT_REPLAY_SCHEMA,
    },
    Migration {
        version: 9,
        name: "control-plane-anomaly-and-lease-evidence",
        sql: CONTROL_PLANE_EVIDENCE_SCHEMA,
    },
];

const REQUIRED_TABLES: &[&str] = &[
    "orchestrator_schema_migrations",
    "orchestrator_records",
    "orchestrator_operation_logs_v2",
    "orchestrator_state",
    "orchestrator_jobs",
    "orchestrator_job_events",
    "orchestrator_topology_revisions",
    "orchestrator_topology_heads",
    "orchestrator_topology_status",
    "orchestrator_runtime_instances",
    "orchestrator_idempotency",
    "orchestrator_durable_operations",
    "orchestrator_audit_log",
    "orchestrator_node_enrollment_codes",
    "orchestrator_node_certificates",
    "orchestrator_legacy_imports",
    "orchestrator_control_plane_anomaly_counters",
    "orchestrator_active_expired_lease_anomalies",
    "orchestrator_active_operation_anomalies",
    "orchestrator_job_status_counts",
];

const REQUIRED_INDEXES: &[&str] = &[
    "idx_orchestrator_jobs_lease_recovery",
    "idx_orchestrator_active_operation_anomalies_operation",
];

const REQUIRED_TRIGGERS: &[&str] = &[
    "orchestrator_audit_log_no_update",
    "orchestrator_audit_log_no_delete",
];

pub(crate) const SERVICES: &str = "services";
pub(crate) const HOST_SERVICES: &str = "host-services";
pub(crate) const RELEASES: &str = "service-releases";
pub(crate) const ROUTES: &str = "service-routes";
pub(crate) const MIGRATION_RECORDS: &str = "service-migration-records";
pub(crate) const PERMISSION_RECORDS: &str = "service-permission-records";
pub(crate) const FRONTENDS: &str = "service-frontends";
pub(crate) const REDIS_RESOURCES: &str = "service-redis-resources";
pub(crate) const STORAGE_RESOURCES: &str = "service-storage-resources";
pub(crate) const RENDERED_CONFIGS: &str = "rendered-service-configs";
pub(crate) const NODES: &str = "nodes";
pub(crate) const API_SURFACES: &str = "service-api-surfaces";
pub(crate) const DEPLOYED_APIS: &str = "deployed-service-apis";
pub(crate) const ENDPOINTS: &str = "endpoints";
pub(crate) const LINKS: &str = "links";
pub(crate) const OPERATIONS: &str = "operations";
pub(crate) const OPERATION_LOCKS: &str = "operation-locks";
pub(crate) const TOPOLOGY_SNAPSHOTS: &str = "topology-snapshots";
pub(crate) const LOG_SOURCES: &str = "log-sources";
pub(crate) const DIAGNOSTICS: &str = "diagnostics";

#[derive(Debug, Error)]
pub enum StorageError {
    #[error("database path has no usable parent")]
    InvalidDatabasePath,
    #[error("orchestrator database is already owned by another process: {0}")]
    AlreadyLocked(String),
    #[error("schema migration {version} ({name}) checksum mismatch")]
    MigrationChecksum { version: u32, name: String },
    #[error("schema migration {0} is newer than this binary supports")]
    UnsupportedSchema(u32),
    #[error("required schema object is missing: {0}")]
    MissingSchemaObject(String),
    #[error("optimistic concurrency conflict: {0}")]
    Conflict(String),
    #[error("storage invariant failed: {0}")]
    Invariant(String),
    #[error("domain validation failed: {0}")]
    Domain(String),
    #[error("sqlite error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("filesystem error: {0}")]
    Io(#[from] std::io::Error),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
}

pub type StorageResult<T> = std::result::Result<T, StorageError>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SynchronousMode {
    Normal,
    Full,
}

impl SynchronousMode {
    fn pragma(self) -> &'static str {
        match self {
            Self::Normal => "NORMAL",
            Self::Full => "FULL",
        }
    }
}

#[derive(Debug, Clone)]
pub struct SqliteOptions {
    pub busy_timeout: Duration,
    pub synchronous: SynchronousMode,
    pub acquire_instance_lock: bool,
}

impl Default for SqliteOptions {
    fn default() -> Self {
        Self {
            busy_timeout: Duration::from_secs(5),
            synchronous: SynchronousMode::Full,
            acquire_instance_lock: true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppliedMigration {
    pub version: u32,
    pub name: String,
    pub checksum: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadinessReport {
    pub quick_check: String,
    pub journal_mode: String,
    pub foreign_keys: bool,
    pub busy_timeout_ms: u64,
    pub schema_version: u32,
    pub expected_schema_version: u32,
}

#[derive(Debug, Clone, Copy)]
struct Migration {
    version: u32,
    name: &'static str,
    sql: &'static str,
}

#[derive(Debug)]
struct InstanceLock {
    file: File,
}

impl InstanceLock {
    fn acquire(database_path: &Path) -> StorageResult<Self> {
        let mut extension = database_path
            .extension()
            .and_then(|value| value.to_str())
            .map(|value| format!("{value}."))
            .unwrap_or_default();
        extension.push_str("lock");
        let lock_path = database_path.with_extension(extension);
        let mut file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&lock_path)?;
        file.try_lock_exclusive().map_err(|error| {
            if error.raw_os_error() == Some(33)
                || matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::PermissionDenied
                )
            {
                StorageError::AlreadyLocked(lock_path.display().to_string())
            } else {
                StorageError::Io(error)
            }
        })?;
        file.set_len(0)?;
        file.seek(SeekFrom::Start(0))?;
        writeln!(file, "pid={}", std::process::id())?;
        file.sync_data()?;
        Ok(Self { file })
    }
}

impl Drop for InstanceLock {
    fn drop(&mut self) {
        let _ = FileExt::unlock(&self.file);
    }
}

/// SQLite implementation of the complete current `OrchestratorStore`.
///
/// Clones share the process ownership guard. Each call opens a short-lived
/// connection, avoiding a process-wide mutex around database or external I/O.
#[derive(Debug, Clone)]
pub struct SqliteOrchestratorStore {
    path: Arc<PathBuf>,
    options: SqliteOptions,
    _instance_lock: Option<Arc<InstanceLock>>,
}

impl SqliteOrchestratorStore {
    pub fn open(path: impl AsRef<Path>) -> StorageResult<Self> {
        Self::open_with_options(path, SqliteOptions::default())
    }

    pub fn open_with_options(
        path: impl AsRef<Path>,
        options: SqliteOptions,
    ) -> StorageResult<Self> {
        let path = absolute_path(path.as_ref())?;
        let parent = path.parent().ok_or(StorageError::InvalidDatabasePath)?;
        std::fs::create_dir_all(parent)?;
        let file_name = path.file_name().ok_or(StorageError::InvalidDatabasePath)?;
        let path = parent.canonicalize()?.join(file_name);
        let instance_lock = options
            .acquire_instance_lock
            .then(|| InstanceLock::acquire(&path))
            .transpose()?
            .map(Arc::new);
        let store = Self {
            path: Arc::new(path),
            options,
            _instance_lock: instance_lock,
        };
        let mut connection = store.connection()?;
        apply_migrations(&mut connection)?;
        drop(connection);
        store.import_legacy_v0_2()?;
        store.readiness()?;
        Ok(store)
    }

    pub fn path(&self) -> &Path {
        self.path.as_path()
    }

    pub fn applied_migrations(&self) -> StorageResult<Vec<AppliedMigration>> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT version, name, checksum FROM orchestrator_schema_migrations ORDER BY version",
        )?;
        statement
            .query_map([], |row| {
                Ok(AppliedMigration {
                    version: row.get(0)?,
                    name: row.get(1)?,
                    checksum: row.get(2)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(StorageError::from)
    }

    pub fn readiness(&self) -> StorageResult<ReadinessReport> {
        let connection = self.connection()?;
        verify_migrations(&connection)?;
        verify_schema_objects(&connection)?;
        let quick_check: String =
            connection.query_row("PRAGMA quick_check", [], |row| row.get(0))?;
        if quick_check != "ok" {
            return Err(StorageError::Sqlite(rusqlite::Error::InvalidQuery));
        }
        Ok(ReadinessReport {
            quick_check,
            journal_mode: connection.query_row("PRAGMA journal_mode", [], |row| row.get(0))?,
            foreign_keys: connection
                .query_row::<i64, _, _>("PRAGMA foreign_keys", [], |row| row.get(0))?
                == 1,
            busy_timeout_ms: connection.query_row("PRAGMA busy_timeout", [], |row| row.get(0))?,
            schema_version: connection.query_row("PRAGMA user_version", [], |row| row.get(0))?,
            expected_schema_version: latest_schema_version(),
        })
    }

    /// Persists layouts and per-user preferences separately from desired state.
    pub fn put_state<T: Serialize>(
        &self,
        namespace: &str,
        key: &str,
        value: &T,
    ) -> StorageResult<()> {
        let payload = serde_json::to_string(value)?;
        self.connection()?.execute(
            "INSERT INTO orchestrator_state(namespace, state_key, payload) VALUES (?1, ?2, ?3) ON CONFLICT(namespace, state_key) DO UPDATE SET payload = excluded.payload, updated_at = unixepoch()",
            params![namespace, key, payload],
        )?;
        Ok(())
    }

    pub fn get_state<T: DeserializeOwned>(
        &self,
        namespace: &str,
        key: &str,
    ) -> StorageResult<Option<T>> {
        let payload = self
            .connection()?
            .query_row(
                "SELECT payload FROM orchestrator_state WHERE namespace = ?1 AND state_key = ?2",
                params![namespace, key],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        payload
            .map(|value| serde_json::from_str(&value).map_err(StorageError::from))
            .transpose()
    }

    pub fn delete_state(&self, namespace: &str, key: &str) -> StorageResult<bool> {
        Ok(self.connection()?.execute(
            "DELETE FROM orchestrator_state WHERE namespace = ?1 AND state_key = ?2",
            params![namespace, key],
        )? > 0)
    }

    pub(crate) fn connection(&self) -> StorageResult<Connection> {
        let connection = Connection::open(self.path.as_path())?;
        connection.busy_timeout(self.options.busy_timeout)?;
        connection.execute_batch(&format!(
            "PRAGMA foreign_keys = ON; PRAGMA journal_mode = WAL; PRAGMA synchronous = {};",
            self.options.synchronous.pragma()
        ))?;
        Ok(connection)
    }

    fn core_connection(&self) -> orchestrator_legacy::Result<Connection> {
        self.connection().map_err(core_storage_error)
    }

    fn list_records<T: DeserializeOwned>(&self, kind: &str) -> orchestrator_legacy::Result<Vec<T>> {
        let connection = self.core_connection()?;
        let mut statement = connection
            .prepare("SELECT payload FROM orchestrator_records WHERE kind = ?1 ORDER BY record_key")
            .map_err(core_sqlite_error)?;
        let payloads = statement
            .query_map([kind], |row| row.get::<_, String>(0))
            .map_err(core_sqlite_error)?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(core_sqlite_error)?;
        payloads
            .into_iter()
            .map(|value| serde_json::from_str(&value).map_err(OrchestratorError::from))
            .collect()
    }

    fn get_record<T: DeserializeOwned>(
        &self,
        kind: &str,
        key: &str,
    ) -> orchestrator_legacy::Result<Option<T>> {
        let payload = self
            .core_connection()?
            .query_row(
                "SELECT payload FROM orchestrator_records WHERE kind = ?1 AND record_key = ?2",
                params![kind, key],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(core_sqlite_error)?;
        payload
            .map(|value| serde_json::from_str(&value).map_err(OrchestratorError::from))
            .transpose()
    }

    fn upsert_record<T: Serialize>(
        &self,
        kind: &str,
        key: &str,
        scope: &str,
        value: &T,
    ) -> orchestrator_legacy::Result<()> {
        let payload = serde_json::to_string(value)?;
        self.core_connection()?.execute(
            "INSERT INTO orchestrator_records(kind, record_key, scope, payload) VALUES (?1, ?2, ?3, ?4) ON CONFLICT(kind, record_key) DO UPDATE SET scope = excluded.scope, payload = excluded.payload, updated_at = unixepoch()",
            params![kind, key, scope, payload],
        ).map_err(core_sqlite_error)?;
        Ok(())
    }

    fn delete_record(&self, kind: &str, key: &str) -> orchestrator_legacy::Result<bool> {
        Ok(self
            .core_connection()?
            .execute(
                "DELETE FROM orchestrator_records WHERE kind = ?1 AND record_key = ?2",
                params![kind, key],
            )
            .map_err(core_sqlite_error)?
            > 0)
    }

    fn delete_scope(&self, kind: &str, scope: &str) -> orchestrator_legacy::Result<()> {
        self.core_connection()?
            .execute(
                "DELETE FROM orchestrator_records WHERE kind = ?1 AND scope = ?2",
                params![kind, scope],
            )
            .map_err(core_sqlite_error)?;
        Ok(())
    }

    fn update_record<T, F>(
        &self,
        kind: &str,
        key: &str,
        update: F,
    ) -> orchestrator_legacy::Result<()>
    where
        T: Serialize + DeserializeOwned,
        F: FnOnce(&mut T),
    {
        let mut connection = self.core_connection()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(core_sqlite_error)?;
        let payload = transaction
            .query_row(
                "SELECT payload FROM orchestrator_records WHERE kind = ?1 AND record_key = ?2",
                params![kind, key],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(core_sqlite_error)?
            .ok_or_else(|| {
                OrchestratorError::Dependency(format!("{kind} record {key} not found"))
            })?;
        let mut value: T = serde_json::from_str(&payload)?;
        update(&mut value);
        transaction.execute(
            "UPDATE orchestrator_records SET payload = ?3, updated_at = unixepoch() WHERE kind = ?1 AND record_key = ?2",
            params![kind, key, serde_json::to_string(&value)?],
        ).map_err(core_sqlite_error)?;
        transaction.commit().map_err(core_sqlite_error)
    }
}

fn absolute_path(path: &Path) -> StorageResult<PathBuf> {
    if path.is_absolute() {
        Ok(path.to_path_buf())
    } else {
        Ok(std::env::current_dir()?.join(path))
    }
}

fn apply_migrations(connection: &mut Connection) -> StorageResult<()> {
    connection.execute_batch(
        "CREATE TABLE IF NOT EXISTS orchestrator_schema_migrations (version INTEGER PRIMARY KEY, name TEXT NOT NULL, checksum TEXT NOT NULL, applied_at INTEGER NOT NULL DEFAULT (unixepoch()));",
    )?;
    verify_migrations(connection)?;
    for migration in MIGRATIONS {
        let existing = connection
            .query_row(
                "SELECT checksum FROM orchestrator_schema_migrations WHERE version = ?1",
                [migration.version],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        if existing.is_some() {
            continue;
        }
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        transaction.execute_batch(migration.sql)?;
        transaction.execute(
            "INSERT INTO orchestrator_schema_migrations(version, name, checksum) VALUES (?1, ?2, ?3)",
            params![migration.version, migration.name, migration_checksum(migration)],
        )?;
        transaction.pragma_update(None, "user_version", migration.version)?;
        transaction.commit()?;
    }
    verify_migrations(connection)
}

fn verify_migrations(connection: &Connection) -> StorageResult<()> {
    let newest: u32 = connection.query_row(
        "SELECT COALESCE(MAX(version), 0) FROM orchestrator_schema_migrations",
        [],
        |row| row.get(0),
    )?;
    if newest > latest_schema_version() {
        return Err(StorageError::UnsupportedSchema(newest));
    }
    for migration in MIGRATIONS {
        if let Some(checksum) = connection
            .query_row(
                "SELECT checksum FROM orchestrator_schema_migrations WHERE version = ?1",
                [migration.version],
                |row| row.get::<_, String>(0),
            )
            .optional()?
            && checksum != migration_checksum(migration)
        {
            return Err(StorageError::MigrationChecksum {
                version: migration.version,
                name: migration.name.to_string(),
            });
        }
    }
    Ok(())
}

fn verify_schema_objects(connection: &Connection) -> StorageResult<()> {
    for table in REQUIRED_TABLES {
        let exists: bool = connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1)",
            [table],
            |row| row.get(0),
        )?;
        if !exists {
            return Err(StorageError::MissingSchemaObject((*table).to_string()));
        }
    }
    for index in REQUIRED_INDEXES {
        let exists: bool = connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'index' AND name = ?1)",
            [index],
            |row| row.get(0),
        )?;
        if !exists {
            return Err(StorageError::MissingSchemaObject((*index).to_string()));
        }
    }
    for trigger in REQUIRED_TRIGGERS {
        let exists: bool = connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'trigger' AND name = ?1 AND tbl_name = 'orchestrator_audit_log')",
            [trigger],
            |row| row.get(0),
        )?;
        if !exists {
            return Err(StorageError::MissingSchemaObject((*trigger).to_string()));
        }
    }
    Ok(())
}

fn migration_checksum(migration: &Migration) -> String {
    format!("sha256:{:x}", Sha256::digest(migration.sql.as_bytes()))
}

fn latest_schema_version() -> u32 {
    MIGRATIONS
        .last()
        .map(|migration| migration.version)
        .unwrap_or(0)
}

fn key(parts: &[&str]) -> String {
    serde_json::to_string(parts).expect("string slices always serialize")
}

fn core_storage_error(error: StorageError) -> OrchestratorError {
    OrchestratorError::Dependency(format!("orchestrator sqlite storage: {error}"))
}

fn core_sqlite_error(error: rusqlite::Error) -> OrchestratorError {
    OrchestratorError::Dependency(format!("orchestrator sqlite storage: {error}"))
}

pub(crate) fn validate_node_tree(
    mut nodes: Vec<NodeRecord>,
    candidate: &NodeRecord,
) -> orchestrator_legacy::Result<()> {
    if nodes
        .iter()
        .any(|node| node.node_id != candidate.node_id && node.host_ip == candidate.host_ip)
    {
        return Err(OrchestratorError::InvalidManifest(format!(
            "node host_ip {} is already registered",
            candidate.host_ip
        )));
    }
    match candidate.role.as_str() {
        "root" | "standalone" if !candidate.parent_node_id.trim().is_empty() => {
            return Err(OrchestratorError::InvalidManifest(format!(
                "{} node must not have parent_node_id",
                candidate.role
            )));
        }
        "node" if candidate.parent_node_id.trim().is_empty() => {
            return Err(OrchestratorError::InvalidManifest(
                "node parent_node_id is required".to_string(),
            ));
        }
        _ => {}
    }
    nodes.retain(|node| node.node_id != candidate.node_id);
    nodes.push(candidate.clone());
    let by_id = nodes
        .into_iter()
        .map(|node| (node.node_id.clone(), node))
        .collect::<BTreeMap<_, _>>();
    for node in by_id.values() {
        let mut current = node;
        let mut seen = BTreeSet::new();
        while !current.parent_node_id.trim().is_empty() {
            if !seen.insert(current.node_id.clone()) {
                return Err(OrchestratorError::InvalidManifest(format!(
                    "node tree contains cycle at {}",
                    current.node_id
                )));
            }
            current = by_id.get(&current.parent_node_id).ok_or_else(|| {
                OrchestratorError::Dependency(format!(
                    "parent node {} not found",
                    current.parent_node_id
                ))
            })?;
        }
    }
    Ok(())
}

impl OrchestratorStore for SqliteOrchestratorStore {
    fn list_services(&self) -> orchestrator_legacy::Result<Vec<ServiceManifest>> {
        self.list_records(SERVICES)
    }

    fn get_service(
        &self,
        service_id: &str,
    ) -> orchestrator_legacy::Result<Option<ServiceManifest>> {
        self.get_record(SERVICES, service_id)
    }

    fn upsert_service(&mut self, value: ServiceManifest) -> orchestrator_legacy::Result<()> {
        validate_service_manifest(&value)?;
        self.upsert_record(SERVICES, &value.id, &value.id, &value)
    }

    fn delete_service(&mut self, service_id: &str) -> orchestrator_legacy::Result<()> {
        let endpoint_ids = self
            .list_endpoints()?
            .into_iter()
            .filter(|endpoint| endpoint.service_id == service_id)
            .map(|endpoint| endpoint.endpoint)
            .collect::<Vec<_>>();
        let mut connection = self.core_connection()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(core_sqlite_error)?;
        for endpoint in endpoint_ids {
            transaction.execute(
                "DELETE FROM orchestrator_records WHERE (kind = ?1 AND json_extract(payload, '$.endpoint') = ?2) OR (kind = ?3 AND (json_extract(payload, '$.source_endpoint') = ?2 OR json_extract(payload, '$.target_endpoint') = ?2)) OR (kind = ?4 AND json_extract(payload, '$.endpoint') = ?2)",
                params![DEPLOYED_APIS, endpoint, LINKS, LOG_SOURCES],
            ).map_err(core_sqlite_error)?;
        }
        transaction.execute(
            "DELETE FROM orchestrator_records WHERE (kind = ?1 AND record_key = ?2) OR (kind IN (?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14) AND scope = ?2)",
            params![SERVICES, service_id, HOST_SERVICES, RELEASES, ROUTES, MIGRATION_RECORDS,
                PERMISSION_RECORDS, FRONTENDS, REDIS_RESOURCES, STORAGE_RESOURCES,
                RENDERED_CONFIGS, API_SURFACES, DEPLOYED_APIS, ENDPOINTS],
        ).map_err(core_sqlite_error)?;
        transaction.commit().map_err(core_sqlite_error)
    }

    fn list_host_services(&self) -> orchestrator_legacy::Result<Vec<HostService>> {
        self.list_records(HOST_SERVICES)
    }

    fn get_host_service(
        &self,
        host_ip: &str,
        service_name: &str,
    ) -> orchestrator_legacy::Result<Option<HostService>> {
        self.get_record(HOST_SERVICES, &key(&[host_ip, service_name]))
    }

    fn upsert_host_service(&mut self, value: HostService) -> orchestrator_legacy::Result<()> {
        validate_host_service(&value)?;
        self.upsert_record(
            HOST_SERVICES,
            &key(&[&value.host_ip, &value.service_name]),
            &value.service_name,
            &value,
        )
    }

    fn delete_host_service(
        &mut self,
        host_ip: &str,
        service_name: &str,
    ) -> orchestrator_legacy::Result<()> {
        self.delete_record(HOST_SERVICES, &key(&[host_ip, service_name]))
            .map(|_| ())
    }

    fn delete_host_services_for_service(
        &mut self,
        service_name: &str,
    ) -> orchestrator_legacy::Result<()> {
        self.delete_scope(HOST_SERVICES, service_name)
    }

    fn list_service_releases(&self) -> orchestrator_legacy::Result<Vec<ServiceRelease>> {
        self.list_records(RELEASES)
    }

    fn get_service_release(
        &self,
        service_name: &str,
        version: &str,
    ) -> orchestrator_legacy::Result<Option<ServiceRelease>> {
        self.get_record(RELEASES, &key(&[service_name, version]))
    }

    fn upsert_service_release(&mut self, value: ServiceRelease) -> orchestrator_legacy::Result<()> {
        validate_service_release_record(&value)?;
        self.upsert_record(
            RELEASES,
            &key(&[&value.service_name, &value.version]),
            &value.service_name,
            &value,
        )
    }

    fn delete_service_release(
        &mut self,
        service_name: &str,
        version: &str,
    ) -> orchestrator_legacy::Result<()> {
        self.delete_record(RELEASES, &key(&[service_name, version]))
            .map(|_| ())
    }

    fn register_service_release_atomic(
        &mut self,
        service: ServiceManifest,
        release: ServiceRelease,
    ) -> orchestrator_legacy::Result<()> {
        validate_service_manifest(&service)?;
        validate_service_release_record(&release)?;
        if service.id != release.service_name || service.version != release.version {
            return Err(OrchestratorError::InvalidManifest(
                "service and release identities must match".to_string(),
            ));
        }
        let service_payload = serde_json::to_string(&service)?;
        let release_payload = serde_json::to_string(&release)?;
        let release_key = key(&[&release.service_name, &release.version]);
        let mut connection = self.core_connection()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(core_sqlite_error)?;
        transaction
            .execute(
                "INSERT INTO orchestrator_records(kind, record_key, scope, payload) VALUES (?1, ?2, ?2, ?3) ON CONFLICT(kind, record_key) DO UPDATE SET scope = excluded.scope, payload = excluded.payload, updated_at = unixepoch()",
                params![SERVICES, service.id, service_payload],
            )
            .map_err(core_sqlite_error)?;
        transaction
            .execute(
                "INSERT INTO orchestrator_records(kind, record_key, scope, payload) VALUES (?1, ?2, ?3, ?4) ON CONFLICT(kind, record_key) DO UPDATE SET scope = excluded.scope, payload = excluded.payload, updated_at = unixepoch()",
                params![RELEASES, release_key, release.service_name, release_payload],
            )
            .map_err(core_sqlite_error)?;
        transaction.commit().map_err(core_sqlite_error)
    }

    fn list_service_routes(&self) -> orchestrator_legacy::Result<Vec<ServiceRoute>> {
        self.list_records(ROUTES)
    }

    fn upsert_service_route(&mut self, value: ServiceRoute) -> orchestrator_legacy::Result<()> {
        validate_service_route(&value)?;
        self.upsert_record(
            ROUTES,
            &key(&[&value.path, &value.method]),
            &value.target_service_name,
            &value,
        )
    }

    fn delete_service_routes_for_service(
        &mut self,
        service_name: &str,
    ) -> orchestrator_legacy::Result<()> {
        self.delete_scope(ROUTES, service_name)
    }

    fn list_service_migration_records(
        &self,
    ) -> orchestrator_legacy::Result<Vec<ServiceMigrationRecord>> {
        self.list_records(MIGRATION_RECORDS)
    }

    fn upsert_service_migration_record(
        &mut self,
        value: ServiceMigrationRecord,
    ) -> orchestrator_legacy::Result<()> {
        validate_service_migration_record(&value)?;
        self.upsert_record(
            MIGRATION_RECORDS,
            &key(&[&value.service_name, &value.migration_version]),
            &value.service_name,
            &value,
        )
    }

    fn delete_service_migration_records_for_service(
        &mut self,
        service_name: &str,
    ) -> orchestrator_legacy::Result<()> {
        self.delete_scope(MIGRATION_RECORDS, service_name)
    }

    fn list_service_permission_records(
        &self,
    ) -> orchestrator_legacy::Result<Vec<ServicePermissionRecord>> {
        self.list_records(PERMISSION_RECORDS)
    }

    fn upsert_service_permission_record(
        &mut self,
        value: ServicePermissionRecord,
    ) -> orchestrator_legacy::Result<()> {
        validate_service_permission_record(&value)?;
        self.upsert_record(
            PERMISSION_RECORDS,
            &key(&[&value.service_name, &value.permission_key]),
            &value.service_name,
            &value,
        )
    }

    fn delete_service_permission_records_for_service(
        &mut self,
        service_name: &str,
    ) -> orchestrator_legacy::Result<()> {
        self.delete_scope(PERMISSION_RECORDS, service_name)
    }

    fn list_service_frontend_entries(
        &self,
    ) -> orchestrator_legacy::Result<Vec<ServiceFrontendEntry>> {
        self.list_records(FRONTENDS)
    }

    fn upsert_service_frontend_entry(
        &mut self,
        value: ServiceFrontendEntry,
    ) -> orchestrator_legacy::Result<()> {
        validate_service_frontend_entry(&value)?;
        self.upsert_record(FRONTENDS, &value.service_name, &value.service_name, &value)
    }

    fn delete_service_frontend_entry(
        &mut self,
        service_name: &str,
    ) -> orchestrator_legacy::Result<()> {
        self.delete_record(FRONTENDS, service_name).map(|_| ())
    }

    fn list_service_redis_resources(
        &self,
    ) -> orchestrator_legacy::Result<Vec<ServiceRedisResource>> {
        self.list_records(REDIS_RESOURCES)
    }

    fn upsert_service_redis_resource(
        &mut self,
        value: ServiceRedisResource,
    ) -> orchestrator_legacy::Result<()> {
        validate_service_redis_resource(&value)?;
        self.upsert_record(
            REDIS_RESOURCES,
            &key(&[&value.service_name, &value.name]),
            &value.service_name,
            &value,
        )
    }

    fn delete_service_redis_resources_for_service(
        &mut self,
        service_name: &str,
    ) -> orchestrator_legacy::Result<()> {
        self.delete_scope(REDIS_RESOURCES, service_name)
    }

    fn list_service_storage_resources(
        &self,
    ) -> orchestrator_legacy::Result<Vec<ServiceStorageResource>> {
        self.list_records(STORAGE_RESOURCES)
    }

    fn upsert_service_storage_resource(
        &mut self,
        value: ServiceStorageResource,
    ) -> orchestrator_legacy::Result<()> {
        validate_service_storage_resource(&value)?;
        self.upsert_record(
            STORAGE_RESOURCES,
            &key(&[&value.service_name, &value.object_type, &value.bucket]),
            &value.service_name,
            &value,
        )
    }

    fn delete_service_storage_resources_for_service(
        &mut self,
        service_name: &str,
    ) -> orchestrator_legacy::Result<()> {
        self.delete_scope(STORAGE_RESOURCES, service_name)
    }

    fn list_rendered_service_configs(
        &self,
    ) -> orchestrator_legacy::Result<Vec<RenderedServiceConfig>> {
        self.list_records(RENDERED_CONFIGS)
    }

    fn upsert_rendered_service_config(
        &mut self,
        value: RenderedServiceConfig,
    ) -> orchestrator_legacy::Result<()> {
        validate_rendered_service_config(&value)?;
        self.upsert_record(
            RENDERED_CONFIGS,
            &key(&[&value.service_name, &value.version]),
            &value.service_name,
            &value,
        )
    }

    fn delete_rendered_service_configs_for_service(
        &mut self,
        service_name: &str,
    ) -> orchestrator_legacy::Result<()> {
        self.delete_scope(RENDERED_CONFIGS, service_name)
    }

    fn list_nodes(&self) -> orchestrator_legacy::Result<Vec<NodeRecord>> {
        self.list_records(NODES)
    }

    fn get_node(&self, node_id: &str) -> orchestrator_legacy::Result<Option<NodeRecord>> {
        self.get_record(NODES, node_id)
    }

    fn upsert_node(&mut self, value: NodeRecord) -> orchestrator_legacy::Result<()> {
        validate_node_record(&value)?;
        validate_node_tree(self.list_nodes()?, &value)?;
        self.upsert_record(NODES, &value.node_id, &value.parent_node_id, &value)
    }

    fn delete_node(&mut self, node_id: &str) -> orchestrator_legacy::Result<()> {
        if self
            .list_nodes()?
            .iter()
            .any(|node| node.parent_node_id == node_id)
        {
            return Err(OrchestratorError::Dependency(format!(
                "node {node_id} has child nodes"
            )));
        }
        self.delete_record(NODES, node_id).map(|_| ())
    }

    fn list_service_api_surfaces(&self) -> orchestrator_legacy::Result<Vec<ServiceApiSurface>> {
        self.list_records(API_SURFACES)
    }

    fn upsert_service_api_surface(
        &mut self,
        value: ServiceApiSurface,
    ) -> orchestrator_legacy::Result<()> {
        validate_service_api_surface(&value)?;
        self.upsert_record(
            API_SURFACES,
            &key(&[&value.service_name, &value.version, &value.api_id]),
            &value.service_name,
            &value,
        )
    }

    fn delete_service_api_surfaces_for_service(
        &mut self,
        service_name: &str,
    ) -> orchestrator_legacy::Result<()> {
        self.delete_scope(API_SURFACES, service_name)
    }

    fn list_deployed_service_apis(&self) -> orchestrator_legacy::Result<Vec<DeployedServiceApi>> {
        self.list_records(DEPLOYED_APIS)
    }

    fn upsert_deployed_service_api(
        &mut self,
        value: DeployedServiceApi,
    ) -> orchestrator_legacy::Result<()> {
        validate_deployed_service_api(&value)?;
        if !self
            .list_nodes()?
            .iter()
            .any(|node| node.host_ip == value.host_ip)
        {
            return Err(OrchestratorError::Dependency(format!(
                "deployed api references host_ip {} without node",
                value.host_ip
            )));
        }
        if self.get_endpoint(&value.endpoint)?.is_none() {
            return Err(OrchestratorError::Dependency(format!(
                "deployed api references missing endpoint {}",
                value.endpoint
            )));
        }
        if !self.list_service_api_surfaces()?.iter().any(|api| {
            api.service_name == value.service_name
                && api.version == value.version
                && api.api_id == value.api_id
        }) {
            return Err(OrchestratorError::Dependency(format!(
                "deployed api references missing api surface {}@{}:{}",
                value.service_name, value.version, value.api_id
            )));
        }
        self.upsert_record(
            DEPLOYED_APIS,
            &key(&[
                &value.host_ip,
                &value.service_name,
                &value.api_id,
                &value.endpoint,
            ]),
            &value.service_name,
            &value,
        )
    }

    fn delete_deployed_service_apis_for_service(
        &mut self,
        service_name: &str,
    ) -> orchestrator_legacy::Result<()> {
        self.delete_scope(DEPLOYED_APIS, service_name)
    }

    fn list_endpoints(&self) -> orchestrator_legacy::Result<Vec<Endpoint>> {
        self.list_records(ENDPOINTS)
    }

    fn get_endpoint(&self, endpoint: &str) -> orchestrator_legacy::Result<Option<Endpoint>> {
        self.get_record(ENDPOINTS, endpoint)
    }

    fn upsert_endpoint(&mut self, value: Endpoint) -> orchestrator_legacy::Result<()> {
        validate_endpoint(&value)?;
        if self.get_service(&value.service_id)?.is_none() {
            return Err(OrchestratorError::Dependency(format!(
                "endpoint references missing service {}",
                value.service_id
            )));
        }
        self.upsert_record(ENDPOINTS, &value.endpoint, &value.service_id, &value)
    }

    fn delete_endpoint(&mut self, endpoint: &str) -> orchestrator_legacy::Result<()> {
        validate_endpoint_id(endpoint)?;
        let mut connection = self.core_connection()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(core_sqlite_error)?;
        transaction.execute(
            "DELETE FROM orchestrator_records WHERE (kind = ?1 AND record_key = ?2) OR (kind = ?3 AND json_extract(payload, '$.endpoint') = ?2) OR (kind = ?4 AND (json_extract(payload, '$.source_endpoint') = ?2 OR json_extract(payload, '$.target_endpoint') = ?2)) OR (kind = ?5 AND json_extract(payload, '$.endpoint') = ?2)",
            params![ENDPOINTS, endpoint, DEPLOYED_APIS, LINKS, LOG_SOURCES],
        ).map_err(core_sqlite_error)?;
        transaction.commit().map_err(core_sqlite_error)
    }

    fn update_endpoint_health(
        &mut self,
        endpoint: &str,
        health: String,
        reachable: bool,
    ) -> orchestrator_legacy::Result<()> {
        validate_endpoint_id(endpoint)?;
        self.update_record::<Endpoint, _>(ENDPOINTS, endpoint, |value| {
            value.health = health;
            value.reachable = reachable;
        })
    }

    fn list_links(&self) -> orchestrator_legacy::Result<Vec<Link>> {
        self.list_records(LINKS)
    }

    fn get_link(
        &self,
        source_endpoint: &str,
        target_endpoint: &str,
    ) -> orchestrator_legacy::Result<Option<Link>> {
        self.get_record(LINKS, &key(&[source_endpoint, target_endpoint]))
    }

    fn upsert_link(&mut self, value: Link) -> orchestrator_legacy::Result<()> {
        validate_link(&value, &self.list_endpoints()?)?;
        self.upsert_record(
            LINKS,
            &key(&[&value.source_endpoint, &value.target_endpoint]),
            &value.source_endpoint,
            &value,
        )
    }

    fn delete_link(
        &mut self,
        source_endpoint: &str,
        target_endpoint: &str,
    ) -> orchestrator_legacy::Result<()> {
        validate_endpoint_id(source_endpoint)?;
        validate_endpoint_id(target_endpoint)?;
        if !self.delete_record(LINKS, &key(&[source_endpoint, target_endpoint]))? {
            return Err(OrchestratorError::Dependency(format!(
                "link {source_endpoint} -> {target_endpoint} not found"
            )));
        }
        Ok(())
    }

    fn update_link_health(
        &mut self,
        source_endpoint: &str,
        target_endpoint: &str,
        health: String,
        latency_ms: Option<u32>,
    ) -> orchestrator_legacy::Result<()> {
        validate_endpoint_id(source_endpoint)?;
        validate_endpoint_id(target_endpoint)?;
        self.update_record::<Link, _>(LINKS, &key(&[source_endpoint, target_endpoint]), |value| {
            value.health = health;
            value.latency_ms = latency_ms;
        })
    }

    fn create_operation(&mut self, value: Operation) -> orchestrator_legacy::Result<()> {
        self.upsert_record(OPERATIONS, &value.operation_id, &value.target_id, &value)
    }

    fn get_operation(&self, operation_id: &str) -> orchestrator_legacy::Result<Option<Operation>> {
        self.get_record(OPERATIONS, operation_id)
    }

    fn list_operations(&self) -> orchestrator_legacy::Result<Vec<Operation>> {
        self.list_records(OPERATIONS)
    }

    fn update_operation(&mut self, value: Operation) -> orchestrator_legacy::Result<()> {
        self.upsert_record(OPERATIONS, &value.operation_id, &value.target_id, &value)
    }

    fn update_operation_status(
        &mut self,
        operation_id: &str,
        status: OperationStatus,
        error_message: String,
    ) -> orchestrator_legacy::Result<()> {
        self.update_record::<Operation, _>(OPERATIONS, operation_id, |value| {
            value.status = status;
            value.error_message = error_message;
        })
    }

    fn update_operation_result(
        &mut self,
        operation_id: &str,
        result: serde_json::Value,
    ) -> orchestrator_legacy::Result<()> {
        self.update_record::<Operation, _>(OPERATIONS, operation_id, |value| value.result = result)
    }

    fn append_operation_log(
        &mut self,
        mut value: OperationLogRecord,
    ) -> orchestrator_legacy::Result<()> {
        let mut connection = self.core_connection()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(core_sqlite_error)?;
        let exists: bool = transaction.query_row(
            "SELECT EXISTS(SELECT 1 FROM orchestrator_records WHERE kind = ?1 AND record_key = ?2)",
            params![OPERATIONS, &value.operation_id], |row| row.get(0),
        ).map_err(core_sqlite_error)?;
        if !exists {
            return Err(OrchestratorError::Dependency(format!(
                "operation log references missing operation {}",
                value.operation_id
            )));
        }
        if value.created_at.is_empty() {
            let sequence: i64 = transaction
                .query_row(
                    "SELECT COALESCE(MAX(sequence), 0) + 1 FROM orchestrator_operation_logs_v2",
                    [],
                    |row| row.get(0),
                )
                .map_err(core_sqlite_error)?;
            value.created_at = format!("log-{sequence}");
        }
        transaction
            .execute(
                "INSERT INTO orchestrator_operation_logs_v2(operation_id, payload) VALUES (?1, ?2)",
                params![&value.operation_id, serde_json::to_string(&value)?],
            )
            .map_err(core_sqlite_error)?;
        transaction.commit().map_err(core_sqlite_error)
    }

    fn list_operation_logs(
        &self,
        operation_id: &str,
    ) -> orchestrator_legacy::Result<Vec<OperationLogRecord>> {
        let connection = self.core_connection()?;
        let mut statement = connection.prepare(
            "SELECT payload FROM orchestrator_operation_logs_v2 WHERE operation_id = ?1 ORDER BY sequence",
        ).map_err(core_sqlite_error)?;
        let payloads = statement
            .query_map([operation_id], |row| row.get::<_, String>(0))
            .map_err(core_sqlite_error)?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(core_sqlite_error)?;
        payloads
            .into_iter()
            .map(|value| serde_json::from_str(&value).map_err(OrchestratorError::from))
            .collect()
    }

    fn acquire_operation_lock(
        &mut self,
        value: OperationLock,
    ) -> orchestrator_legacy::Result<bool> {
        let mut connection = self.core_connection()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(core_sqlite_error)?;
        let exists: bool = transaction.query_row(
            "SELECT EXISTS(SELECT 1 FROM orchestrator_records WHERE kind = ?1 AND record_key = ?2)",
            params![OPERATIONS, &value.operation_id], |row| row.get(0),
        ).map_err(core_sqlite_error)?;
        if !exists {
            return Err(OrchestratorError::Dependency(format!(
                "lock references missing operation {}",
                value.operation_id
            )));
        }
        let inserted = transaction.execute(
            "INSERT INTO orchestrator_records(kind, record_key, scope, payload) VALUES (?1, ?2, ?3, ?4) ON CONFLICT(kind, record_key) DO NOTHING",
            params![OPERATION_LOCKS, &value.lock_key, &value.operation_id, serde_json::to_string(&value)?],
        ).map_err(core_sqlite_error)? > 0;
        transaction.commit().map_err(core_sqlite_error)?;
        Ok(inserted)
    }

    fn release_operation_lock(
        &mut self,
        lock_key: &str,
        operation_id: &str,
    ) -> orchestrator_legacy::Result<()> {
        self.core_connection()?.execute(
            "DELETE FROM orchestrator_records WHERE kind = ?1 AND record_key = ?2 AND scope = ?3",
            params![OPERATION_LOCKS, lock_key, operation_id],
        ).map_err(core_sqlite_error)?;
        Ok(())
    }

    fn save_topology_snapshot(
        &mut self,
        value: TopologySnapshot,
    ) -> orchestrator_legacy::Result<()> {
        validate_topology(&value.topology)?;
        self.upsert_record(
            TOPOLOGY_SNAPSHOTS,
            &value.snapshot_id,
            &value.topology.root_endpoint,
            &value,
        )
    }

    fn get_latest_topology_snapshot(
        &self,
    ) -> orchestrator_legacy::Result<Option<TopologySnapshot>> {
        let payload = self.core_connection()?.query_row(
            "SELECT payload FROM orchestrator_records WHERE kind = ?1 ORDER BY rowid DESC LIMIT 1",
            [TOPOLOGY_SNAPSHOTS], |row| row.get::<_, String>(0),
        ).optional().map_err(core_sqlite_error)?;
        payload
            .map(|value| serde_json::from_str(&value).map_err(OrchestratorError::from))
            .transpose()
    }

    fn build_topology_view(&self) -> orchestrator_legacy::Result<Topology> {
        let endpoints = self.list_endpoints()?;
        if endpoints.is_empty() {
            return self
                .get_latest_topology_snapshot()?
                .map(|snapshot| snapshot.topology)
                .ok_or_else(|| {
                    OrchestratorError::Dependency("no endpoint for topology".to_string())
                });
        }
        let root = endpoints
            .iter()
            .find(|endpoint| endpoint.service_id == "gateway")
            .or_else(|| endpoints.first())
            .map(|endpoint| endpoint.endpoint.clone())
            .ok_or_else(|| OrchestratorError::Dependency("no endpoint for topology".to_string()))?;
        build_topology(
            root,
            self.list_services()?
                .into_iter()
                .map(|service| service.id)
                .collect(),
            endpoints,
            self.list_links()?,
            self.list_operations()?,
            self.list_log_sources()?,
            self.list_diagnostic_reports()?,
        )
    }

    fn delete_topology(&mut self, root_endpoint: &str) -> orchestrator_legacy::Result<()> {
        self.delete_scope(TOPOLOGY_SNAPSHOTS, root_endpoint)
    }

    fn list_log_sources(&self) -> orchestrator_legacy::Result<Vec<LogView>> {
        self.list_records(LOG_SOURCES)
    }

    fn upsert_log_source(&mut self, value: LogView) -> orchestrator_legacy::Result<()> {
        validate_log_view(&value)?;
        if self.get_endpoint(&value.endpoint)?.is_none() {
            return Err(OrchestratorError::Dependency(format!(
                "log view references missing endpoint {}",
                value.endpoint
            )));
        }
        self.upsert_record(LOG_SOURCES, &value.source_id, &value.endpoint, &value)
    }

    fn delete_log_source(&mut self, source_id: &str) -> orchestrator_legacy::Result<()> {
        self.delete_record(LOG_SOURCES, source_id).map(|_| ())
    }

    fn create_diagnostic_report(
        &mut self,
        value: DiagnosticReport,
    ) -> orchestrator_legacy::Result<()> {
        self.upsert_record(DIAGNOSTICS, &value.report_id, &value.target_id, &value)
    }

    fn get_diagnostic_report(
        &self,
        report_id: &str,
    ) -> orchestrator_legacy::Result<Option<DiagnosticReport>> {
        self.get_record(DIAGNOSTICS, report_id)
    }

    fn list_diagnostic_reports(&self) -> orchestrator_legacy::Result<Vec<DiagnosticReport>> {
        self.list_records(DIAGNOSTICS)
    }
}
