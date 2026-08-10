use r2d2_postgres::{
    PostgresConnectionManager,
    postgres::{
        Client, Config, Error as PostgresDriverError, GenericClient, Transaction,
        config::{Host, SslMode},
    },
    r2d2::{Pool, PooledConnection},
};
use rustls_tokio_postgres::{MakeRustlsConnect, config_from_ca_cert, config_platform_verifier};
use sha2::{Digest, Sha256};
use std::{fmt, path::PathBuf, time::Duration};
use thiserror::Error;

pub const DEFAULT_CONTROL_PLANE_LOCK_KEY: i64 = i64::from_be_bytes(*b"OJOSCP01");
const MIGRATION_LOCK_KEY: i64 = i64::from_be_bytes(*b"OJOSMIG1");

const INITIAL_SCHEMA: &str = r#"
CREATE TABLE orchestrator_records (
    kind TEXT NOT NULL,
    record_key TEXT NOT NULL,
    scope TEXT NOT NULL DEFAULT '',
    payload JSONB NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    PRIMARY KEY (kind, record_key)
);
CREATE INDEX idx_orchestrator_records_kind_scope
    ON orchestrator_records(kind, scope, record_key);

CREATE TABLE orchestrator_operation_logs_v2 (
    sequence BIGSERIAL PRIMARY KEY,
    operation_id TEXT NOT NULL,
    payload JSONB NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp()
);
CREATE INDEX idx_orchestrator_operation_logs_v2_operation
    ON orchestrator_operation_logs_v2(operation_id, sequence);

CREATE TABLE orchestrator_state (
    namespace TEXT NOT NULL,
    state_key TEXT NOT NULL,
    payload JSONB NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    PRIMARY KEY (namespace, state_key)
);

CREATE TABLE orchestrator_jobs (
    job_id TEXT PRIMARY KEY,
    operation_id TEXT NOT NULL,
    node_id TEXT NOT NULL,
    idempotency_key TEXT NOT NULL,
    payload_sha256 TEXT NOT NULL,
    status TEXT NOT NULL,
    available_at_ms BIGINT NOT NULL,
    created_at_ms BIGINT NOT NULL,
    payload JSONB NOT NULL,
    UNIQUE(node_id, idempotency_key)
);
CREATE INDEX idx_orchestrator_jobs_claim
    ON orchestrator_jobs(node_id, status, available_at_ms, created_at_ms, job_id);
CREATE INDEX idx_orchestrator_jobs_operation
    ON orchestrator_jobs(operation_id, created_at_ms, job_id);

CREATE TABLE orchestrator_job_events (
    job_id TEXT NOT NULL,
    sequence BIGINT NOT NULL CHECK (sequence >= 0),
    payload JSONB NOT NULL,
    created_at_ms BIGINT NOT NULL,
    PRIMARY KEY(job_id, sequence),
    FOREIGN KEY(job_id) REFERENCES orchestrator_jobs(job_id) ON DELETE CASCADE
);
"#;

const TOPOLOGY_REVISION_SCHEMA: &str = r#"
CREATE TABLE orchestrator_topology_revisions (
    topology_id TEXT NOT NULL,
    revision_number BIGINT NOT NULL CHECK (revision_number > 0),
    revision_id TEXT NOT NULL UNIQUE,
    parent_revision_id TEXT,
    rollback_of_revision_id TEXT,
    content_sha256 TEXT NOT NULL,
    payload JSONB NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
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
    updated_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    FOREIGN KEY (draft_revision_id) REFERENCES orchestrator_topology_revisions(revision_id),
    FOREIGN KEY (applied_revision_id) REFERENCES orchestrator_topology_revisions(revision_id),
    FOREIGN KEY (applying_revision_id) REFERENCES orchestrator_topology_revisions(revision_id),
    CHECK ((applying_revision_id IS NULL) = (applying_operation_id IS NULL))
);

CREATE TABLE orchestrator_topology_status (
    topology_id TEXT PRIMARY KEY,
    desired_revision_id TEXT,
    observed_revision_id TEXT,
    payload JSONB NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    FOREIGN KEY (desired_revision_id) REFERENCES orchestrator_topology_revisions(revision_id),
    FOREIGN KEY (observed_revision_id) REFERENCES orchestrator_topology_revisions(revision_id)
);

CREATE TABLE orchestrator_runtime_instances (
    deployment_id TEXT PRIMARY KEY,
    node_id TEXT NOT NULL,
    service_id TEXT NOT NULL,
    desired_state TEXT NOT NULL,
    observed_state TEXT NOT NULL,
    payload JSONB NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp()
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
    response_headers JSONB,
    response_body JSONB,
    started_at_ms BIGINT NOT NULL,
    completed_at_ms BIGINT,
    expires_at_ms BIGINT NOT NULL,
    PRIMARY KEY (scope, idempotency_key)
);
CREATE INDEX idx_orchestrator_idempotency_expiry
    ON orchestrator_idempotency(expires_at_ms);
"#;

const DURABLE_OPERATION_SCHEMA: &str = r#"
CREATE TABLE orchestrator_durable_operations (
    operation_id TEXT PRIMARY KEY,
    revision BIGINT NOT NULL CHECK (revision > 0),
    status TEXT NOT NULL,
    payload JSONB NOT NULL,
    created_at_ms BIGINT NOT NULL,
    updated_at_ms BIGINT NOT NULL
);
CREATE INDEX idx_orchestrator_durable_operations_recovery
    ON orchestrator_durable_operations(status, updated_at_ms, operation_id);
"#;

const APPEND_ONLY_AUDIT_SCHEMA: &str = r#"
CREATE TABLE orchestrator_audit_log (
    sequence BIGSERIAL PRIMARY KEY,
    request_id TEXT NOT NULL,
    actor TEXT NOT NULL,
    action TEXT NOT NULL,
    resource TEXT NOT NULL,
    idempotency_key TEXT NOT NULL,
    request_digest TEXT NOT NULL,
    outcome TEXT NOT NULL CHECK (outcome IN ('INTENT', 'SUCCEEDED', 'REJECTED')),
    response_status INTEGER CHECK (response_status BETWEEN 100 AND 599),
    operation_id TEXT,
    timestamp_ms BIGINT NOT NULL CHECK (timestamp_ms >= 0),
    CHECK ((outcome = 'INTENT' AND response_status IS NULL) OR
           (outcome <> 'INTENT' AND response_status IS NOT NULL))
);
CREATE INDEX idx_orchestrator_audit_log_request
    ON orchestrator_audit_log(request_id, sequence);
CREATE INDEX idx_orchestrator_audit_log_operation
    ON orchestrator_audit_log(operation_id, sequence)
    WHERE operation_id IS NOT NULL;

CREATE FUNCTION orchestrator_reject_audit_mutation()
RETURNS trigger
LANGUAGE plpgsql
AS $audit$
BEGIN
    RAISE EXCEPTION 'orchestrator audit log is append-only';
END;
$audit$;

CREATE TRIGGER orchestrator_audit_log_no_update_or_delete
BEFORE UPDATE OR DELETE ON orchestrator_audit_log
FOR EACH ROW EXECUTE FUNCTION orchestrator_reject_audit_mutation();
"#;

const NODE_IDENTITY_SCHEMA: &str = r#"
CREATE TABLE orchestrator_node_enrollment_codes (
    code_id TEXT PRIMARY KEY,
    secret_sha256 TEXT NOT NULL UNIQUE,
    node_id TEXT NOT NULL,
    created_at_ms BIGINT NOT NULL CHECK (created_at_ms >= 0),
    expires_at_ms BIGINT NOT NULL CHECK (expires_at_ms > created_at_ms),
    redeemed_at_ms BIGINT CHECK (redeemed_at_ms IS NULL OR redeemed_at_ms >= created_at_ms)
);
CREATE INDEX idx_orchestrator_node_enrollment_expiry
    ON orchestrator_node_enrollment_codes(expires_at_ms, code_id);

CREATE TABLE orchestrator_node_certificates (
    serial_hex TEXT PRIMARY KEY,
    node_id TEXT NOT NULL,
    spiffe_id TEXT NOT NULL,
    certificate_pem TEXT NOT NULL,
    fingerprint_sha256 TEXT NOT NULL,
    issued_at_ms BIGINT NOT NULL CHECK (issued_at_ms >= 0),
    not_before_ms BIGINT NOT NULL,
    not_after_ms BIGINT NOT NULL CHECK (not_after_ms > not_before_ms),
    revoked_at_ms BIGINT,
    revoke_reason TEXT,
    replaced_by_serial TEXT REFERENCES orchestrator_node_certificates(serial_hex),
    CHECK ((revoked_at_ms IS NULL) = (revoke_reason IS NULL))
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
    report JSONB NOT NULL,
    imported_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp()
);
"#;

const ENROLLMENT_REPLAY_SCHEMA: &str = r#"
ALTER TABLE orchestrator_node_enrollment_codes
    ADD COLUMN redeemed_csr_sha256 TEXT,
    ADD COLUMN issued_certificate_serial TEXT
        REFERENCES orchestrator_node_certificates(serial_hex),
    ADD CONSTRAINT orchestrator_node_enrollment_replay_pair
        CHECK ((redeemed_csr_sha256 IS NULL) = (issued_certificate_serial IS NULL));
CREATE INDEX idx_orchestrator_node_enrollment_certificate
    ON orchestrator_node_enrollment_codes(issued_certificate_serial);
"#;

const CONTROL_PLANE_EVIDENCE_SCHEMA: &str = r#"
ALTER TABLE orchestrator_jobs ADD COLUMN lease_expires_at_ms BIGINT;
ALTER TABLE orchestrator_jobs ADD COLUMN updated_at_ms BIGINT NOT NULL DEFAULT 0;
UPDATE orchestrator_jobs
SET lease_expires_at_ms = NULLIF(payload->>'lease_expires_at_ms', '')::BIGINT,
    updated_at_ms = COALESCE(NULLIF(payload->>'updated_at_ms', '')::BIGINT, 0);
CREATE INDEX idx_orchestrator_jobs_lease_recovery
    ON orchestrator_jobs(status, lease_expires_at_ms, job_id)
    WHERE status IN ('LEASED', 'CANCEL_REQUESTED') AND lease_expires_at_ms IS NOT NULL;

CREATE TABLE orchestrator_job_status_counts (
    status TEXT PRIMARY KEY,
    job_count BIGINT NOT NULL CHECK (job_count >= 0)
);
INSERT INTO orchestrator_job_status_counts(status, job_count) VALUES
    ('QUEUED', 0), ('LEASED', 0), ('RETRY_WAIT', 0), ('CANCEL_REQUESTED', 0),
    ('SUCCEEDED', 0), ('FAILED', 0), ('CANCELLED', 0), ('NEEDS_ATTENTION', 0);
UPDATE orchestrator_job_status_counts AS counts
SET job_count = source.job_count
FROM (SELECT status, COUNT(*)::BIGINT AS job_count FROM orchestrator_jobs GROUP BY status) AS source
WHERE counts.status = source.status;

CREATE TABLE orchestrator_control_plane_anomaly_counters (
    counter_key TEXT PRIMARY KEY,
    counter_value BIGINT NOT NULL CHECK (counter_value >= 0)
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
    generation BIGINT NOT NULL CHECK (generation >= 0),
    started_at_ms BIGINT,
    FOREIGN KEY(operation_id) REFERENCES orchestrator_durable_operations(operation_id)
        ON DELETE CASCADE
);
CREATE INDEX idx_orchestrator_active_operation_anomalies_operation
    ON orchestrator_active_operation_anomalies(operation_id, generation);
"#;

const API_BINDING_SCHEMA: &str = r#"
CREATE TABLE orchestrator_api_bindings (
    binding_id TEXT PRIMARY KEY,
    consumer_deployment_id TEXT NOT NULL,
    provider_deployment_id TEXT NOT NULL DEFAULT '',
    topology_id TEXT NOT NULL DEFAULT '',
    topology_revision_id TEXT NOT NULL DEFAULT '',
    api_id TEXT NOT NULL,
    binding_state TEXT NOT NULL CHECK (binding_state IN ('PENDING', 'RESOLVED', 'ACTIVE', 'UNBOUND', 'REVOKED', 'ERROR')),
    payload JSONB NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    UNIQUE (consumer_deployment_id, api_id, binding_id)
);
CREATE INDEX idx_orchestrator_api_bindings_consumer
    ON orchestrator_api_bindings(consumer_deployment_id, binding_id);
CREATE INDEX idx_orchestrator_api_bindings_provider
    ON orchestrator_api_bindings(provider_deployment_id, binding_id)
    WHERE provider_deployment_id <> '';
CREATE INDEX idx_orchestrator_api_bindings_topology
    ON orchestrator_api_bindings(topology_id, topology_revision_id, binding_id)
    WHERE topology_id <> '';
"#;

const NODE_RUNTIME_FACTS_SCHEMA: &str = r#"
CREATE TABLE orchestrator_node_runtime_facts (
    node_id TEXT PRIMARY KEY,
    observed_at_ms BIGINT NOT NULL CHECK (observed_at_ms >= 0),
    received_at_ms BIGINT NOT NULL CHECK (received_at_ms >= 0),
    payload JSONB NOT NULL
);
CREATE INDEX idx_orchestrator_node_runtime_facts_received
    ON orchestrator_node_runtime_facts(received_at_ms, node_id);
"#;

const API_BINDING_REQUIREMENT_SCHEMA: &str = r#"
ALTER TABLE orchestrator_api_bindings
    ADD COLUMN requirement_name TEXT NOT NULL DEFAULT '';
UPDATE orchestrator_api_bindings
SET requirement_name = payload->>'requirement_name'
WHERE requirement_name = '';
CREATE UNIQUE INDEX idx_orchestrator_api_bindings_consumer_requirement
    ON orchestrator_api_bindings(consumer_deployment_id, requirement_name);
"#;

const MIGRATIONS: &[PostgresMigration] = &[
    PostgresMigration {
        version: 1,
        name: "initial-durable-storage",
        sql: INITIAL_SCHEMA,
    },
    PostgresMigration {
        version: 2,
        name: "topology-revisions-and-runtime-instances",
        sql: TOPOLOGY_REVISION_SCHEMA,
    },
    PostgresMigration {
        version: 3,
        name: "http-idempotency-ledger",
        sql: IDEMPOTENCY_SCHEMA,
    },
    PostgresMigration {
        version: 4,
        name: "durable-operation-coordinator",
        sql: DURABLE_OPERATION_SCHEMA,
    },
    PostgresMigration {
        version: 5,
        name: "append-only-audit-ledger",
        sql: APPEND_ONLY_AUDIT_SCHEMA,
    },
    PostgresMigration {
        version: 6,
        name: "node-enrollment-and-certificate-ledger",
        sql: NODE_IDENTITY_SCHEMA,
    },
    PostgresMigration {
        version: 7,
        name: "legacy-v0.2-import-ledger",
        sql: LEGACY_IMPORT_SCHEMA,
    },
    PostgresMigration {
        version: 8,
        name: "idempotent-node-enrollment-replay",
        sql: ENROLLMENT_REPLAY_SCHEMA,
    },
    PostgresMigration {
        version: 9,
        name: "control-plane-anomaly-and-lease-evidence",
        sql: CONTROL_PLANE_EVIDENCE_SCHEMA,
    },
    PostgresMigration {
        version: 10,
        name: "durable-api-bindings",
        sql: API_BINDING_SCHEMA,
    },
    PostgresMigration {
        version: 11,
        name: "node-runtime-facts",
        sql: NODE_RUNTIME_FACTS_SCHEMA,
    },
    PostgresMigration {
        version: 12,
        name: "api-binding-consumer-requirement-identity",
        sql: API_BINDING_REQUIREMENT_SCHEMA,
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
    "orchestrator_api_bindings",
    "orchestrator_node_runtime_facts",
];

const REQUIRED_INDEXES: &[&str] = &[
    "idx_orchestrator_jobs_lease_recovery",
    "idx_orchestrator_active_operation_anomalies_operation",
    "idx_orchestrator_api_bindings_consumer",
    "idx_orchestrator_api_bindings_provider",
    "idx_orchestrator_api_bindings_topology",
    "idx_orchestrator_node_runtime_facts_received",
    "idx_orchestrator_api_bindings_consumer_requirement",
];

const REQUIRED_TRIGGERS: &[&str] = &["orchestrator_audit_log_no_update_or_delete"];

#[derive(Debug, Clone, Copy)]
struct PostgresMigration {
    version: i32,
    name: &'static str,
    sql: &'static str,
}

pub type RustlsPostgresManager = PostgresConnectionManager<MakeRustlsConnect>;
pub type PooledPostgresConnection = PooledConnection<RustlsPostgresManager>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PostgresTlsTrust {
    Platform,
    CaCertificate(PathBuf),
}

impl PostgresTlsTrust {
    fn label(&self) -> &'static str {
        match self {
            Self::Platform => "platform-verifier",
            Self::CaCertificate(_) => "ca-certificate",
        }
    }
}

#[derive(Debug, Clone)]
pub struct PostgresOptions {
    pub max_size: u32,
    pub min_idle: u32,
    pub connection_timeout: Duration,
    pub statement_timeout: Duration,
    pub lock_timeout: Duration,
    pub idle_in_transaction_timeout: Duration,
    pub advisory_lock_key: i64,
    pub tls_trust: PostgresTlsTrust,
}

impl Default for PostgresOptions {
    fn default() -> Self {
        Self {
            max_size: 16,
            min_idle: 2,
            connection_timeout: Duration::from_secs(5),
            statement_timeout: Duration::from_secs(30),
            lock_timeout: Duration::from_secs(5),
            idle_in_transaction_timeout: Duration::from_secs(30),
            advisory_lock_key: DEFAULT_CONTROL_PLANE_LOCK_KEY,
            tls_trust: PostgresTlsTrust::Platform,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PostgresPreflight {
    pub host_count: usize,
    pub database: String,
    pub user_is_explicit: bool,
    pub tls_trust: String,
    pub max_size: u32,
    pub min_idle: u32,
    pub advisory_lock_key: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PostgresReadinessReport {
    pub database: String,
    pub server_version: String,
    pub tls_enabled: bool,
    pub in_recovery: bool,
    pub pool_connections: u32,
    pub pool_idle_connections: u32,
    pub schema_version: i32,
    pub expected_schema_version: i32,
}

#[derive(Debug, Error)]
pub enum PostgresError {
    #[error("invalid PostgreSQL production configuration: {0}")]
    InvalidConfiguration(String),
    #[error("PostgreSQL TLS configuration failed: {0}")]
    Tls(String),
    #[error("PostgreSQL connection pool failed: {0}")]
    Pool(String),
    #[error("PostgreSQL query failed: {}", postgres_error_display(.0))]
    Database(#[from] PostgresDriverError),
    #[error("another orchestrator control plane already holds advisory lock {0}")]
    AlreadyActive(i64),
    #[error("the PostgreSQL server is a read-only recovery replica")]
    RecoveryReplica,
    #[error("schema migration {version} ({name}) checksum mismatch")]
    MigrationChecksum { version: i32, name: String },
    #[error("schema migration {0} is newer than this binary supports")]
    UnsupportedSchema(i32),
    #[error("required schema object is missing: {0}")]
    MissingSchemaObject(String),
    #[error("json serialization failed: {0}")]
    Json(#[from] serde_json::Error),
    #[error("optimistic concurrency conflict: {0}")]
    Conflict(String),
    #[error("storage invariant failed: {0}")]
    Invariant(String),
    #[error("domain validation failed: {0}")]
    Domain(String),
}

struct PostgresErrorDisplay<'a> {
    driver_error: &'a dyn fmt::Display,
    database_error: Option<&'a dyn fmt::Display>,
}

impl fmt::Display for PostgresErrorDisplay<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.database_error {
            Some(database_error) => database_error.fmt(formatter),
            None => self.driver_error.fmt(formatter),
        }
    }
}

fn postgres_error_display(error: &PostgresDriverError) -> PostgresErrorDisplay<'_> {
    PostgresErrorDisplay {
        driver_error: error,
        database_error: error
            .as_db_error()
            .map(|database_error| database_error as &dyn fmt::Display),
    }
}

pub type PostgresResult<T> = std::result::Result<T, PostgresError>;

#[derive(Clone)]
pub struct PostgresPool {
    pool: Pool<RustlsPostgresManager>,
    options: PostgresOptions,
    preflight: PostgresPreflight,
}

impl fmt::Debug for PostgresPool {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PostgresPool")
            .field("preflight", &self.preflight)
            .field("state", &self.pool.state())
            .finish_non_exhaustive()
    }
}

impl PostgresPool {
    /// Validates fail-closed production requirements without opening a network
    /// connection. The report never includes usernames, passwords or hosts.
    pub fn preflight(
        database_url: &str,
        options: &PostgresOptions,
    ) -> PostgresResult<PostgresPreflight> {
        let (_, report) = validated_config(database_url, options)?;
        Ok(report)
    }

    /// Creates a certificate-verifying TLS pool and immediately probes it.
    pub fn connect(database_url: &str, options: PostgresOptions) -> PostgresResult<Self> {
        let (mut config, preflight) = validated_config(database_url, &options)?;
        if config.get_application_name().is_none() {
            config.application_name("orchestrator-control-plane");
        }
        let tls_config =
            match &options.tls_trust {
                PostgresTlsTrust::Platform => config_platform_verifier()
                    .map_err(|error| PostgresError::Tls(error.to_string()))?,
                PostgresTlsTrust::CaCertificate(path) => config_from_ca_cert(path)
                    .map_err(|error| PostgresError::Tls(error.to_string()))?,
            };
        let manager = PostgresConnectionManager::new(config, MakeRustlsConnect::new(tls_config));
        let pool = Pool::builder()
            .max_size(options.max_size)
            .min_idle(Some(options.min_idle))
            .connection_timeout(options.connection_timeout)
            .test_on_check_out(true)
            .build(manager)
            .map_err(|error| PostgresError::Pool(error.to_string()))?;
        let store = Self {
            pool,
            options,
            preflight,
        };
        store.apply_migrations()?;
        store.readiness()?;
        Ok(store)
    }

    pub fn configuration(&self) -> &PostgresPreflight {
        &self.preflight
    }

    pub fn connection(&self) -> PostgresResult<PooledPostgresConnection> {
        let mut connection = self
            .pool
            .get()
            .map_err(|error| PostgresError::Pool(error.to_string()))?;
        configure_session(&mut connection, &self.options)?;
        Ok(connection)
    }

    pub fn with_client<T>(
        &self,
        action: impl FnOnce(&mut Client) -> PostgresResult<T>,
    ) -> PostgresResult<T> {
        let mut connection = self.connection()?;
        action(&mut connection)
    }

    /// Commits only when the closure succeeds. On an error or unwind, the
    /// transaction rolls back when its guard is dropped.
    pub fn with_transaction<T>(
        &self,
        action: impl FnOnce(&mut Transaction<'_>) -> PostgresResult<T>,
    ) -> PostgresResult<T> {
        let mut connection = self.connection()?;
        let mut transaction = connection.transaction()?;
        let result = action(&mut transaction)?;
        transaction.commit()?;
        Ok(result)
    }

    pub fn readiness(&self) -> PostgresResult<PostgresReadinessReport> {
        let mut connection = self.connection()?;
        verify_migrations(&mut *connection)?;
        verify_schema_objects(&mut *connection)?;
        let row = connection.query_one(
            "SELECT current_database(), current_setting('server_version'), pg_is_in_recovery()",
            &[],
        )?;
        let tls_enabled: bool = connection
            .query_one(
                "SELECT ssl FROM pg_stat_ssl WHERE pid = pg_backend_pid()",
                &[],
            )?
            .get(0);
        let in_recovery: bool = row.get(2);
        if !tls_enabled {
            return Err(PostgresError::InvalidConfiguration(
                "current database session is not using TLS".to_string(),
            ));
        }
        if in_recovery {
            return Err(PostgresError::RecoveryReplica);
        }
        let state = self.pool.state();
        let schema_version: i32 = connection
            .query_one(
                "SELECT COALESCE(MAX(version), 0)::integer FROM orchestrator_schema_migrations",
                &[],
            )?
            .get(0);
        Ok(PostgresReadinessReport {
            database: row.get(0),
            server_version: row.get(1),
            tls_enabled,
            in_recovery,
            pool_connections: state.connections,
            pool_idle_connections: state.idle_connections,
            schema_version,
            expected_schema_version: latest_schema_version(),
        })
    }

    /// Applies only append-only versioned migrations. A transaction-scoped
    /// advisory lock serializes concurrent startup without holding the daemon's
    /// lifetime control-plane lock.
    pub fn apply_migrations(&self) -> PostgresResult<()> {
        let mut connection = self.connection()?;
        let mut transaction = connection.transaction()?;
        transaction.batch_execute(
            "CREATE TABLE IF NOT EXISTS orchestrator_schema_migrations (
                version INTEGER PRIMARY KEY,
                name TEXT NOT NULL,
                checksum TEXT NOT NULL,
                applied_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp()
            );",
        )?;
        transaction.query_one("SELECT pg_advisory_xact_lock($1)", &[&MIGRATION_LOCK_KEY])?;
        verify_migrations(&mut transaction)?;
        for migration in MIGRATIONS {
            let existing = transaction.query_opt(
                "SELECT checksum FROM orchestrator_schema_migrations WHERE version = $1",
                &[&migration.version],
            )?;
            if existing.is_some() {
                continue;
            }
            transaction.batch_execute(migration.sql)?;
            transaction.execute(
                "INSERT INTO orchestrator_schema_migrations(version, name, checksum) VALUES ($1, $2, $3)",
                &[&migration.version, &migration.name, &migration_checksum(migration)],
            )?;
        }
        verify_migrations(&mut transaction)?;
        transaction.commit()?;
        Ok(())
    }

    /// Acquires a session advisory lock on a dedicated pooled connection. The
    /// caller keeps the returned guard alive for the daemon lifetime.
    pub fn acquire_single_active(&self) -> PostgresResult<AdvisoryLockGuard> {
        let mut connection = self.connection()?;
        let key = self.options.advisory_lock_key;
        let acquired: bool = connection
            .query_one("SELECT pg_try_advisory_lock($1)", &[&key])?
            .get(0);
        if !acquired {
            return Err(PostgresError::AlreadyActive(key));
        }
        Ok(AdvisoryLockGuard {
            connection: Some(connection),
            key,
        })
    }
}

pub struct AdvisoryLockGuard {
    connection: Option<PooledPostgresConnection>,
    key: i64,
}

impl fmt::Debug for AdvisoryLockGuard {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AdvisoryLockGuard")
            .field("key", &self.key)
            .field("held", &self.connection.is_some())
            .finish()
    }
}

impl AdvisoryLockGuard {
    pub fn key(&self) -> i64 {
        self.key
    }

    pub fn is_held(&self) -> bool {
        self.connection.is_some()
    }

    pub fn release(mut self) -> PostgresResult<()> {
        self.unlock()
    }

    fn unlock(&mut self) -> PostgresResult<()> {
        let Some(mut connection) = self.connection.take() else {
            return Ok(());
        };
        let unlocked: bool = connection
            .query_one("SELECT pg_advisory_unlock($1)", &[&self.key])?
            .get(0);
        if !unlocked {
            return Err(PostgresError::InvalidConfiguration(
                "advisory lock ownership was lost".to_string(),
            ));
        }
        Ok(())
    }
}

impl Drop for AdvisoryLockGuard {
    fn drop(&mut self) {
        let _ = self.unlock();
    }
}

fn validated_config(
    database_url: &str,
    options: &PostgresOptions,
) -> PostgresResult<(Config, PostgresPreflight)> {
    if database_url.trim().is_empty() {
        return invalid("database URL is required");
    }
    let config = database_url
        .parse::<Config>()
        .map_err(|_| PostgresError::InvalidConfiguration("database URL is invalid".to_string()))?;
    if config.get_ssl_mode() != SslMode::Require {
        return invalid("sslmode=require is mandatory");
    }
    if config.get_hosts().is_empty() {
        return invalid("an explicit TCP host is required");
    }
    if config
        .get_hosts()
        .iter()
        .any(|host| !matches!(host, Host::Tcp(value) if !value.trim().is_empty()))
    {
        return invalid("all PostgreSQL hosts must be non-empty TCP hostnames");
    }
    let database = config
        .get_dbname()
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| {
            PostgresError::InvalidConfiguration("an explicit database name is required".to_string())
        })?
        .to_string();
    if config
        .get_user()
        .is_none_or(|value| value.trim().is_empty())
    {
        return invalid("an explicit database user is required");
    }
    if options.max_size < 2 {
        return invalid(
            "pool max_size must be at least two because the advisory lock owns one connection",
        );
    }
    if options.min_idle > options.max_size {
        return invalid("pool min_idle cannot exceed max_size");
    }
    for (name, timeout) in [
        ("connection_timeout", options.connection_timeout),
        ("statement_timeout", options.statement_timeout),
        ("lock_timeout", options.lock_timeout),
        (
            "idle_in_transaction_timeout",
            options.idle_in_transaction_timeout,
        ),
    ] {
        if timeout.is_zero() {
            return invalid(format!("{name} must be greater than zero"));
        }
        if timeout.as_millis() > i64::MAX as u128 {
            return invalid(format!("{name} is too large"));
        }
    }
    if let PostgresTlsTrust::CaCertificate(path) = &options.tls_trust
        && !path.is_file()
    {
        return invalid("TLS CA certificate file does not exist");
    }
    let host_count = config.get_hosts().len();
    let user_is_explicit = config.get_user().is_some();
    Ok((
        config,
        PostgresPreflight {
            host_count,
            database,
            user_is_explicit,
            tls_trust: options.tls_trust.label().to_string(),
            max_size: options.max_size,
            min_idle: options.min_idle,
            advisory_lock_key: options.advisory_lock_key,
        },
    ))
}

fn configure_session(client: &mut Client, options: &PostgresOptions) -> PostgresResult<()> {
    let statement = duration_ms_text(options.statement_timeout);
    let lock = duration_ms_text(options.lock_timeout);
    let idle = duration_ms_text(options.idle_in_transaction_timeout);
    client.query_one(
        "SELECT set_config('statement_timeout', $1, false), set_config('lock_timeout', $2, false), set_config('idle_in_transaction_session_timeout', $3, false)",
        &[&statement, &lock, &idle],
    )?;
    Ok(())
}

fn verify_migrations(client: &mut impl GenericClient) -> PostgresResult<()> {
    let newest: i32 = client
        .query_one(
            "SELECT COALESCE(MAX(version), 0)::integer FROM orchestrator_schema_migrations",
            &[],
        )?
        .get(0);
    if newest > latest_schema_version() {
        return Err(PostgresError::UnsupportedSchema(newest));
    }
    for migration in MIGRATIONS {
        if let Some(row) = client.query_opt(
            "SELECT name, checksum FROM orchestrator_schema_migrations WHERE version = $1",
            &[&migration.version],
        )? {
            let stored_name: String = row.get(0);
            let stored_checksum: String = row.get(1);
            if stored_name != migration.name || stored_checksum != migration_checksum(migration) {
                return Err(PostgresError::MigrationChecksum {
                    version: migration.version,
                    name: migration.name.to_string(),
                });
            }
        }
    }
    Ok(())
}

fn verify_schema_objects(client: &mut impl GenericClient) -> PostgresResult<()> {
    for table in REQUIRED_TABLES {
        let exists: bool = client
            .query_one("SELECT to_regclass($1) IS NOT NULL", &[table])?
            .get(0);
        if !exists {
            return Err(PostgresError::MissingSchemaObject((*table).to_string()));
        }
    }
    for index in REQUIRED_INDEXES {
        let exists: bool = client
            .query_one("SELECT to_regclass($1) IS NOT NULL", &[index])?
            .get(0);
        if !exists {
            return Err(PostgresError::MissingSchemaObject((*index).to_string()));
        }
    }
    for trigger in REQUIRED_TRIGGERS {
        let exists: bool = client
            .query_one(
                "SELECT EXISTS(SELECT 1 FROM pg_trigger AS audit_trigger JOIN pg_class AS relation ON relation.oid = audit_trigger.tgrelid WHERE audit_trigger.tgname = $1 AND relation.relname = 'orchestrator_audit_log' AND NOT audit_trigger.tgisinternal)",
                &[trigger],
            )?
            .get(0);
        if !exists {
            return Err(PostgresError::MissingSchemaObject((*trigger).to_string()));
        }
    }
    Ok(())
}

fn migration_checksum(migration: &PostgresMigration) -> String {
    format!("sha256:{:x}", Sha256::digest(migration.sql.as_bytes()))
}

fn latest_schema_version() -> i32 {
    MIGRATIONS
        .last()
        .map(|migration| migration.version)
        .unwrap_or(0)
}

fn duration_ms_text(duration: Duration) -> String {
    format!("{}ms", duration.as_millis())
}

fn invalid<T>(message: impl Into<String>) -> PostgresResult<T> {
    Err(PostgresError::InvalidConfiguration(message.into()))
}

#[cfg(test)]
mod tests {
    use super::*;

    const VALID_URL: &str =
        "postgresql://orchestrator:secret@db.example.test:5432/orchestrator?sslmode=require";

    #[test]
    fn production_preflight_requires_tls_and_explicit_database() {
        let options = PostgresOptions::default();
        assert!(
            PostgresPool::preflight(
                "postgresql://orchestrator:secret@db.example.test/orchestrator?sslmode=disable",
                &options,
            )
            .is_err()
        );
        assert!(
            PostgresPool::preflight(
                "postgresql://db.example.test/orchestrator?sslmode=require",
                &options,
            )
            .is_err()
        );
        assert!(
            PostgresPool::preflight(
                "postgresql://orchestrator:secret@db.example.test?sslmode=require",
                &options,
            )
            .is_err()
        );
        assert!(PostgresPool::preflight(VALID_URL, &options).is_ok());
    }

    #[test]
    fn preflight_report_is_credential_free() {
        let report = PostgresPool::preflight(VALID_URL, &PostgresOptions::default())
            .expect("valid production config");
        let debug = format!("{report:?}");
        assert!(!debug.contains("secret"));
        assert_eq!(report.database, "orchestrator");
        assert_eq!(report.host_count, 1);
        assert_eq!(report.tls_trust, "platform-verifier");
        assert_eq!(report.advisory_lock_key, DEFAULT_CONTROL_PLANE_LOCK_KEY);
    }

    #[test]
    fn invalid_pool_and_timeout_options_fail_closed() {
        let options = PostgresOptions {
            max_size: 1,
            ..PostgresOptions::default()
        };
        assert!(PostgresPool::preflight(VALID_URL, &options).is_err());
        let options = PostgresOptions {
            max_size: 2,
            min_idle: 3,
            ..PostgresOptions::default()
        };
        assert!(PostgresPool::preflight(VALID_URL, &options).is_err());
        let options = PostgresOptions {
            min_idle: 1,
            statement_timeout: Duration::ZERO,
            ..PostgresOptions::default()
        };
        assert!(PostgresPool::preflight(VALID_URL, &options).is_err());
    }

    #[test]
    fn ca_trust_requires_an_existing_file() {
        let options = PostgresOptions {
            tls_trust: PostgresTlsTrust::CaCertificate(PathBuf::from("missing-ca.pem")),
            ..PostgresOptions::default()
        };
        assert!(PostgresPool::preflight(VALID_URL, &options).is_err());
    }

    #[test]
    fn postgres_error_display_prefers_database_message_and_detail() {
        let driver_error = "db error";
        let database_error = concat!(
            "ERROR: duplicate key value violates unique constraint ",
            "\"orchestrator_jobs_pkey\"\n",
            "DETAIL: Key (job_id)=(job-1) already exists."
        );

        let display = PostgresErrorDisplay {
            driver_error: &driver_error,
            database_error: Some(&database_error),
        };

        assert_eq!(display.to_string(), database_error);
        assert!(!display.to_string().contains("db error"));
    }

    #[test]
    fn postgres_error_display_keeps_driver_fallback_without_database_source() {
        let driver_error = "error communicating with the server";
        let display = PostgresErrorDisplay {
            driver_error: &driver_error,
            database_error: None,
        };

        assert_eq!(display.to_string(), driver_error);
    }

    #[test]
    fn migrations_are_expand_only_versioned_and_have_stable_checksums() {
        assert_eq!(latest_schema_version(), 12);
        assert_eq!(MIGRATIONS.len(), 12);
        for (index, migration) in MIGRATIONS.iter().enumerate() {
            assert_eq!(migration.version, index as i32 + 1);
        }
        for migration in MIGRATIONS {
            let sql = migration.sql.trim_start();
            assert!(sql.starts_with("CREATE TABLE") || sql.starts_with("ALTER TABLE"));
            assert!(!migration.sql.to_ascii_uppercase().contains("DROP TABLE"));
            let checksum = migration_checksum(migration);
            assert!(checksum.starts_with("sha256:"));
            assert_eq!(checksum.len(), 71);
        }
    }
}
