use crate::{
    DeployedServiceApi, DiagnosticReport, Endpoint, HostService, Link, LogView, NodeRecord,
    Operation, OperationLock, OperationLogRecord, OperationStatus, OrchestratorError,
    OrchestratorStore, RenderedServiceConfig, Result, ServiceApiSurface, ServiceFrontendEntry,
    ServiceManifest, ServiceMigrationRecord, ServicePermissionRecord, ServiceRedisResource,
    ServiceRelease, ServiceRoute, ServiceStorageResource, Topology, TopologySnapshot,
    build_topology, parse_endpoint_id, validate_deployed_service_api, validate_endpoint,
    validate_endpoint_id, validate_host_service, validate_link, validate_log_view,
    validate_node_record, validate_rendered_service_config, validate_service_api_surface,
    validate_service_frontend_entry, validate_service_manifest, validate_service_migration_record,
    validate_service_permission_record, validate_service_redis_resource,
    validate_service_release_record, validate_service_route, validate_service_storage_resource,
    validate_topology,
};
use postgres::{Client, NoTls, Row, types::ToSql};
use regex::Regex;
use std::collections::{BTreeMap, BTreeSet, HashSet};

pub const ORCHESTRATOR_TABLES: &[&str] = &[
    "service_releases",
    "host_services",
    "services",
    "service_endpoints",
    "service_links",
    "service_routes",
    "service_migration_records",
    "service_permission_records",
    "service_frontend_entries",
    "service_redis_resources",
    "service_storage_resources",
    "rendered_service_configs",
    "nodes",
    "service_api_surfaces",
    "deployed_service_apis",
    "orchestrator_operations",
    "orchestrator_operation_logs",
    "orchestrator_operation_locks",
    "topology_snapshots",
    "log_sources",
    "diagnostic_reports",
];

pub const ORCHESTRATOR_DATABASE_STATEMENTS: &[DatabaseStatement] = &[
    DatabaseStatement {
        name: "service_releases.upsert",
        sql: r#"
INSERT INTO service_releases (service_name, version, release_url, manifest, checksum)
VALUES ($1, $2, $3, $4, $5)
ON CONFLICT (service_name, version) DO UPDATE SET
    release_url = EXCLUDED.release_url,
    manifest = EXCLUDED.manifest,
    checksum = EXCLUDED.checksum
"#,
    },
    DatabaseStatement {
        name: "host_services.upsert",
        sql: r#"
INSERT INTO host_services (host_ip, service_name, version, status, config, labels, updated_at)
VALUES ($1, $2, $3, $4, $5, $6, NOW())
ON CONFLICT (host_ip, service_name) DO UPDATE SET
    version = EXCLUDED.version,
    status = EXCLUDED.status,
    config = EXCLUDED.config,
    labels = EXCLUDED.labels,
    updated_at = NOW()
"#,
    },
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
        name: "service_endpoints.upsert",
        sql: r#"
INSERT INTO service_endpoints (endpoint, service_id, ip, port, service_name, host_ip, protocol, health_path, health, reachable, display_name, note, config, updated_at)
VALUES ($1, $2, $3, $4, $5, $3, $6, $7, $8, $9, $10, $11, $12, NOW())
ON CONFLICT (endpoint) DO UPDATE SET
    service_id = EXCLUDED.service_id,
    ip = EXCLUDED.ip,
    port = EXCLUDED.port,
    service_name = EXCLUDED.service_name,
    host_ip = EXCLUDED.host_ip,
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
INSERT INTO service_links (source_endpoint, target_endpoint, from_ip, from_port, from_service_name, to_type, to_ip, to_port, to_service_name, protocol, auth_mode, scope, health, latency_ms, config_ref, secret_ref, policy, updated_at)
VALUES ($1, $2, $3, $4, $5, 'endpoint', $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, NOW())
ON CONFLICT (source_endpoint, target_endpoint) DO UPDATE SET
    from_ip = EXCLUDED.from_ip,
    from_port = EXCLUDED.from_port,
    from_service_name = EXCLUDED.from_service_name,
    to_type = EXCLUDED.to_type,
    to_ip = EXCLUDED.to_ip,
    to_port = EXCLUDED.to_port,
    to_service_name = EXCLUDED.to_service_name,
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
        name: "service_routes.upsert",
        sql: r#"
INSERT INTO service_routes (path, method, target_type, target_service_name, target_selector, permission, enabled, updated_at)
VALUES ($1, $2, $3, $4, $5, $6, $7, NOW())
ON CONFLICT (path, method) DO UPDATE SET
    target_type = EXCLUDED.target_type,
    target_service_name = EXCLUDED.target_service_name,
    target_selector = EXCLUDED.target_selector,
    permission = EXCLUDED.permission,
    enabled = EXCLUDED.enabled,
    updated_at = NOW()
"#,
    },
    DatabaseStatement {
        name: "service_migration_records.upsert",
        sql: r#"
INSERT INTO service_migration_records (service_name, migration_version, checksum, status, applied_at, updated_at)
VALUES ($1, $2, $3, $4, NULLIF($5, '')::TIMESTAMPTZ, NOW())
ON CONFLICT (service_name, migration_version) DO UPDATE SET
    checksum = EXCLUDED.checksum,
    status = EXCLUDED.status,
    applied_at = EXCLUDED.applied_at,
    updated_at = NOW()
"#,
    },
    DatabaseStatement {
        name: "service_permission_records.upsert",
        sql: r#"
INSERT INTO service_permission_records (service_name, permission_key, source, updated_at)
VALUES ($1, $2, $3, NOW())
ON CONFLICT (service_name, permission_key) DO UPDATE SET
    source = EXCLUDED.source,
    updated_at = NOW()
"#,
    },
    DatabaseStatement {
        name: "service_frontend_entries.upsert",
        sql: r#"
INSERT INTO service_frontend_entries (service_name, enabled, route_prefix, remote_entry, menu_items, updated_at)
VALUES ($1, $2, $3, $4, $5, NOW())
ON CONFLICT (service_name) DO UPDATE SET
    enabled = EXCLUDED.enabled,
    route_prefix = EXCLUDED.route_prefix,
    remote_entry = EXCLUDED.remote_entry,
    menu_items = EXCLUDED.menu_items,
    updated_at = NOW()
"#,
    },
    DatabaseStatement {
        name: "service_redis_resources.upsert",
        sql: r#"
INSERT INTO service_redis_resources (service_name, name, kind, usage, updated_at)
VALUES ($1, $2, $3, $4, NOW())
ON CONFLICT (service_name, name) DO UPDATE SET
    kind = EXCLUDED.kind,
    usage = EXCLUDED.usage,
    updated_at = NOW()
"#,
    },
    DatabaseStatement {
        name: "service_storage_resources.upsert",
        sql: r#"
INSERT INTO service_storage_resources (service_name, object_type, bucket, path_prefix, updated_at)
VALUES ($1, $2, $3, $4, NOW())
ON CONFLICT (service_name, object_type, bucket) DO UPDATE SET
    path_prefix = EXCLUDED.path_prefix,
    updated_at = NOW()
"#,
    },
    DatabaseStatement {
        name: "rendered_service_configs.upsert",
        sql: r#"
INSERT INTO rendered_service_configs (service_name, version, config, updated_at)
VALUES ($1, $2, $3, NOW())
ON CONFLICT (service_name, version) DO UPDATE SET
    config = EXCLUDED.config,
    updated_at = NOW()
"#,
    },
    DatabaseStatement {
        name: "nodes.upsert",
        sql: r#"
INSERT INTO nodes (node_id, host_ip, parent_node_id, role, labels, status, updated_at)
VALUES ($1, $2, $3, $4, $5, $6, NOW())
ON CONFLICT (node_id) DO UPDATE SET
    host_ip = EXCLUDED.host_ip,
    parent_node_id = EXCLUDED.parent_node_id,
    role = EXCLUDED.role,
    labels = EXCLUDED.labels,
    status = EXCLUDED.status,
    updated_at = NOW()
"#,
    },
    DatabaseStatement {
        name: "service_api_surfaces.upsert",
        sql: r#"
INSERT INTO service_api_surfaces (service_name, version, api_id, protocol, port_name, path_prefix, methods, visibility, auth_mode, permission, stability, api_version, rate_limit, timeout, config, updated_at)
VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, NOW())
ON CONFLICT (service_name, version, api_id) DO UPDATE SET
    protocol = EXCLUDED.protocol,
    port_name = EXCLUDED.port_name,
    path_prefix = EXCLUDED.path_prefix,
    methods = EXCLUDED.methods,
    visibility = EXCLUDED.visibility,
    auth_mode = EXCLUDED.auth_mode,
    permission = EXCLUDED.permission,
    stability = EXCLUDED.stability,
    api_version = EXCLUDED.api_version,
    rate_limit = EXCLUDED.rate_limit,
    timeout = EXCLUDED.timeout,
    config = EXCLUDED.config,
    updated_at = NOW()
"#,
    },
    DatabaseStatement {
        name: "deployed_service_apis.upsert",
        sql: r#"
INSERT INTO deployed_service_apis (host_ip, service_name, version, endpoint, api_id, status, updated_at)
VALUES ($1, $2, $3, $4, $5, $6, NOW())
ON CONFLICT (host_ip, service_name, api_id, endpoint) DO UPDATE SET
    version = EXCLUDED.version,
    status = EXCLUDED.status,
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
        validate_service_manifest(&service)?;
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
                "DELETE FROM host_services WHERE service_name = $1",
                &[&service_id],
            )
            .map_err(|err| OrchestratorError::Dependency(format!("orchestrator db: {err}")))?;
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
            .execute(
                "DELETE FROM service_routes WHERE target_service_name = $1",
                &[&service_id],
            )
            .map_err(|err| OrchestratorError::Dependency(format!("orchestrator db: {err}")))?;
        client
            .execute(
                "DELETE FROM service_migration_records WHERE service_name = $1",
                &[&service_id],
            )
            .map_err(|err| OrchestratorError::Dependency(format!("orchestrator db: {err}")))?;
        client
            .execute(
                "DELETE FROM service_permission_records WHERE service_name = $1",
                &[&service_id],
            )
            .map_err(|err| OrchestratorError::Dependency(format!("orchestrator db: {err}")))?;
        client
            .execute(
                "DELETE FROM service_frontend_entries WHERE service_name = $1",
                &[&service_id],
            )
            .map_err(|err| OrchestratorError::Dependency(format!("orchestrator db: {err}")))?;
        client
            .execute(
                "DELETE FROM service_redis_resources WHERE service_name = $1",
                &[&service_id],
            )
            .map_err(|err| OrchestratorError::Dependency(format!("orchestrator db: {err}")))?;
        client
            .execute(
                "DELETE FROM service_storage_resources WHERE service_name = $1",
                &[&service_id],
            )
            .map_err(|err| OrchestratorError::Dependency(format!("orchestrator db: {err}")))?;
        client
            .execute(
                "DELETE FROM rendered_service_configs WHERE service_name = $1",
                &[&service_id],
            )
            .map_err(|err| OrchestratorError::Dependency(format!("orchestrator db: {err}")))?;
        client
            .execute(
                "DELETE FROM deployed_service_apis WHERE service_name = $1",
                &[&service_id],
            )
            .map_err(|err| OrchestratorError::Dependency(format!("orchestrator db: {err}")))?;
        client
            .execute(
                "DELETE FROM service_api_surfaces WHERE service_name = $1",
                &[&service_id],
            )
            .map_err(|err| OrchestratorError::Dependency(format!("orchestrator db: {err}")))?;
        client
            .execute(
                "DELETE FROM service_releases WHERE service_name = $1",
                &[&service_id],
            )
            .map_err(|err| OrchestratorError::Dependency(format!("orchestrator db: {err}")))?;
        client
            .execute("DELETE FROM services WHERE service_id = $1", &[&service_id])
            .map_err(|err| OrchestratorError::Dependency(format!("orchestrator db: {err}")))?;
        Ok(())
    }

    fn list_host_services(&self) -> Result<Vec<HostService>> {
        self.query("SELECT host_ip, service_name, version, status, config, labels, created_at::TEXT, updated_at::TEXT FROM host_services ORDER BY host_ip, service_name", &[])?
            .into_iter()
            .map(host_service_from_row)
            .collect()
    }

    fn get_host_service(&self, host_ip: &str, service_name: &str) -> Result<Option<HostService>> {
        let mut rows = self.query("SELECT host_ip, service_name, version, status, config, labels, created_at::TEXT, updated_at::TEXT FROM host_services WHERE host_ip = $1 AND service_name = $2", &[&host_ip, &service_name])?;
        rows.pop().map(host_service_from_row).transpose()
    }

    fn upsert_host_service(&mut self, host_service: HostService) -> Result<()> {
        validate_host_service(&host_service)?;
        self.execute(
            "INSERT INTO host_services (host_ip, service_name, version, status, config, labels, updated_at) VALUES ($1, $2, $3, $4, $5, $6, NOW()) ON CONFLICT (host_ip, service_name) DO UPDATE SET version = EXCLUDED.version, status = EXCLUDED.status, config = EXCLUDED.config, labels = EXCLUDED.labels, updated_at = NOW()",
            &[&host_service.host_ip, &host_service.service_name, &host_service.version, &host_service.status, &host_service.config, &host_service.labels],
        )?;
        Ok(())
    }

    fn delete_host_service(&mut self, host_ip: &str, service_name: &str) -> Result<()> {
        self.execute(
            "DELETE FROM host_services WHERE host_ip = $1 AND service_name = $2",
            &[&host_ip, &service_name],
        )?;
        Ok(())
    }

    fn delete_host_services_for_service(&mut self, service_name: &str) -> Result<()> {
        self.execute(
            "DELETE FROM host_services WHERE service_name = $1",
            &[&service_name],
        )?;
        Ok(())
    }

    fn list_service_releases(&self) -> Result<Vec<ServiceRelease>> {
        self.query("SELECT service_name, version, release_url, manifest, checksum, created_at::TEXT FROM service_releases ORDER BY service_name, version", &[])?
            .into_iter()
            .map(service_release_from_row)
            .collect()
    }

    fn get_service_release(
        &self,
        service_name: &str,
        version: &str,
    ) -> Result<Option<ServiceRelease>> {
        let mut rows = self.query("SELECT service_name, version, release_url, manifest, checksum, created_at::TEXT FROM service_releases WHERE service_name = $1 AND version = $2", &[&service_name, &version])?;
        rows.pop().map(service_release_from_row).transpose()
    }

    fn upsert_service_release(&mut self, release: ServiceRelease) -> Result<()> {
        validate_service_release_record(&release)?;
        self.execute(
            "INSERT INTO service_releases (service_name, version, release_url, manifest, checksum) VALUES ($1, $2, $3, $4, $5) ON CONFLICT (service_name, version) DO UPDATE SET release_url = EXCLUDED.release_url, manifest = EXCLUDED.manifest, checksum = EXCLUDED.checksum",
            &[&release.service_name, &release.version, &release.release_url, &release.manifest, &release.checksum],
        )?;
        Ok(())
    }

    fn delete_service_release(&mut self, service_name: &str, version: &str) -> Result<()> {
        self.execute(
            "DELETE FROM service_releases WHERE service_name = $1 AND version = $2",
            &[&service_name, &version],
        )?;
        Ok(())
    }

    fn list_service_routes(&self) -> Result<Vec<ServiceRoute>> {
        self.query("SELECT path, method, target_type, target_service_name, target_selector, permission, enabled, created_at::TEXT, updated_at::TEXT FROM service_routes ORDER BY path, method", &[])?
            .into_iter()
            .map(service_route_from_row)
            .collect()
    }

    fn upsert_service_route(&mut self, route: ServiceRoute) -> Result<()> {
        validate_service_route(&route)?;
        self.execute(
            "INSERT INTO service_routes (path, method, target_type, target_service_name, target_selector, permission, enabled, updated_at) VALUES ($1, $2, $3, $4, $5, $6, $7, NOW()) ON CONFLICT (path, method) DO UPDATE SET target_type = EXCLUDED.target_type, target_service_name = EXCLUDED.target_service_name, target_selector = EXCLUDED.target_selector, permission = EXCLUDED.permission, enabled = EXCLUDED.enabled, updated_at = NOW()",
            &[&route.path, &route.method, &route.target_type, &route.target_service_name, &route.target_selector, &route.permission, &route.enabled],
        )?;
        Ok(())
    }

    fn delete_service_routes_for_service(&mut self, service_name: &str) -> Result<()> {
        self.execute(
            "DELETE FROM service_routes WHERE target_service_name = $1",
            &[&service_name],
        )?;
        Ok(())
    }

    fn list_service_migration_records(&self) -> Result<Vec<ServiceMigrationRecord>> {
        self.query("SELECT service_name, migration_version, checksum, status, COALESCE(applied_at::TEXT, ''), created_at::TEXT, updated_at::TEXT FROM service_migration_records ORDER BY service_name, migration_version", &[])?
            .into_iter()
            .map(service_migration_record_from_row)
            .collect()
    }

    fn upsert_service_migration_record(&mut self, record: ServiceMigrationRecord) -> Result<()> {
        validate_service_migration_record(&record)?;
        let applied_at = db_time_text(&record.applied_at);
        self.execute(
            "INSERT INTO service_migration_records (service_name, migration_version, checksum, status, applied_at, updated_at) VALUES ($1, $2, $3, $4, NULLIF($5, '')::TIMESTAMPTZ, NOW()) ON CONFLICT (service_name, migration_version) DO UPDATE SET checksum = EXCLUDED.checksum, status = EXCLUDED.status, applied_at = EXCLUDED.applied_at, updated_at = NOW()",
            &[&record.service_name, &record.migration_version, &record.checksum, &record.status, &applied_at],
        )?;
        Ok(())
    }

    fn delete_service_migration_records_for_service(&mut self, service_name: &str) -> Result<()> {
        self.execute(
            "DELETE FROM service_migration_records WHERE service_name = $1",
            &[&service_name],
        )?;
        Ok(())
    }

    fn list_service_permission_records(&self) -> Result<Vec<ServicePermissionRecord>> {
        self.query("SELECT service_name, permission_key, source, created_at::TEXT, updated_at::TEXT FROM service_permission_records ORDER BY service_name, permission_key", &[])?
            .into_iter()
            .map(service_permission_record_from_row)
            .collect()
    }

    fn upsert_service_permission_record(&mut self, record: ServicePermissionRecord) -> Result<()> {
        validate_service_permission_record(&record)?;
        self.execute(
            "INSERT INTO service_permission_records (service_name, permission_key, source, updated_at) VALUES ($1, $2, $3, NOW()) ON CONFLICT (service_name, permission_key) DO UPDATE SET source = EXCLUDED.source, updated_at = NOW()",
            &[&record.service_name, &record.permission_key, &record.source],
        )?;
        Ok(())
    }

    fn delete_service_permission_records_for_service(&mut self, service_name: &str) -> Result<()> {
        self.execute(
            "DELETE FROM service_permission_records WHERE service_name = $1",
            &[&service_name],
        )?;
        Ok(())
    }

    fn list_service_frontend_entries(&self) -> Result<Vec<ServiceFrontendEntry>> {
        self.query("SELECT service_name, enabled, route_prefix, remote_entry, menu_items, created_at::TEXT, updated_at::TEXT FROM service_frontend_entries ORDER BY service_name", &[])?
            .into_iter()
            .map(service_frontend_entry_from_row)
            .collect()
    }

    fn upsert_service_frontend_entry(&mut self, entry: ServiceFrontendEntry) -> Result<()> {
        validate_service_frontend_entry(&entry)?;
        let menu_items = serde_json::to_value(&entry.menu_items)?;
        self.execute(
            "INSERT INTO service_frontend_entries (service_name, enabled, route_prefix, remote_entry, menu_items, updated_at) VALUES ($1, $2, $3, $4, $5, NOW()) ON CONFLICT (service_name) DO UPDATE SET enabled = EXCLUDED.enabled, route_prefix = EXCLUDED.route_prefix, remote_entry = EXCLUDED.remote_entry, menu_items = EXCLUDED.menu_items, updated_at = NOW()",
            &[&entry.service_name, &entry.enabled, &entry.route_prefix, &entry.remote_entry, &menu_items],
        )?;
        Ok(())
    }

    fn delete_service_frontend_entry(&mut self, service_name: &str) -> Result<()> {
        self.execute(
            "DELETE FROM service_frontend_entries WHERE service_name = $1",
            &[&service_name],
        )?;
        Ok(())
    }

    fn list_service_redis_resources(&self) -> Result<Vec<ServiceRedisResource>> {
        self.query("SELECT service_name, name, kind, usage, created_at::TEXT, updated_at::TEXT FROM service_redis_resources ORDER BY service_name, name", &[])?
            .into_iter()
            .map(service_redis_resource_from_row)
            .collect()
    }

    fn upsert_service_redis_resource(&mut self, resource: ServiceRedisResource) -> Result<()> {
        validate_service_redis_resource(&resource)?;
        self.execute(
            "INSERT INTO service_redis_resources (service_name, name, kind, usage, updated_at) VALUES ($1, $2, $3, $4, NOW()) ON CONFLICT (service_name, name) DO UPDATE SET kind = EXCLUDED.kind, usage = EXCLUDED.usage, updated_at = NOW()",
            &[&resource.service_name, &resource.name, &resource.kind, &resource.usage],
        )?;
        Ok(())
    }

    fn delete_service_redis_resources_for_service(&mut self, service_name: &str) -> Result<()> {
        self.execute(
            "DELETE FROM service_redis_resources WHERE service_name = $1",
            &[&service_name],
        )?;
        Ok(())
    }

    fn list_service_storage_resources(&self) -> Result<Vec<ServiceStorageResource>> {
        self.query("SELECT service_name, object_type, bucket, path_prefix, created_at::TEXT, updated_at::TEXT FROM service_storage_resources ORDER BY service_name, object_type, bucket", &[])?
            .into_iter()
            .map(service_storage_resource_from_row)
            .collect()
    }

    fn upsert_service_storage_resource(&mut self, resource: ServiceStorageResource) -> Result<()> {
        validate_service_storage_resource(&resource)?;
        self.execute(
            "INSERT INTO service_storage_resources (service_name, object_type, bucket, path_prefix, updated_at) VALUES ($1, $2, $3, $4, NOW()) ON CONFLICT (service_name, object_type, bucket) DO UPDATE SET path_prefix = EXCLUDED.path_prefix, updated_at = NOW()",
            &[&resource.service_name, &resource.object_type, &resource.bucket, &resource.path_prefix],
        )?;
        Ok(())
    }

    fn delete_service_storage_resources_for_service(&mut self, service_name: &str) -> Result<()> {
        self.execute(
            "DELETE FROM service_storage_resources WHERE service_name = $1",
            &[&service_name],
        )?;
        Ok(())
    }

    fn list_rendered_service_configs(&self) -> Result<Vec<RenderedServiceConfig>> {
        self.query("SELECT service_name, version, config, created_at::TEXT, updated_at::TEXT FROM rendered_service_configs ORDER BY service_name, version", &[])?
            .into_iter()
            .map(rendered_service_config_from_row)
            .collect()
    }

    fn upsert_rendered_service_config(&mut self, config: RenderedServiceConfig) -> Result<()> {
        validate_rendered_service_config(&config)?;
        self.execute(
            "INSERT INTO rendered_service_configs (service_name, version, config, updated_at) VALUES ($1, $2, $3, NOW()) ON CONFLICT (service_name, version) DO UPDATE SET config = EXCLUDED.config, updated_at = NOW()",
            &[&config.service_name, &config.version, &config.config],
        )?;
        Ok(())
    }

    fn delete_rendered_service_configs_for_service(&mut self, service_name: &str) -> Result<()> {
        self.execute(
            "DELETE FROM rendered_service_configs WHERE service_name = $1",
            &[&service_name],
        )?;
        Ok(())
    }

    fn list_nodes(&self) -> Result<Vec<NodeRecord>> {
        self.query("SELECT node_id, host_ip, parent_node_id, role, labels, status, created_at::TEXT, updated_at::TEXT FROM nodes ORDER BY node_id", &[])?
            .into_iter()
            .map(node_record_from_row)
            .collect()
    }

    fn get_node(&self, node_id: &str) -> Result<Option<NodeRecord>> {
        let mut rows = self.query("SELECT node_id, host_ip, parent_node_id, role, labels, status, created_at::TEXT, updated_at::TEXT FROM nodes WHERE node_id = $1", &[&node_id])?;
        rows.pop().map(node_record_from_row).transpose()
    }

    fn upsert_node(&mut self, node: NodeRecord) -> Result<()> {
        validate_node_record(&node)?;
        validate_pg_node_tree_upsert(self, &node)?;
        self.execute(
            "INSERT INTO nodes (node_id, host_ip, parent_node_id, role, labels, status, updated_at) VALUES ($1, $2, $3, $4, $5, $6, NOW()) ON CONFLICT (node_id) DO UPDATE SET host_ip = EXCLUDED.host_ip, parent_node_id = EXCLUDED.parent_node_id, role = EXCLUDED.role, labels = EXCLUDED.labels, status = EXCLUDED.status, updated_at = NOW()",
            &[&node.node_id, &node.host_ip, &node.parent_node_id, &node.role, &node.labels, &node.status],
        )?;
        Ok(())
    }

    fn delete_node(&mut self, node_id: &str) -> Result<()> {
        let children = self.query(
            "SELECT node_id FROM nodes WHERE parent_node_id = $1 LIMIT 1",
            &[&node_id],
        )?;
        if !children.is_empty() {
            return Err(OrchestratorError::Dependency(format!(
                "node {node_id} has child nodes"
            )));
        }
        self.execute("DELETE FROM nodes WHERE node_id = $1", &[&node_id])?;
        Ok(())
    }

    fn list_service_api_surfaces(&self) -> Result<Vec<ServiceApiSurface>> {
        self.query("SELECT service_name, version, api_id, protocol, port_name, path_prefix, methods, visibility, auth_mode, permission, stability, api_version, rate_limit, timeout, config, created_at::TEXT, updated_at::TEXT FROM service_api_surfaces ORDER BY service_name, version, api_id", &[])?
            .into_iter()
            .map(service_api_surface_from_row)
            .collect()
    }

    fn upsert_service_api_surface(&mut self, api: ServiceApiSurface) -> Result<()> {
        validate_service_api_surface(&api)?;
        let methods = serde_json::to_value(&api.methods)?;
        self.execute(
            "INSERT INTO service_api_surfaces (service_name, version, api_id, protocol, port_name, path_prefix, methods, visibility, auth_mode, permission, stability, api_version, rate_limit, timeout, config, updated_at) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, NOW()) ON CONFLICT (service_name, version, api_id) DO UPDATE SET protocol = EXCLUDED.protocol, port_name = EXCLUDED.port_name, path_prefix = EXCLUDED.path_prefix, methods = EXCLUDED.methods, visibility = EXCLUDED.visibility, auth_mode = EXCLUDED.auth_mode, permission = EXCLUDED.permission, stability = EXCLUDED.stability, api_version = EXCLUDED.api_version, rate_limit = EXCLUDED.rate_limit, timeout = EXCLUDED.timeout, config = EXCLUDED.config, updated_at = NOW()",
            &[&api.service_name, &api.version, &api.api_id, &api.protocol, &api.port_name, &api.path_prefix, &methods, &api.visibility, &api.auth_mode, &api.permission, &api.stability, &api.api_version, &api.rate_limit, &api.timeout, &api.config],
        )?;
        Ok(())
    }

    fn delete_service_api_surfaces_for_service(&mut self, service_name: &str) -> Result<()> {
        self.execute(
            "DELETE FROM service_api_surfaces WHERE service_name = $1",
            &[&service_name],
        )?;
        Ok(())
    }

    fn list_deployed_service_apis(&self) -> Result<Vec<DeployedServiceApi>> {
        self.query("SELECT host_ip, service_name, version, endpoint, api_id, status, created_at::TEXT, updated_at::TEXT FROM deployed_service_apis ORDER BY host_ip, service_name, api_id, endpoint", &[])?
            .into_iter()
            .map(deployed_service_api_from_row)
            .collect()
    }

    fn upsert_deployed_service_api(&mut self, api: DeployedServiceApi) -> Result<()> {
        validate_deployed_service_api(&api)?;
        self.execute(
            "INSERT INTO deployed_service_apis (host_ip, service_name, version, endpoint, api_id, status, updated_at) VALUES ($1, $2, $3, $4, $5, $6, NOW()) ON CONFLICT (host_ip, service_name, api_id, endpoint) DO UPDATE SET version = EXCLUDED.version, status = EXCLUDED.status, updated_at = NOW()",
            &[&api.host_ip, &api.service_name, &api.version, &api.endpoint, &api.api_id, &api.status],
        )?;
        Ok(())
    }

    fn delete_deployed_service_apis_for_service(&mut self, service_name: &str) -> Result<()> {
        self.execute(
            "DELETE FROM deployed_service_apis WHERE service_name = $1",
            &[&service_name],
        )?;
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
        validate_endpoint(&endpoint)?;
        let identity = parse_endpoint_id(&endpoint.endpoint)?;
        let port = identity.port.parse::<i32>().map_err(|_| {
            OrchestratorError::InvalidManifest("endpoint port is invalid".to_string())
        })?;
        self.execute(
            "INSERT INTO service_endpoints (endpoint, service_id, ip, port, service_name, host_ip, protocol, health_path, health, reachable, display_name, note, config, updated_at) VALUES ($1, $2, $3, $4, $5, $3, $6, $7, $8, $9, $10, $11, $12, NOW()) ON CONFLICT (endpoint) DO UPDATE SET service_id = EXCLUDED.service_id, ip = EXCLUDED.ip, port = EXCLUDED.port, service_name = EXCLUDED.service_name, host_ip = EXCLUDED.host_ip, protocol = EXCLUDED.protocol, health_path = EXCLUDED.health_path, health = EXCLUDED.health, reachable = EXCLUDED.reachable, display_name = EXCLUDED.display_name, note = EXCLUDED.note, config = EXCLUDED.config, updated_at = NOW()",
            &[
                &endpoint.endpoint,
                &endpoint.service_id,
                &identity.host,
                &port,
                &identity.service_name,
                &endpoint.protocol,
                &endpoint.health_path,
                &endpoint.health,
                &endpoint.reachable,
                &endpoint.display_name,
                &endpoint.note,
                &endpoint.config,
            ],
        )?;
        Ok(())
    }

    fn delete_endpoint(&mut self, endpoint: &str) -> Result<()> {
        validate_endpoint_id(endpoint)?;
        let mut client = self.connect()?;
        client
            .execute(
                "DELETE FROM service_links WHERE source_endpoint = $1 OR target_endpoint = $1",
                &[&endpoint],
            )
            .map_err(|err| OrchestratorError::Dependency(format!("orchestrator db: {err}")))?;
        client
            .execute("DELETE FROM log_sources WHERE endpoint = $1", &[&endpoint])
            .map_err(|err| OrchestratorError::Dependency(format!("orchestrator db: {err}")))?;
        client
            .execute(
                "DELETE FROM deployed_service_apis WHERE endpoint = $1",
                &[&endpoint],
            )
            .map_err(|err| OrchestratorError::Dependency(format!("orchestrator db: {err}")))?;
        client
            .execute(
                "DELETE FROM service_endpoints WHERE endpoint = $1",
                &[&endpoint],
            )
            .map_err(|err| OrchestratorError::Dependency(format!("orchestrator db: {err}")))?;
        Ok(())
    }

    fn update_endpoint_health(
        &mut self,
        endpoint: &str,
        health: String,
        reachable: bool,
    ) -> Result<()> {
        validate_endpoint_id(endpoint)?;
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
        let endpoints = self.list_endpoints()?;
        validate_link(&link, &endpoints)?;
        let source = parse_endpoint_id(&link.source_endpoint)?;
        let target = parse_endpoint_id(&link.target_endpoint)?;
        let source_port = source.port.parse::<i32>().map_err(|_| {
            OrchestratorError::InvalidManifest("endpoint port is invalid".to_string())
        })?;
        let target_port = target.port.parse::<i32>().map_err(|_| {
            OrchestratorError::InvalidManifest("endpoint port is invalid".to_string())
        })?;
        let latency_ms = link.latency_ms.map(|value| value as i32);
        self.execute(
            "INSERT INTO service_links (source_endpoint, target_endpoint, from_ip, from_port, from_service_name, to_type, to_ip, to_port, to_service_name, protocol, auth_mode, scope, health, latency_ms, config_ref, secret_ref, policy, updated_at) VALUES ($1, $2, $3, $4, $5, 'endpoint', $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, NOW()) ON CONFLICT (source_endpoint, target_endpoint) DO UPDATE SET from_ip = EXCLUDED.from_ip, from_port = EXCLUDED.from_port, from_service_name = EXCLUDED.from_service_name, to_type = EXCLUDED.to_type, to_ip = EXCLUDED.to_ip, to_port = EXCLUDED.to_port, to_service_name = EXCLUDED.to_service_name, protocol = EXCLUDED.protocol, auth_mode = EXCLUDED.auth_mode, scope = EXCLUDED.scope, health = EXCLUDED.health, latency_ms = EXCLUDED.latency_ms, config_ref = EXCLUDED.config_ref, secret_ref = EXCLUDED.secret_ref, policy = EXCLUDED.policy, updated_at = NOW()",
            &[&link.source_endpoint, &link.target_endpoint, &source.host, &source_port, &source.service_name, &target.host, &target_port, &target.service_name, &link.protocol, &link.auth_mode, &link.scope, &link.health, &latency_ms, &link.config_ref, &link.secret_ref, &link.policy],
        )?;
        Ok(())
    }

    fn delete_link(&mut self, source_endpoint: &str, target_endpoint: &str) -> Result<()> {
        validate_endpoint_id(source_endpoint)?;
        validate_endpoint_id(target_endpoint)?;
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
        validate_endpoint_id(source_endpoint)?;
        validate_endpoint_id(target_endpoint)?;
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
        validate_topology(&snapshot.topology)?;
        let topology = serde_json::to_value(&snapshot.topology)?;
        self.execute("INSERT INTO topology_snapshots (snapshot_id, topology) VALUES ($1, $2) ON CONFLICT (snapshot_id) DO UPDATE SET topology = EXCLUDED.topology, created_at = NOW()", &[&snapshot.snapshot_id, &topology])?;
        Ok(())
    }

    fn get_latest_topology_snapshot(&self) -> Result<Option<TopologySnapshot>> {
        let mut rows = self.query("SELECT snapshot_id, topology, created_at::TEXT FROM topology_snapshots ORDER BY created_at DESC LIMIT 1", &[])?;
        rows.pop().map(topology_snapshot_from_row).transpose()
    }

    fn build_topology_view(&self) -> Result<Topology> {
        let endpoints = self.list_endpoints()?;
        if endpoints.is_empty() {
            if let Some(snapshot) = self.get_latest_topology_snapshot()? {
                return Ok(snapshot.topology);
            }
            return Err(OrchestratorError::Dependency(
                "no endpoint for topology".to_string(),
            ));
        }
        let root_endpoint = endpoints
            .iter()
            .find(|endpoint| endpoint.service_id == "gateway")
            .or_else(|| endpoints.first())
            .map(|endpoint| endpoint.endpoint.clone())
            .ok_or_else(|| OrchestratorError::Dependency("no endpoint for topology".to_string()))?;
        build_topology(
            root_endpoint,
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
    let endpoint = Endpoint {
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
    };
    validate_endpoint(&endpoint)?;
    Ok(endpoint)
}

fn host_service_from_row(row: Row) -> Result<HostService> {
    let host_service = HostService {
        host_ip: row.get(0),
        service_name: row.get(1),
        version: row.get(2),
        status: row.get(3),
        config: row.get(4),
        labels: row.get(5),
        created_at: row.get(6),
        updated_at: row.get(7),
    };
    validate_host_service(&host_service)?;
    Ok(host_service)
}

fn link_from_row(row: Row) -> Result<Link> {
    let latency_ms = row.get::<usize, Option<i32>>(6).map(|value| value as u32);
    let link = Link {
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
    };
    validate_endpoint_id(&link.source_endpoint)?;
    validate_endpoint_id(&link.target_endpoint)?;
    Ok(link)
}

fn service_release_from_row(row: Row) -> Result<ServiceRelease> {
    let release = ServiceRelease {
        service_name: row.get(0),
        version: row.get(1),
        release_url: row.get(2),
        manifest: row.get(3),
        checksum: row.get(4),
        created_at: row.get(5),
    };
    validate_service_release_record(&release)?;
    Ok(release)
}

fn service_route_from_row(row: Row) -> Result<ServiceRoute> {
    let route = ServiceRoute {
        path: row.get(0),
        method: row.get(1),
        target_type: row.get(2),
        target_service_name: row.get(3),
        target_selector: row.get(4),
        permission: row.get(5),
        enabled: row.get(6),
        created_at: row.get(7),
        updated_at: row.get(8),
    };
    validate_service_route(&route)?;
    Ok(route)
}

fn service_migration_record_from_row(row: Row) -> Result<ServiceMigrationRecord> {
    let record = ServiceMigrationRecord {
        service_name: row.get(0),
        migration_version: row.get(1),
        checksum: row.get(2),
        status: row.get(3),
        applied_at: row.get(4),
        created_at: row.get(5),
        updated_at: row.get(6),
    };
    validate_service_migration_record(&record)?;
    Ok(record)
}

fn service_permission_record_from_row(row: Row) -> Result<ServicePermissionRecord> {
    let record = ServicePermissionRecord {
        service_name: row.get(0),
        permission_key: row.get(1),
        source: row.get(2),
        created_at: row.get(3),
        updated_at: row.get(4),
    };
    validate_service_permission_record(&record)?;
    Ok(record)
}

fn service_frontend_entry_from_row(row: Row) -> Result<ServiceFrontendEntry> {
    let menu_items_value: serde_json::Value = row.get(4);
    let entry = ServiceFrontendEntry {
        service_name: row.get(0),
        enabled: row.get(1),
        route_prefix: row.get(2),
        remote_entry: row.get(3),
        menu_items: serde_json::from_value(menu_items_value)?,
        created_at: row.get(5),
        updated_at: row.get(6),
    };
    validate_service_frontend_entry(&entry)?;
    Ok(entry)
}

fn service_redis_resource_from_row(row: Row) -> Result<ServiceRedisResource> {
    let resource = ServiceRedisResource {
        service_name: row.get(0),
        name: row.get(1),
        kind: row.get(2),
        usage: row.get(3),
        created_at: row.get(4),
        updated_at: row.get(5),
    };
    validate_service_redis_resource(&resource)?;
    Ok(resource)
}

fn service_storage_resource_from_row(row: Row) -> Result<ServiceStorageResource> {
    let resource = ServiceStorageResource {
        service_name: row.get(0),
        object_type: row.get(1),
        bucket: row.get(2),
        path_prefix: row.get(3),
        created_at: row.get(4),
        updated_at: row.get(5),
    };
    validate_service_storage_resource(&resource)?;
    Ok(resource)
}

fn rendered_service_config_from_row(row: Row) -> Result<RenderedServiceConfig> {
    let config = RenderedServiceConfig {
        service_name: row.get(0),
        version: row.get(1),
        config: row.get(2),
        created_at: row.get(3),
        updated_at: row.get(4),
    };
    validate_rendered_service_config(&config)?;
    Ok(config)
}

fn node_record_from_row(row: Row) -> Result<NodeRecord> {
    let node = NodeRecord {
        node_id: row.get(0),
        host_ip: row.get(1),
        parent_node_id: row.get(2),
        role: row.get(3),
        labels: row.get(4),
        status: row.get(5),
        created_at: row.get(6),
        updated_at: row.get(7),
    };
    validate_node_record(&node)?;
    Ok(node)
}

fn service_api_surface_from_row(row: Row) -> Result<ServiceApiSurface> {
    let methods_value: serde_json::Value = row.get(6);
    let api = ServiceApiSurface {
        service_name: row.get(0),
        version: row.get(1),
        api_id: row.get(2),
        protocol: row.get(3),
        port_name: row.get(4),
        path_prefix: row.get(5),
        methods: serde_json::from_value(methods_value)?,
        visibility: row.get(7),
        auth_mode: row.get(8),
        permission: row.get(9),
        stability: row.get(10),
        api_version: row.get(11),
        rate_limit: row.get(12),
        timeout: row.get(13),
        config: row.get(14),
        created_at: row.get(15),
        updated_at: row.get(16),
    };
    validate_service_api_surface(&api)?;
    Ok(api)
}

fn deployed_service_api_from_row(row: Row) -> Result<DeployedServiceApi> {
    let api = DeployedServiceApi {
        host_ip: row.get(0),
        service_name: row.get(1),
        version: row.get(2),
        endpoint: row.get(3),
        api_id: row.get(4),
        status: row.get(5),
        created_at: row.get(6),
        updated_at: row.get(7),
    };
    validate_deployed_service_api(&api)?;
    Ok(api)
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
    let snapshot = TopologySnapshot {
        snapshot_id: row.get(0),
        topology: json_model(row.get(1))?,
        created_at: row.get(2),
    };
    validate_topology(&snapshot.topology)?;
    Ok(snapshot)
}

fn log_view_from_row(row: Row) -> Result<LogView> {
    let log_view = LogView {
        source_id: row.get(0),
        service_id: row.get(1),
        endpoint: row.get(2),
        operation_id: row.get(3),
        path: row.get(4),
        driver: row.get(5),
        read_policy: row.get(6),
        display_name: row.get(0),
    };
    validate_log_view(&log_view)?;
    Ok(log_view)
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

fn validate_pg_node_tree_upsert(store: &PgOrchestratorStore, node: &NodeRecord) -> Result<()> {
    let mut nodes = store
        .list_nodes()?
        .into_iter()
        .map(|item| (item.node_id.clone(), item))
        .collect::<BTreeMap<_, _>>();
    if nodes
        .values()
        .any(|item| item.node_id != node.node_id && item.host_ip == node.host_ip)
    {
        return Err(OrchestratorError::InvalidManifest(format!(
            "node host_ip {} is already registered",
            node.host_ip
        )));
    }
    match node.role.as_str() {
        "root" | "standalone" => {
            if !node.parent_node_id.trim().is_empty() {
                return Err(OrchestratorError::InvalidManifest(format!(
                    "{} node must not have parent_node_id",
                    node.role
                )));
            }
        }
        "node" => {
            if node.parent_node_id.trim().is_empty() {
                return Err(OrchestratorError::InvalidManifest(
                    "node parent_node_id is required".to_string(),
                ));
            }
            if !nodes.contains_key(&node.parent_node_id) {
                return Err(OrchestratorError::Dependency(format!(
                    "parent node {} not found",
                    node.parent_node_id
                )));
            }
        }
        _ => {}
    }
    nodes.insert(node.node_id.clone(), node.clone());
    for node_id in nodes.keys() {
        let mut seen = BTreeSet::new();
        let mut current = node_id.as_str();
        loop {
            if !seen.insert(current.to_string()) {
                return Err(OrchestratorError::InvalidManifest(format!(
                    "node tree contains cycle at {current}"
                )));
            }
            let Some(item) = nodes.get(current) else {
                return Err(OrchestratorError::Dependency(format!(
                    "node {current} is missing during tree validation"
                )));
            };
            let parent = item.parent_node_id.trim();
            if parent.is_empty() {
                break;
            }
            if !nodes.contains_key(parent) {
                return Err(OrchestratorError::Dependency(format!(
                    "parent node {parent} not found"
                )));
            }
            current = parent;
        }
    }
    Ok(())
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
    for host_service in store.host_services()? {
        writes.push(database_write(
            "HostService",
            format!("{}:{}", host_service.host_ip, host_service.service_name),
            "host_services",
            "host_services.upsert",
        ));
    }
    for service in store.services()? {
        let service_id = service.id.clone();
        writes.push(database_write(
            "Service",
            service_id,
            "services",
            "services.upsert",
        ));
    }
    for release in store.service_releases()? {
        writes.push(database_write(
            "ServiceRelease",
            format!("{}@{}", release.service_name, release.version),
            "service_releases",
            "service_releases.upsert",
        ));
    }
    for route in store.service_routes()? {
        writes.push(database_write(
            "Route",
            format!("{} {}", route.method, route.path),
            "service_routes",
            "service_routes.upsert",
        ));
    }
    for migration in store.service_migration_records()? {
        writes.push(database_write(
            "MigrationRecord",
            format!("{}@{}", migration.service_name, migration.migration_version),
            "service_migration_records",
            "service_migration_records.upsert",
        ));
    }
    for permission in store.service_permission_records()? {
        writes.push(database_write(
            "Permission",
            format!("{}:{}", permission.service_name, permission.permission_key),
            "service_permission_records",
            "service_permission_records.upsert",
        ));
    }
    for frontend in store.service_frontend_entries()? {
        writes.push(database_write(
            "Frontend",
            frontend.service_name,
            "service_frontend_entries",
            "service_frontend_entries.upsert",
        ));
    }
    for redis in store.service_redis_resources()? {
        writes.push(database_write(
            "RedisResource",
            format!("{}:{}", redis.service_name, redis.name),
            "service_redis_resources",
            "service_redis_resources.upsert",
        ));
    }
    for storage in store.service_storage_resources()? {
        writes.push(database_write(
            "StorageResource",
            format!(
                "{}:{}:{}",
                storage.service_name, storage.object_type, storage.bucket
            ),
            "service_storage_resources",
            "service_storage_resources.upsert",
        ));
    }
    for config in store.rendered_service_configs()? {
        writes.push(database_write(
            "RenderedConfig",
            format!("{}@{}", config.service_name, config.version),
            "rendered_service_configs",
            "rendered_service_configs.upsert",
        ));
    }
    for node in store.nodes()? {
        writes.push(database_write(
            "Node",
            node.node_id,
            "nodes",
            "nodes.upsert",
        ));
    }
    for api in store.service_api_surfaces()? {
        writes.push(database_write(
            "ServiceApiSurface",
            format!("{}@{}:{}", api.service_name, api.version, api.api_id),
            "service_api_surfaces",
            "service_api_surfaces.upsert",
        ));
    }
    for api in store.deployed_service_apis()? {
        writes.push(database_write(
            "DeployedServiceApi",
            format!(
                "{}@{}:{}:{}",
                api.service_name, api.version, api.api_id, api.endpoint
            ),
            "deployed_service_apis",
            "deployed_service_apis.upsert",
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
