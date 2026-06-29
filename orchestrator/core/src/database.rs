use crate::{
    DiagnosticReport, Endpoint, Link, LogView, Operation, OperationLock, OperationLogRecord,
    OperationStatus, OrchestratorError, OrchestratorStore, Result, ServiceManifest, ServiceSet,
    Topology, TopologySnapshot, validate_log_view,
};
use postgres::{Client, NoTls, Row, types::ToSql};
use regex::Regex;
use std::collections::HashSet;

pub const ORCHESTRATOR_TABLES: &[&str] = &[
    "services",
    "service_sets",
    "service_endpoints",
    "service_links",
    "orchestrator_operations",
    "orchestrator_operation_logs",
    "orchestrator_operation_locks",
    "topology_snapshots",
    "log_sources",
    "diagnostic_reports",
];

pub const ORCHESTRATOR_DATABASE_STATEMENTS: &[DatabaseStatement] = &[
    DatabaseStatement {
        name: "services.upsert",
        sql: r#"
INSERT INTO services (service_id, name, version, kind, description, manifest, health, updated_at)
VALUES ($1, $2, $3, $4, $5, $6, $7, NOW())
ON CONFLICT (service_id) DO UPDATE SET
    name = EXCLUDED.name,
    version = EXCLUDED.version,
    kind = EXCLUDED.kind,
    description = EXCLUDED.description,
    manifest = EXCLUDED.manifest,
    health = EXCLUDED.health,
    updated_at = NOW()
"#,
    },
    DatabaseStatement {
        name: "services.list",
        sql: "SELECT service_id, name, version, kind, description, manifest, health, created_at, updated_at FROM services ORDER BY service_id",
    },
    DatabaseStatement {
        name: "service_sets.upsert",
        sql: r#"
INSERT INTO service_sets (set_id, name, version, description, sort_order, manifest, updated_at)
VALUES ($1, $2, $3, $4, $5, $6, NOW())
ON CONFLICT (set_id) DO UPDATE SET
    name = EXCLUDED.name,
    version = EXCLUDED.version,
    description = EXCLUDED.description,
    sort_order = EXCLUDED.sort_order,
    manifest = EXCLUDED.manifest,
    updated_at = NOW()
"#,
    },
    DatabaseStatement {
        name: "service_endpoints.upsert",
        sql: r#"
INSERT INTO service_endpoints (endpoint, service_id, protocol, health_path, health, reachable, display_name, note, config, updated_at)
VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, NOW())
ON CONFLICT (endpoint) DO UPDATE SET
    service_id = EXCLUDED.service_id,
    protocol = EXCLUDED.protocol,
    health_path = EXCLUDED.health_path,
    health = EXCLUDED.health,
    reachable = EXCLUDED.reachable,
    display_name = EXCLUDED.display_name,
    note = EXCLUDED.note,
    config = EXCLUDED.config,
    updated_at = NOW()
"#,
    },
    DatabaseStatement {
        name: "service_links.upsert",
        sql: r#"
INSERT INTO service_links (source_endpoint, target_endpoint, protocol, auth_mode, scope, health, latency_ms, config_ref, secret_ref, policy, updated_at)
VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, NOW())
ON CONFLICT (source_endpoint, target_endpoint) DO UPDATE SET
    protocol = EXCLUDED.protocol,
    auth_mode = EXCLUDED.auth_mode,
    scope = EXCLUDED.scope,
    health = EXCLUDED.health,
    latency_ms = EXCLUDED.latency_ms,
    config_ref = EXCLUDED.config_ref,
    secret_ref = EXCLUDED.secret_ref,
    policy = EXCLUDED.policy,
    updated_at = NOW()
"#,
    },
    DatabaseStatement {
        name: "orchestrator_operations.insert",
        sql: r#"
INSERT INTO orchestrator_operations (operation_id, action, target_type, target_id, status, request, plan, rollback_plan, result, error_message, updated_at, confirmed_at, started_at, finished_at, rolled_back_at)
VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, NOW(), $11, $12, $13, $14)
"#,
    },
    DatabaseStatement {
        name: "orchestrator_operations.update_status",
        sql: r#"
UPDATE orchestrator_operations
SET status = $2, result = $3, error_message = $4, updated_at = NOW(), confirmed_at = $5, started_at = $6, finished_at = $7, rolled_back_at = $8
WHERE operation_id = $1
"#,
    },
    DatabaseStatement {
        name: "orchestrator_operation_logs.append",
        sql: "INSERT INTO orchestrator_operation_logs (operation_id, step_id, level, message, data) VALUES ($1, $2, $3, $4, $5)",
    },
    DatabaseStatement {
        name: "orchestrator_operation_locks.acquire",
        sql: r#"
INSERT INTO orchestrator_operation_locks (lock_key, operation_id, owner, expires_at)
VALUES ($1, $2, $3, COALESCE(NULLIF($4, '')::TIMESTAMPTZ, NOW() + INTERVAL '5 minutes'))
ON CONFLICT (lock_key) DO UPDATE SET
    operation_id = EXCLUDED.operation_id,
    owner = EXCLUDED.owner,
    expires_at = EXCLUDED.expires_at
WHERE orchestrator_operation_locks.expires_at < NOW()
"#,
    },
    DatabaseStatement {
        name: "topology_snapshots.insert",
        sql: "INSERT INTO topology_snapshots (snapshot_id, topology) VALUES ($1, $2)",
    },
    DatabaseStatement {
        name: "log_sources.upsert",
        sql: r#"
INSERT INTO log_sources (source_id, endpoint, service_id, operation_id, kind, path, driver, read_policy, updated_at)
VALUES ($1, $2, $3, $4, $5, $6, $7, $8, NOW())
ON CONFLICT (source_id) DO UPDATE SET
    endpoint = EXCLUDED.endpoint,
    service_id = EXCLUDED.service_id,
    operation_id = EXCLUDED.operation_id,
    kind = EXCLUDED.kind,
    path = EXCLUDED.path,
    driver = EXCLUDED.driver,
    read_policy = EXCLUDED.read_policy,
    updated_at = NOW()
"#,
    },
    DatabaseStatement {
        name: "diagnostic_reports.insert",
        sql: "INSERT INTO diagnostic_reports (report_id, operation_id, target_type, target_id, status, summary, data) VALUES ($1, $2, $3, $4, $5, $6, $7)",
    },
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PgOrchestratorStore {
    database_url: String,
}

impl PgOrchestratorStore {
    pub const ENV_NAME: &'static str = "ORCHESTRATOR_DATABASE_URL";

    pub fn from_env() -> Result<Self> {
        let value = std::env::var(Self::ENV_NAME).map_err(|_| {
            OrchestratorError::Dependency(format!("{} is required", Self::ENV_NAME))
        })?;
        Self::new(value)
    }

    pub fn new(database_url: impl Into<String>) -> Result<Self> {
        let database_url = database_url.into();
        if database_url.trim().is_empty() {
            return Err(OrchestratorError::Dependency(
                "ORCHESTRATOR_DATABASE_URL is empty".to_string(),
            ));
        }
        if database_url.contains("OJ_DATABASE_URL") {
            return Err(OrchestratorError::Dependency(
                "PgOrchestratorStore must not use OJ_DATABASE_URL".to_string(),
            ));
        }
        Ok(Self { database_url })
    }

    pub fn database_url(&self) -> &str {
        &self.database_url
    }

    pub fn statements(&self) -> &'static [DatabaseStatement] {
        ORCHESTRATOR_DATABASE_STATEMENTS
    }

    fn connect(&self) -> Result<Client> {
        Client::connect(&self.database_url, NoTls)
            .map_err(|err| OrchestratorError::Dependency(format!("orchestrator db: {err}")))
    }

    fn execute(&self, sql: &str, params: &[&(dyn ToSql + Sync)]) -> Result<u64> {
        let mut client = self.connect()?;
        client
            .execute(sql, params)
            .map_err(|err| OrchestratorError::Dependency(format!("orchestrator db: {err}")))
    }

    fn query(&self, sql: &str, params: &[&(dyn ToSql + Sync)]) -> Result<Vec<Row>> {
        let mut client = self.connect()?;
        client
            .query(sql, params)
            .map_err(|err| OrchestratorError::Dependency(format!("orchestrator db: {err}")))
    }
}

impl OrchestratorStore for PgOrchestratorStore {
    fn list_services(&self) -> Result<Vec<ServiceManifest>> {
        self.query("SELECT manifest FROM services ORDER BY service_id", &[])?
            .into_iter()
            .map(|row| json_model(row.get(0)))
            .collect()
    }

    fn get_service(&self, service_id: &str) -> Result<Option<ServiceManifest>> {
        optional_json_model(self.query(
            "SELECT manifest FROM services WHERE service_id = $1",
            &[&service_id],
        )?)
    }

    fn upsert_service(&mut self, service: ServiceManifest) -> Result<()> {
        let manifest = serde_json::to_value(&service)?;
        let health = service.health.checks.join(",");
        self.execute(
            "INSERT INTO services (service_id, name, version, kind, description, manifest, health, updated_at) VALUES ($1, $2, $3, $4, $5, $6, $7, NOW()) ON CONFLICT (service_id) DO UPDATE SET name = EXCLUDED.name, version = EXCLUDED.version, kind = EXCLUDED.kind, description = EXCLUDED.description, manifest = EXCLUDED.manifest, health = EXCLUDED.health, updated_at = NOW()",
            &[&service.id, &service.name, &service.version, &service.kind, &service.description, &manifest, &health],
        )?;
        Ok(())
    }

    fn delete_service(&mut self, service_id: &str) -> Result<()> {
        let mut client = self.connect()?;
        client
            .execute(
                "DELETE FROM service_links WHERE source_endpoint IN (SELECT endpoint FROM service_endpoints WHERE service_id = $1) OR target_endpoint IN (SELECT endpoint FROM service_endpoints WHERE service_id = $1)",
                &[&service_id],
            )
            .map_err(|err| OrchestratorError::Dependency(format!("orchestrator db: {err}")))?;
        client
            .execute(
                "DELETE FROM log_sources WHERE service_id = $1",
                &[&service_id],
            )
            .map_err(|err| OrchestratorError::Dependency(format!("orchestrator db: {err}")))?;
        client
            .execute(
                "DELETE FROM service_endpoints WHERE service_id = $1",
                &[&service_id],
            )
            .map_err(|err| OrchestratorError::Dependency(format!("orchestrator db: {err}")))?;
        client
            .execute("DELETE FROM services WHERE service_id = $1", &[&service_id])
            .map_err(|err| OrchestratorError::Dependency(format!("orchestrator db: {err}")))?;
        Ok(())
    }

    fn list_sets(&self) -> Result<Vec<ServiceSet>> {
        self.query("SELECT manifest FROM service_sets ORDER BY set_id", &[])?
            .into_iter()
            .map(|row| json_model(row.get(0)))
            .collect()
    }

    fn get_set(&self, set_id: &str) -> Result<Option<ServiceSet>> {
        optional_json_model(self.query(
            "SELECT manifest FROM service_sets WHERE set_id = $1",
            &[&set_id],
        )?)
    }

    fn upsert_set(&mut self, set: ServiceSet) -> Result<()> {
        let manifest = serde_json::to_value(&set)?;
        let version = set.schema_version.to_string();
        self.execute(
            "INSERT INTO service_sets (set_id, name, version, description, sort_order, manifest, updated_at) VALUES ($1, $2, $3, $4, $5, $6, NOW()) ON CONFLICT (set_id) DO UPDATE SET name = EXCLUDED.name, version = EXCLUDED.version, description = EXCLUDED.description, sort_order = EXCLUDED.sort_order, manifest = EXCLUDED.manifest, updated_at = NOW()",
            &[&set.id, &set.name, &version, &set.description, &100i32, &manifest],
        )?;
        Ok(())
    }

    fn delete_set(&mut self, set_id: &str) -> Result<()> {
        self.execute("DELETE FROM service_sets WHERE set_id = $1", &[&set_id])?;
        Ok(())
    }

    fn list_endpoints(&self) -> Result<Vec<Endpoint>> {
        self.query("SELECT endpoint, service_id, protocol, health_path, health, reachable, display_name, note, config, created_at::TEXT, updated_at::TEXT FROM service_endpoints ORDER BY endpoint", &[])?
            .into_iter()
            .map(endpoint_from_row)
            .collect()
    }

    fn get_endpoint(&self, endpoint: &str) -> Result<Option<Endpoint>> {
        let mut rows = self.query("SELECT endpoint, service_id, protocol, health_path, health, reachable, display_name, note, config, created_at::TEXT, updated_at::TEXT FROM service_endpoints WHERE endpoint = $1", &[&endpoint])?;
        rows.pop().map(endpoint_from_row).transpose()
    }

    fn upsert_endpoint(&mut self, endpoint: Endpoint) -> Result<()> {
        self.execute(
            "INSERT INTO service_endpoints (endpoint, service_id, protocol, health_path, health, reachable, display_name, note, config, updated_at) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, NOW()) ON CONFLICT (endpoint) DO UPDATE SET service_id = EXCLUDED.service_id, protocol = EXCLUDED.protocol, health_path = EXCLUDED.health_path, health = EXCLUDED.health, reachable = EXCLUDED.reachable, display_name = EXCLUDED.display_name, note = EXCLUDED.note, config = EXCLUDED.config, updated_at = NOW()",
            &[&endpoint.endpoint, &endpoint.service_id, &endpoint.protocol, &endpoint.health_path, &endpoint.health, &endpoint.reachable, &endpoint.display_name, &endpoint.note, &endpoint.config],
        )?;
        Ok(())
    }

    fn delete_endpoint(&mut self, endpoint: &str) -> Result<()> {
        self.execute(
            "DELETE FROM service_endpoints WHERE endpoint = $1",
            &[&endpoint],
        )?;
        Ok(())
    }

    fn update_endpoint_health(
        &mut self,
        endpoint: &str,
        health: String,
        reachable: bool,
    ) -> Result<()> {
        self.execute("UPDATE service_endpoints SET health = $2, reachable = $3, updated_at = NOW() WHERE endpoint = $1", &[&endpoint, &health, &reachable])?;
        Ok(())
    }

    fn list_links(&self) -> Result<Vec<Link>> {
        self.query("SELECT source_endpoint, target_endpoint, protocol, auth_mode, scope, health, latency_ms, config_ref, secret_ref, policy, created_at::TEXT, updated_at::TEXT FROM service_links ORDER BY source_endpoint, target_endpoint", &[])?
            .into_iter()
            .map(link_from_row)
            .collect()
    }

    fn get_link(&self, source_endpoint: &str, target_endpoint: &str) -> Result<Option<Link>> {
        let mut rows = self.query("SELECT source_endpoint, target_endpoint, protocol, auth_mode, scope, health, latency_ms, config_ref, secret_ref, policy, created_at::TEXT, updated_at::TEXT FROM service_links WHERE source_endpoint = $1 AND target_endpoint = $2", &[&source_endpoint, &target_endpoint])?;
        rows.pop().map(link_from_row).transpose()
    }

    fn upsert_link(&mut self, link: Link) -> Result<()> {
        let latency_ms = link.latency_ms.map(|value| value as i32);
        self.execute(
            "INSERT INTO service_links (source_endpoint, target_endpoint, protocol, auth_mode, scope, health, latency_ms, config_ref, secret_ref, policy, updated_at) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, NOW()) ON CONFLICT (source_endpoint, target_endpoint) DO UPDATE SET protocol = EXCLUDED.protocol, auth_mode = EXCLUDED.auth_mode, scope = EXCLUDED.scope, health = EXCLUDED.health, latency_ms = EXCLUDED.latency_ms, config_ref = EXCLUDED.config_ref, secret_ref = EXCLUDED.secret_ref, policy = EXCLUDED.policy, updated_at = NOW()",
            &[&link.source_endpoint, &link.target_endpoint, &link.protocol, &link.auth_mode, &link.scope, &link.health, &latency_ms, &link.config_ref, &link.secret_ref, &link.policy],
        )?;
        Ok(())
    }

    fn delete_link(&mut self, source_endpoint: &str, target_endpoint: &str) -> Result<()> {
        let changed = self.execute(
            "DELETE FROM service_links WHERE source_endpoint = $1 AND target_endpoint = $2",
            &[&source_endpoint, &target_endpoint],
        )?;
        if changed == 0 {
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
    ) -> Result<()> {
        let latency_ms = latency_ms.map(|value| value as i32);
        self.execute("UPDATE service_links SET health = $3, latency_ms = $4, updated_at = NOW() WHERE source_endpoint = $1 AND target_endpoint = $2", &[&source_endpoint, &target_endpoint, &health, &latency_ms])?;
        Ok(())
    }

    fn create_operation(&mut self, operation: Operation) -> Result<()> {
        self.update_operation(operation)
    }

    fn get_operation(&self, operation_id: &str) -> Result<Option<Operation>> {
        let mut rows = self.query("SELECT action, target_type, target_id, status, request, plan, rollback_plan, result, error_message, created_at::TEXT, updated_at::TEXT, confirmed_at::TEXT, started_at::TEXT, finished_at::TEXT, rolled_back_at::TEXT FROM orchestrator_operations WHERE operation_id = $1", &[&operation_id])?;
        rows.pop()
            .map(|row| operation_from_row(operation_id.to_string(), row))
            .transpose()
    }

    fn list_operations(&self) -> Result<Vec<Operation>> {
        self.query("SELECT operation_id, action, target_type, target_id, status, request, plan, rollback_plan, result, error_message, created_at::TEXT, updated_at::TEXT, confirmed_at::TEXT, started_at::TEXT, finished_at::TEXT, rolled_back_at::TEXT FROM orchestrator_operations ORDER BY created_at DESC", &[])?
            .into_iter()
            .map(|row| operation_from_row(row.get(0), row))
            .collect()
    }

    fn update_operation(&mut self, operation: Operation) -> Result<()> {
        let status = operation_status_text(&operation.status);
        let confirmed_at = db_time_text(&operation.confirmed_at);
        let started_at = db_time_text(&operation.started_at);
        let finished_at = db_time_text(&operation.finished_at);
        let rolled_back_at = db_time_text(&operation.rolled_back_at);
        self.execute(
            "INSERT INTO orchestrator_operations (operation_id, action, target_type, target_id, status, request, plan, rollback_plan, result, error_message, updated_at, confirmed_at, started_at, finished_at, rolled_back_at) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, NOW(), NULLIF($11, '')::TIMESTAMPTZ, NULLIF($12, '')::TIMESTAMPTZ, NULLIF($13, '')::TIMESTAMPTZ, NULLIF($14, '')::TIMESTAMPTZ) ON CONFLICT (operation_id) DO UPDATE SET action = EXCLUDED.action, target_type = EXCLUDED.target_type, target_id = EXCLUDED.target_id, status = EXCLUDED.status, request = EXCLUDED.request, plan = EXCLUDED.plan, rollback_plan = EXCLUDED.rollback_plan, result = EXCLUDED.result, error_message = EXCLUDED.error_message, updated_at = NOW(), confirmed_at = EXCLUDED.confirmed_at, started_at = EXCLUDED.started_at, finished_at = EXCLUDED.finished_at, rolled_back_at = EXCLUDED.rolled_back_at",
            &[&operation.operation_id, &operation.action, &operation.target_type, &operation.target_id, &status, &operation.request, &operation.plan, &operation.rollback_plan, &operation.result, &operation.error_message, &confirmed_at, &started_at, &finished_at, &rolled_back_at],
        )?;
        Ok(())
    }

    fn update_operation_status(
        &mut self,
        operation_id: &str,
        status: OperationStatus,
        error_message: String,
    ) -> Result<()> {
        let status = operation_status_text(&status);
        self.execute("UPDATE orchestrator_operations SET status = $2, error_message = $3, updated_at = NOW() WHERE operation_id = $1", &[&operation_id, &status, &error_message])?;
        Ok(())
    }

    fn update_operation_result(
        &mut self,
        operation_id: &str,
        result: serde_json::Value,
    ) -> Result<()> {
        self.execute("UPDATE orchestrator_operations SET result = $2, updated_at = NOW() WHERE operation_id = $1", &[&operation_id, &result])?;
        Ok(())
    }

    fn append_operation_log(&mut self, record: OperationLogRecord) -> Result<()> {
        self.execute("INSERT INTO orchestrator_operation_logs (operation_id, step_id, level, message, data) VALUES ($1, $2, $3, $4, $5)", &[&record.operation_id, &record.step_id, &record.level, &record.message, &record.data])?;
        Ok(())
    }

    fn list_operation_logs(&self, operation_id: &str) -> Result<Vec<OperationLogRecord>> {
        self.query("SELECT step_id, level, message, data, created_at::TEXT FROM orchestrator_operation_logs WHERE operation_id = $1 ORDER BY created_at, log_id", &[&operation_id])?
            .into_iter()
            .map(|row| Ok(OperationLogRecord {
                operation_id: operation_id.to_string(),
                step_id: row.get(0),
                level: row.get(1),
                message: row.get(2),
                data: row.get(3),
                redacted: false,
                created_at: row.get(4),
            }))
            .collect()
    }

    fn acquire_operation_lock(&mut self, lock: OperationLock) -> Result<bool> {
        let expires_at = db_time_text(&lock.expires_at);
        let changed = self.execute(
            "INSERT INTO orchestrator_operation_locks (lock_key, operation_id, owner, expires_at) VALUES ($1, $2, $3, COALESCE(NULLIF($4, '')::TIMESTAMPTZ, NOW() + INTERVAL '5 minutes')) ON CONFLICT (lock_key) DO UPDATE SET operation_id = EXCLUDED.operation_id, owner = EXCLUDED.owner, expires_at = EXCLUDED.expires_at WHERE orchestrator_operation_locks.expires_at < NOW()",
            &[&lock.lock_key, &lock.operation_id, &lock.owner, &expires_at],
        )?;
        Ok(changed > 0)
    }

    fn release_operation_lock(&mut self, lock_key: &str, operation_id: &str) -> Result<()> {
        self.execute(
            "DELETE FROM orchestrator_operation_locks WHERE lock_key = $1 AND operation_id = $2",
            &[&lock_key, &operation_id],
        )?;
        Ok(())
    }

    fn save_topology_snapshot(&mut self, snapshot: TopologySnapshot) -> Result<()> {
        let topology = serde_json::to_value(&snapshot.topology)?;
        self.execute("INSERT INTO topology_snapshots (snapshot_id, topology) VALUES ($1, $2) ON CONFLICT (snapshot_id) DO UPDATE SET topology = EXCLUDED.topology, created_at = NOW()", &[&snapshot.snapshot_id, &topology])?;
        Ok(())
    }

    fn get_latest_topology_snapshot(&self) -> Result<Option<TopologySnapshot>> {
        let mut rows = self.query("SELECT snapshot_id, topology, created_at::TEXT FROM topology_snapshots ORDER BY created_at DESC LIMIT 1", &[])?;
        rows.pop().map(topology_snapshot_from_row).transpose()
    }

    fn build_topology_view(&self) -> Result<Topology> {
        self.get_latest_topology_snapshot()?
            .map(|snapshot| snapshot.topology)
            .ok_or_else(|| OrchestratorError::Dependency("topology snapshot not found".to_string()))
    }

    fn delete_topology(&mut self, root_endpoint: &str) -> Result<()> {
        self.execute(
            "DELETE FROM topology_snapshots WHERE topology->>'root_endpoint' = $1",
            &[&root_endpoint],
        )?;
        Ok(())
    }

    fn list_log_sources(&self) -> Result<Vec<LogView>> {
        self.query("SELECT source_id, service_id, endpoint, operation_id, path, driver, read_policy, created_at::TEXT FROM log_sources ORDER BY source_id", &[])?
            .into_iter()
            .map(log_view_from_row)
            .collect()
    }

    fn upsert_log_source(&mut self, log_view: LogView) -> Result<()> {
        validate_log_view(&log_view)?;
        self.execute("INSERT INTO log_sources (source_id, endpoint, service_id, operation_id, kind, path, driver, read_policy, updated_at) VALUES ($1, $2, $3, $4, 'service', $5, $6, $7, NOW()) ON CONFLICT (source_id) DO UPDATE SET endpoint = EXCLUDED.endpoint, service_id = EXCLUDED.service_id, operation_id = EXCLUDED.operation_id, kind = EXCLUDED.kind, path = EXCLUDED.path, driver = EXCLUDED.driver, read_policy = EXCLUDED.read_policy, updated_at = NOW()", &[&log_view.source_id, &log_view.endpoint, &log_view.service_id, &log_view.operation_id, &log_view.path, &log_view.driver, &log_view.read_policy])?;
        Ok(())
    }

    fn delete_log_source(&mut self, source_id: &str) -> Result<()> {
        self.execute(
            "DELETE FROM log_sources WHERE source_id = $1",
            &[&source_id],
        )?;
        Ok(())
    }

    fn create_diagnostic_report(&mut self, report: DiagnosticReport) -> Result<()> {
        self.execute("INSERT INTO diagnostic_reports (report_id, operation_id, target_type, target_id, status, summary, data) VALUES ($1, $2, $3, $4, $5, $6, $7) ON CONFLICT (report_id) DO UPDATE SET operation_id = EXCLUDED.operation_id, target_type = EXCLUDED.target_type, target_id = EXCLUDED.target_id, status = EXCLUDED.status, summary = EXCLUDED.summary, data = EXCLUDED.data", &[&report.report_id, &report.operation_id, &report.target_type, &report.target_id, &report.status, &report.summary, &report.data])?;
        Ok(())
    }

    fn get_diagnostic_report(&self, report_id: &str) -> Result<Option<DiagnosticReport>> {
        let mut rows = self.query("SELECT report_id, operation_id, target_type, target_id, status, summary, data, created_at::TEXT FROM diagnostic_reports WHERE report_id = $1", &[&report_id])?;
        rows.pop().map(diagnostic_from_row).transpose()
    }

    fn list_diagnostic_reports(&self) -> Result<Vec<DiagnosticReport>> {
        self.query("SELECT report_id, operation_id, target_type, target_id, status, summary, data, created_at::TEXT FROM diagnostic_reports ORDER BY created_at DESC", &[])?
            .into_iter()
            .map(diagnostic_from_row)
            .collect()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DatabaseSchemaReport {
    pub tables: Vec<String>,
    pub missing_tables: Vec<String>,
    pub non_formal_tables: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DatabaseStatement {
    pub name: &'static str,
    pub sql: &'static str,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DatabaseAccessReport {
    pub statement_count: usize,
    pub touched_tables: Vec<String>,
    pub missing_tables: Vec<String>,
    pub non_formal_tables: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DatabaseWritePlan {
    pub writes: Vec<DatabaseWrite>,
    pub touched_tables: Vec<String>,
    pub non_formal_tables: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DatabaseWrite {
    pub object_type: String,
    pub object_id: String,
    pub table: String,
    pub statement: String,
}

pub fn inspect_orchestrator_schema(sql: &str) -> Result<DatabaseSchemaReport> {
    let table_re =
        Regex::new(r"(?i)CREATE\s+TABLE\s+(?:IF\s+NOT\s+EXISTS\s+)?([a-zA-Z_][a-zA-Z0-9_]*)")
            .expect("valid table regex");
    let mut tables = table_re
        .captures_iter(sql)
        .filter_map(|captures| captures.get(1).map(|item| item.as_str().to_string()))
        .collect::<Vec<_>>();
    tables.sort();
    tables.dedup();

    let table_set = tables.iter().map(String::as_str).collect::<HashSet<_>>();
    let formal_table_set = ORCHESTRATOR_TABLES.iter().copied().collect::<HashSet<_>>();
    let missing_tables = ORCHESTRATOR_TABLES
        .iter()
        .copied()
        .filter(|table| !table_set.contains(*table))
        .map(str::to_string)
        .collect::<Vec<_>>();
    let non_formal_tables = tables
        .iter()
        .filter(|table| !formal_table_set.contains(table.as_str()))
        .cloned()
        .collect::<Vec<_>>();

    if !missing_tables.is_empty() {
        return Err(OrchestratorError::Dependency(format!(
            "orchestrator schema is missing tables: {}",
            missing_tables.join(", ")
        )));
    }
    if !non_formal_tables.is_empty() {
        return Err(OrchestratorError::Dependency(format!(
            "orchestrator schema contains non-formal tables: {}",
            non_formal_tables.join(", ")
        )));
    }

    Ok(DatabaseSchemaReport {
        tables,
        missing_tables,
        non_formal_tables,
    })
}

fn json_model<T: serde::de::DeserializeOwned>(value: serde_json::Value) -> Result<T> {
    serde_json::from_value(value).map_err(OrchestratorError::Json)
}

fn optional_json_model<T: serde::de::DeserializeOwned>(mut rows: Vec<Row>) -> Result<Option<T>> {
    rows.pop()
        .map(|row| json_model(row.get::<usize, serde_json::Value>(0)))
        .transpose()
}

fn endpoint_from_row(row: Row) -> Result<Endpoint> {
    Ok(Endpoint {
        endpoint: row.get(0),
        service_id: row.get(1),
        protocol: row.get(2),
        health_path: row.get(3),
        health: row.get(4),
        reachable: row.get(5),
        display_name: row.get(6),
        note: row.get(7),
        config: row.get(8),
        created_at: row.get(9),
        updated_at: row.get(10),
    })
}

fn link_from_row(row: Row) -> Result<Link> {
    let latency_ms = row.get::<usize, Option<i32>>(6).map(|value| value as u32);
    Ok(Link {
        source_endpoint: row.get(0),
        target_endpoint: row.get(1),
        protocol: row.get(2),
        auth_mode: row.get(3),
        scope: row.get(4),
        health: row.get(5),
        latency_ms,
        config_ref: row.get(7),
        secret_ref: row.get(8),
        policy: row.get(9),
        created_at: row.get(10),
        updated_at: row.get(11),
    })
}

fn operation_from_row(operation_id: String, row: Row) -> Result<Operation> {
    let offset = if row.len() == 16 { 1 } else { 0 };
    let status_text: String = row.get(3 + offset);
    Ok(Operation {
        operation_id,
        action: row.get(0 + offset),
        target_type: row.get(1 + offset),
        target_id: row.get(2 + offset),
        status: operation_status_from_text(&status_text),
        request: row.get(4 + offset),
        plan: row.get(5 + offset),
        rollback_plan: row.get(6 + offset),
        result: row.get(7 + offset),
        error_message: row.get(8 + offset),
        created_at: row.get(9 + offset),
        updated_at: row.get(10 + offset),
        confirmed_at: optional_time_text(&row, 11 + offset),
        started_at: optional_time_text(&row, 12 + offset),
        finished_at: optional_time_text(&row, 13 + offset),
        rolled_back_at: optional_time_text(&row, 14 + offset),
    })
}

fn topology_snapshot_from_row(row: Row) -> Result<TopologySnapshot> {
    Ok(TopologySnapshot {
        snapshot_id: row.get(0),
        topology: json_model(row.get(1))?,
        created_at: row.get(2),
    })
}

fn log_view_from_row(row: Row) -> Result<LogView> {
    Ok(LogView {
        source_id: row.get(0),
        service_id: row.get(1),
        endpoint: row.get(2),
        operation_id: row.get(3),
        path: row.get(4),
        driver: row.get(5),
        read_policy: row.get(6),
        display_name: row.get(0),
    })
}

fn diagnostic_from_row(row: Row) -> Result<DiagnosticReport> {
    Ok(DiagnosticReport {
        report_id: row.get(0),
        operation_id: row.get(1),
        target_type: row.get(2),
        target_id: row.get(3),
        status: row.get(4),
        summary: row.get(5),
        data: row.get(6),
        findings: Vec::new(),
        created_at: row.get(7),
    })
}

fn operation_status_text(status: &OperationStatus) -> String {
    match status {
        OperationStatus::Planned => "PLANNED",
        OperationStatus::AwaitingConfirmation => "AWAITING_CONFIRMATION",
        OperationStatus::Running => "RUNNING",
        OperationStatus::Succeeded => "SUCCEEDED",
        OperationStatus::Failed => "FAILED",
        OperationStatus::RolledBack => "ROLLED_BACK",
        OperationStatus::Cancelled => "CANCELLED",
        OperationStatus::Expired => "EXPIRED",
    }
    .to_string()
}

fn operation_status_from_text(value: &str) -> OperationStatus {
    match value {
        "AWAITING_CONFIRMATION" => OperationStatus::AwaitingConfirmation,
        "RUNNING" => OperationStatus::Running,
        "SUCCEEDED" => OperationStatus::Succeeded,
        "FAILED" => OperationStatus::Failed,
        "ROLLED_BACK" => OperationStatus::RolledBack,
        "CANCELLED" => OperationStatus::Cancelled,
        "EXPIRED" => OperationStatus::Expired,
        _ => OperationStatus::Planned,
    }
}

fn optional_time_text(row: &Row, index: usize) -> String {
    row.get::<usize, Option<String>>(index).unwrap_or_default()
}

pub(crate) fn db_time_text(value: &str) -> String {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        String::new()
    } else if matches!(
        trimmed,
        "planned"
            | "confirmed"
            | "started"
            | "finished"
            | "failed"
            | "rolled_back"
            | "cancelled"
            | "expired"
    ) {
        "now".to_string()
    } else if trimmed.contains('T') || trimmed.contains('-') {
        trimmed.to_string()
    } else {
        String::new()
    }
}

pub fn plan_database_writes<S: OrchestratorStore>(store: &S) -> Result<DatabaseWritePlan> {
    let mut writes = Vec::new();
    for service in store.services()? {
        writes.push(database_write(
            "Service",
            service.id,
            "services",
            "services.upsert",
        ));
    }
    for set in store.sets()? {
        writes.push(database_write(
            "Set",
            set.id,
            "service_sets",
            "service_sets.upsert",
        ));
    }
    for endpoint in store.endpoints()? {
        writes.push(database_write(
            "Endpoint",
            endpoint.endpoint,
            "service_endpoints",
            "service_endpoints.upsert",
        ));
    }
    for link in store.links()? {
        writes.push(database_write(
            "Link",
            format!("{} -> {}", link.source_endpoint, link.target_endpoint),
            "service_links",
            "service_links.upsert",
        ));
    }
    for operation in store.operations()? {
        writes.push(database_write(
            "Operation",
            operation.operation_id.clone(),
            "orchestrator_operations",
            "orchestrator_operations.insert",
        ));
        for (index, _record) in store
            .operation_logs(&operation.operation_id)?
            .iter()
            .enumerate()
        {
            writes.push(database_write(
                "OperationLog",
                format!("{}#{}", operation.operation_id, index + 1),
                "orchestrator_operation_logs",
                "orchestrator_operation_logs.append",
            ));
        }
    }
    if let Some(snapshot) = store.get_latest_topology_snapshot()? {
        writes.push(database_write(
            "Topology",
            snapshot.snapshot_id,
            "topology_snapshots",
            "topology_snapshots.insert",
        ));
    }
    for log_view in store.log_views()? {
        writes.push(database_write(
            "LogView",
            log_view.source_id,
            "log_sources",
            "log_sources.upsert",
        ));
    }
    for report in store.diagnostic_reports()? {
        writes.push(database_write(
            "DiagnosticReport",
            report.report_id,
            "diagnostic_reports",
            "diagnostic_reports.insert",
        ));
    }

    let mut touched_tables = writes
        .iter()
        .map(|write| write.table.clone())
        .collect::<Vec<_>>();
    touched_tables.push("orchestrator_operation_locks".to_string());
    touched_tables.sort();
    touched_tables.dedup();

    let non_formal_tables = touched_tables
        .iter()
        .filter(|table| !is_formal_table(table))
        .cloned()
        .collect::<Vec<_>>();
    if !non_formal_tables.is_empty() {
        return Err(OrchestratorError::Dependency(format!(
            "database write plan touches non-formal tables: {}",
            non_formal_tables.join(", ")
        )));
    }
    let statement_names = ORCHESTRATOR_DATABASE_STATEMENTS
        .iter()
        .map(|statement| statement.name)
        .collect::<HashSet<_>>();
    let missing_statements = writes
        .iter()
        .filter(|write| !statement_names.contains(write.statement.as_str()))
        .map(|write| write.statement.clone())
        .collect::<Vec<_>>();
    if !missing_statements.is_empty() {
        return Err(OrchestratorError::Dependency(format!(
            "database write plan references missing statements: {}",
            missing_statements.join(", ")
        )));
    }

    Ok(DatabaseWritePlan {
        writes,
        touched_tables,
        non_formal_tables,
    })
}

pub fn inspect_database_access(statements: &[DatabaseStatement]) -> Result<DatabaseAccessReport> {
    let mut touched_tables = statements
        .iter()
        .flat_map(|statement| sql_table_references(statement.sql))
        .collect::<Vec<_>>();
    touched_tables.sort();
    touched_tables.dedup();

    let table_set = touched_tables
        .iter()
        .map(String::as_str)
        .collect::<HashSet<_>>();
    let missing_tables = ORCHESTRATOR_TABLES
        .iter()
        .copied()
        .filter(|table| !table_set.contains(*table))
        .map(str::to_string)
        .collect::<Vec<_>>();
    let non_formal_tables = touched_tables
        .iter()
        .filter(|table| !is_formal_table(table))
        .cloned()
        .collect::<Vec<_>>();

    if !missing_tables.is_empty() {
        return Err(OrchestratorError::Dependency(format!(
            "orchestrator database access is missing tables: {}",
            missing_tables.join(", ")
        )));
    }
    if !non_formal_tables.is_empty() {
        return Err(OrchestratorError::Dependency(format!(
            "orchestrator database access touches non-formal tables: {}",
            non_formal_tables.join(", ")
        )));
    }

    Ok(DatabaseAccessReport {
        statement_count: statements.len(),
        touched_tables,
        missing_tables,
        non_formal_tables,
    })
}

fn sql_table_references(sql: &str) -> Vec<String> {
    let table_re = Regex::new(r"(?i)\b(?:FROM|INTO|UPDATE|JOIN)\s+([a-zA-Z_][a-zA-Z0-9_]*)")
        .expect("valid table reference regex");
    table_re
        .captures_iter(sql)
        .filter_map(|captures| captures.get(1).map(|item| item.as_str().to_string()))
        .filter(|table| !matches!(table.to_ascii_lowercase().as_str(), "set"))
        .collect()
}

fn is_formal_table(table: &str) -> bool {
    ORCHESTRATOR_TABLES.contains(&table)
}

fn database_write(
    object_type: impl Into<String>,
    object_id: impl Into<String>,
    table: impl Into<String>,
    statement: impl Into<String>,
) -> DatabaseWrite {
    DatabaseWrite {
        object_type: object_type.into(),
        object_id: object_id.into(),
        table: table.into(),
        statement: statement.into(),
    }
}
