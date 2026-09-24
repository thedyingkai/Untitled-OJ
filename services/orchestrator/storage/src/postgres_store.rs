use crate::OrchestratorStore;
use crate::{
    PostgresError, PostgresOptions, PostgresPool, PostgresReadinessReport,
    sqlite::{
        API_SURFACES, DEPLOYED_APIS, DIAGNOSTICS, ENDPOINTS, FRONTENDS, HOST_SERVICES, LINKS,
        LOG_SOURCES, MIGRATION_RECORDS, NODES, OPERATION_LOCKS, OPERATIONS, PERMISSION_RECORDS,
        REDIS_RESOURCES, RELEASES, RENDERED_CONFIGS, ROUTES, SERVICES, STORAGE_RESOURCES,
        TOPOLOGY_SNAPSHOTS,
    },
};
pub(crate) use orchestrator_core::validate_node_tree;
use orchestrator_core::{
    DeployedServiceApi, DiagnosticReport, Endpoint, HostService, Link, LogView, NodeRecord,
    Operation, OperationLock, OperationLogRecord, OperationStatus, OrchestratorError,
    RenderedServiceConfig, ServiceApiSurface, ServiceFrontendEntry, ServiceManifest,
    ServiceMigrationRecord, ServicePermissionRecord, ServiceRedisResource, ServiceRelease,
    ServiceRoute, ServiceStorageResource, Topology, TopologySnapshot, build_topology,
    validate_deployed_service_api, validate_endpoint, validate_endpoint_id, validate_host_service,
    validate_link, validate_log_view, validate_node_record, validate_rendered_service_config,
    validate_service_api_surface, validate_service_frontend_entry, validate_service_manifest,
    validate_service_migration_record, validate_service_permission_record,
    validate_service_redis_resource, validate_service_release_record, validate_service_route,
    validate_service_storage_resource, validate_topology,
};
use r2d2_postgres::postgres::Transaction;
use serde::{Serialize, de::DeserializeOwned};

/// Pool-backed PostgreSQL implementation of the current domain repository.
///
/// Each operation checks out its own connection and mutations use only the
/// shortest transaction needed for the state transition. The type never owns
/// a full-table memory mirror.
#[derive(Debug, Clone)]
pub struct PostgresOrchestratorStore {
    pool: PostgresPool,
}

impl PostgresOrchestratorStore {
    pub fn connect(database_url: &str, options: PostgresOptions) -> Result<Self, PostgresError> {
        Self::from_pool(PostgresPool::connect(database_url, options)?)
    }

    pub fn from_pool(pool: PostgresPool) -> Result<Self, PostgresError> {
        pool.apply_migrations()?;
        pool.readiness()?;
        let store = Self { pool };
        store.import_legacy_v0_2()?;
        store.readiness()?;
        Ok(store)
    }

    pub fn pool(&self) -> &PostgresPool {
        &self.pool
    }

    pub fn readiness(&self) -> Result<PostgresReadinessReport, PostgresError> {
        self.pool.readiness()
    }

    pub fn put_state<T: Serialize>(
        &self,
        namespace: &str,
        key: &str,
        value: &T,
    ) -> Result<(), PostgresError> {
        let payload = serde_json::to_string(value)?;
        self.pool.with_client(|client| {
            client.execute(
                "INSERT INTO orchestrator_state(namespace, state_key, payload) VALUES ($1, $2, $3::text::jsonb) ON CONFLICT(namespace, state_key) DO UPDATE SET payload = excluded.payload, updated_at = clock_timestamp()",
                &[&namespace, &key, &payload],
            )?;
            Ok(())
        })
    }

    pub fn get_state<T: DeserializeOwned>(
        &self,
        namespace: &str,
        key: &str,
    ) -> Result<Option<T>, PostgresError> {
        self.pool.with_client(|client| {
            client
                .query_opt(
                    "SELECT payload::text FROM orchestrator_state WHERE namespace = $1 AND state_key = $2",
                    &[&namespace, &key],
                )?
                .map(|row| serde_json::from_str::<T>(&row.get::<_, String>(0)).map_err(Into::into))
                .transpose()
        })
    }

    pub fn delete_state(&self, namespace: &str, key: &str) -> Result<bool, PostgresError> {
        self.pool.with_client(|client| {
            Ok(client.execute(
                "DELETE FROM orchestrator_state WHERE namespace = $1 AND state_key = $2",
                &[&namespace, &key],
            )? > 0)
        })
    }

    fn list_records<T: DeserializeOwned>(&self, kind: &str) -> orchestrator_core::Result<Vec<T>> {
        self.pool
            .with_client(|client| {
                client
                    .query(
                        "SELECT payload::text FROM orchestrator_records WHERE kind = $1 ORDER BY record_key",
                        &[&kind],
                    )?
                    .into_iter()
                    .map(|row| serde_json::from_str::<T>(&row.get::<_, String>(0)).map_err(Into::into))
                    .collect()
            })
            .map_err(core_postgres_error)
    }

    fn get_record<T: DeserializeOwned>(
        &self,
        kind: &str,
        key: &str,
    ) -> orchestrator_core::Result<Option<T>> {
        self.pool
            .with_client(|client| {
                client
                    .query_opt(
                        "SELECT payload::text FROM orchestrator_records WHERE kind = $1 AND record_key = $2",
                        &[&kind, &key],
                    )?
                    .map(|row| serde_json::from_str::<T>(&row.get::<_, String>(0)).map_err(Into::into))
                    .transpose()
            })
            .map_err(core_postgres_error)
    }

    fn upsert_record<T: Serialize>(
        &self,
        kind: &str,
        key: &str,
        scope: &str,
        value: &T,
    ) -> orchestrator_core::Result<()> {
        let payload = serde_json::to_string(value)?;
        self.pool
            .with_client(|client| {
                client.execute(
                    "INSERT INTO orchestrator_records(kind, record_key, scope, payload) VALUES ($1, $2, $3, $4::text::jsonb) ON CONFLICT(kind, record_key) DO UPDATE SET scope = excluded.scope, payload = excluded.payload, updated_at = clock_timestamp()",
                    &[&kind, &key, &scope, &payload],
                )?;
                Ok(())
            })
            .map_err(core_postgres_error)
    }

    fn delete_record(&self, kind: &str, key: &str) -> orchestrator_core::Result<bool> {
        self.pool
            .with_client(|client| {
                Ok(client.execute(
                    "DELETE FROM orchestrator_records WHERE kind = $1 AND record_key = $2",
                    &[&kind, &key],
                )? > 0)
            })
            .map_err(core_postgres_error)
    }

    fn delete_scope(&self, kind: &str, scope: &str) -> orchestrator_core::Result<()> {
        self.pool
            .with_client(|client| {
                client.execute(
                    "DELETE FROM orchestrator_records WHERE kind = $1 AND scope = $2",
                    &[&kind, &scope],
                )?;
                Ok(())
            })
            .map_err(core_postgres_error)
    }

    fn update_record<T, F>(&self, kind: &str, key: &str, update: F) -> orchestrator_core::Result<()>
    where
        T: Serialize + DeserializeOwned,
        F: FnOnce(&mut T),
    {
        self.pool
            .with_transaction(|transaction| {
                let row = transaction
                    .query_opt(
                        "SELECT payload::text FROM orchestrator_records WHERE kind = $1 AND record_key = $2 FOR UPDATE",
                        &[&kind, &key],
                    )?
                    .ok_or_else(|| {
                        PostgresError::InvalidConfiguration(format!(
                            "{kind} record {key} not found"
                        ))
                    })?;
                let mut value: T = serde_json::from_str(&row.get::<_, String>(0))?;
                update(&mut value);
                let payload = serde_json::to_string(&value)?;
                transaction.execute(
                    "UPDATE orchestrator_records SET payload = $3::text::jsonb, updated_at = clock_timestamp() WHERE kind = $1 AND record_key = $2",
                    &[&kind, &key, &payload],
                )?;
                Ok(())
            })
            .map_err(core_postgres_error)
    }
}

fn key(parts: &[&str]) -> String {
    serde_json::to_string(parts).expect("string slices always serialize")
}

fn core_postgres_error(error: PostgresError) -> OrchestratorError {
    OrchestratorError::Dependency(format!("orchestrator postgres storage: {error}"))
}

fn operation_exists(
    transaction: &mut Transaction<'_>,
    operation_id: &str,
) -> Result<bool, PostgresError> {
    Ok(transaction
        .query_one(
            "SELECT EXISTS(SELECT 1 FROM orchestrator_records WHERE kind = $1 AND record_key = $2)",
            &[&OPERATIONS, &operation_id],
        )?
        .get(0))
}

impl OrchestratorStore for PostgresOrchestratorStore {
    fn list_services(&self) -> orchestrator_core::Result<Vec<ServiceManifest>> {
        self.list_records(SERVICES)
    }

    fn get_service(&self, service_id: &str) -> orchestrator_core::Result<Option<ServiceManifest>> {
        self.get_record(SERVICES, service_id)
    }

    fn upsert_service(&mut self, value: ServiceManifest) -> orchestrator_core::Result<()> {
        validate_service_manifest(&value)?;
        self.upsert_record(SERVICES, &value.id, &value.id, &value)
    }

    fn delete_service(&mut self, service_id: &str) -> orchestrator_core::Result<()> {
        let endpoint_ids = self
            .list_endpoints()?
            .into_iter()
            .filter(|endpoint| endpoint.service_id == service_id)
            .map(|endpoint| endpoint.endpoint)
            .collect::<Vec<_>>();
        self.pool
            .with_transaction(|transaction| {
                for endpoint in &endpoint_ids {
                    transaction.execute(
                        "DELETE FROM orchestrator_records WHERE (kind = $1 AND payload->>'endpoint' = $2) OR (kind = $3 AND (payload->>'source_endpoint' = $2 OR payload->>'target_endpoint' = $2)) OR (kind = $4 AND payload->>'endpoint' = $2)",
                        &[&DEPLOYED_APIS, endpoint, &LINKS, &LOG_SOURCES],
                    )?;
                }
                transaction.execute(
                    "DELETE FROM orchestrator_records WHERE (kind = $1 AND record_key = $2) OR (kind = ANY($3) AND scope = $2)",
                    &[&SERVICES, &service_id, &&[
                        HOST_SERVICES, RELEASES, ROUTES, MIGRATION_RECORDS, PERMISSION_RECORDS,
                        FRONTENDS, REDIS_RESOURCES, STORAGE_RESOURCES, RENDERED_CONFIGS,
                        API_SURFACES, DEPLOYED_APIS, ENDPOINTS,
                    ][..]],
                )?;
                Ok(())
            })
            .map_err(core_postgres_error)
    }

    fn list_host_services(&self) -> orchestrator_core::Result<Vec<HostService>> {
        self.list_records(HOST_SERVICES)
    }

    fn get_host_service(
        &self,
        host_ip: &str,
        service_name: &str,
    ) -> orchestrator_core::Result<Option<HostService>> {
        self.get_record(HOST_SERVICES, &key(&[host_ip, service_name]))
    }

    fn upsert_host_service(&mut self, value: HostService) -> orchestrator_core::Result<()> {
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
    ) -> orchestrator_core::Result<()> {
        self.delete_record(HOST_SERVICES, &key(&[host_ip, service_name]))
            .map(|_| ())
    }

    fn delete_host_services_for_service(
        &mut self,
        service_name: &str,
    ) -> orchestrator_core::Result<()> {
        self.delete_scope(HOST_SERVICES, service_name)
    }

    fn list_service_releases(&self) -> orchestrator_core::Result<Vec<ServiceRelease>> {
        self.list_records(RELEASES)
    }

    fn get_service_release(
        &self,
        service_name: &str,
        version: &str,
    ) -> orchestrator_core::Result<Option<ServiceRelease>> {
        self.get_record(RELEASES, &key(&[service_name, version]))
    }

    fn upsert_service_release(&mut self, value: ServiceRelease) -> orchestrator_core::Result<()> {
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
    ) -> orchestrator_core::Result<()> {
        self.delete_record(RELEASES, &key(&[service_name, version]))
            .map(|_| ())
    }

    fn register_service_release_atomic(
        &mut self,
        service: ServiceManifest,
        release: ServiceRelease,
    ) -> orchestrator_core::Result<()> {
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
        self.pool
            .with_transaction(|transaction| {
                transaction.execute(
                    "INSERT INTO orchestrator_records(kind, record_key, scope, payload) VALUES ($1, $2, $2, $3::text::jsonb) ON CONFLICT(kind, record_key) DO UPDATE SET scope = excluded.scope, payload = excluded.payload, updated_at = clock_timestamp()",
                    &[&SERVICES, &service.id, &service_payload],
                )?;
                transaction.execute(
                    "INSERT INTO orchestrator_records(kind, record_key, scope, payload) VALUES ($1, $2, $3, $4::text::jsonb) ON CONFLICT(kind, record_key) DO UPDATE SET scope = excluded.scope, payload = excluded.payload, updated_at = clock_timestamp()",
                    &[&RELEASES, &release_key, &release.service_name, &release_payload],
                )?;
                Ok(())
            })
            .map_err(core_postgres_error)
    }

    fn list_service_routes(&self) -> orchestrator_core::Result<Vec<ServiceRoute>> {
        self.list_records(ROUTES)
    }

    fn upsert_service_route(&mut self, value: ServiceRoute) -> orchestrator_core::Result<()> {
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
    ) -> orchestrator_core::Result<()> {
        self.delete_scope(ROUTES, service_name)
    }

    fn list_service_migration_records(
        &self,
    ) -> orchestrator_core::Result<Vec<ServiceMigrationRecord>> {
        self.list_records(MIGRATION_RECORDS)
    }

    fn upsert_service_migration_record(
        &mut self,
        value: ServiceMigrationRecord,
    ) -> orchestrator_core::Result<()> {
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
    ) -> orchestrator_core::Result<()> {
        self.delete_scope(MIGRATION_RECORDS, service_name)
    }

    fn list_service_permission_records(
        &self,
    ) -> orchestrator_core::Result<Vec<ServicePermissionRecord>> {
        self.list_records(PERMISSION_RECORDS)
    }

    fn upsert_service_permission_record(
        &mut self,
        value: ServicePermissionRecord,
    ) -> orchestrator_core::Result<()> {
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
    ) -> orchestrator_core::Result<()> {
        self.delete_scope(PERMISSION_RECORDS, service_name)
    }

    fn list_service_frontend_entries(
        &self,
    ) -> orchestrator_core::Result<Vec<ServiceFrontendEntry>> {
        self.list_records(FRONTENDS)
    }

    fn upsert_service_frontend_entry(
        &mut self,
        value: ServiceFrontendEntry,
    ) -> orchestrator_core::Result<()> {
        validate_service_frontend_entry(&value)?;
        self.upsert_record(FRONTENDS, &value.service_name, &value.service_name, &value)
    }

    fn delete_service_frontend_entry(
        &mut self,
        service_name: &str,
    ) -> orchestrator_core::Result<()> {
        self.delete_record(FRONTENDS, service_name).map(|_| ())
    }

    fn list_service_redis_resources(&self) -> orchestrator_core::Result<Vec<ServiceRedisResource>> {
        self.list_records(REDIS_RESOURCES)
    }

    fn upsert_service_redis_resource(
        &mut self,
        value: ServiceRedisResource,
    ) -> orchestrator_core::Result<()> {
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
    ) -> orchestrator_core::Result<()> {
        self.delete_scope(REDIS_RESOURCES, service_name)
    }

    fn list_service_storage_resources(
        &self,
    ) -> orchestrator_core::Result<Vec<ServiceStorageResource>> {
        self.list_records(STORAGE_RESOURCES)
    }

    fn upsert_service_storage_resource(
        &mut self,
        value: ServiceStorageResource,
    ) -> orchestrator_core::Result<()> {
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
    ) -> orchestrator_core::Result<()> {
        self.delete_scope(STORAGE_RESOURCES, service_name)
    }

    fn list_rendered_service_configs(
        &self,
    ) -> orchestrator_core::Result<Vec<RenderedServiceConfig>> {
        self.list_records(RENDERED_CONFIGS)
    }

    fn upsert_rendered_service_config(
        &mut self,
        value: RenderedServiceConfig,
    ) -> orchestrator_core::Result<()> {
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
    ) -> orchestrator_core::Result<()> {
        self.delete_scope(RENDERED_CONFIGS, service_name)
    }

    fn list_nodes(&self) -> orchestrator_core::Result<Vec<NodeRecord>> {
        self.list_records(NODES)
    }

    fn get_node(&self, node_id: &str) -> orchestrator_core::Result<Option<NodeRecord>> {
        self.get_record(NODES, node_id)
    }

    fn upsert_node(&mut self, value: NodeRecord) -> orchestrator_core::Result<()> {
        validate_node_record(&value)?;
        validate_node_tree(self.list_nodes()?, &value)?;
        self.upsert_record(NODES, &value.node_id, &value.parent_node_id, &value)
    }

    fn delete_node(&mut self, node_id: &str) -> orchestrator_core::Result<()> {
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

    fn list_service_api_surfaces(&self) -> orchestrator_core::Result<Vec<ServiceApiSurface>> {
        self.list_records(API_SURFACES)
    }

    fn upsert_service_api_surface(
        &mut self,
        value: ServiceApiSurface,
    ) -> orchestrator_core::Result<()> {
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
    ) -> orchestrator_core::Result<()> {
        self.delete_scope(API_SURFACES, service_name)
    }

    fn list_deployed_service_apis(&self) -> orchestrator_core::Result<Vec<DeployedServiceApi>> {
        self.list_records(DEPLOYED_APIS)
    }

    fn upsert_deployed_service_api(
        &mut self,
        value: DeployedServiceApi,
    ) -> orchestrator_core::Result<()> {
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
    ) -> orchestrator_core::Result<()> {
        self.delete_scope(DEPLOYED_APIS, service_name)
    }

    fn list_endpoints(&self) -> orchestrator_core::Result<Vec<Endpoint>> {
        self.list_records(ENDPOINTS)
    }

    fn get_endpoint(&self, endpoint: &str) -> orchestrator_core::Result<Option<Endpoint>> {
        self.get_record(ENDPOINTS, endpoint)
    }

    fn upsert_endpoint(&mut self, value: Endpoint) -> orchestrator_core::Result<()> {
        validate_endpoint(&value)?;
        if self.get_service(&value.service_id)?.is_none() {
            return Err(OrchestratorError::Dependency(format!(
                "endpoint references missing service {}",
                value.service_id
            )));
        }
        self.upsert_record(ENDPOINTS, &value.endpoint, &value.service_id, &value)
    }

    fn delete_endpoint(&mut self, endpoint: &str) -> orchestrator_core::Result<()> {
        validate_endpoint_id(endpoint)?;
        self.pool
            .with_transaction(|transaction| {
                transaction.execute(
                    "DELETE FROM orchestrator_records WHERE (kind = $1 AND record_key = $2) OR (kind = $3 AND payload->>'endpoint' = $2) OR (kind = $4 AND (payload->>'source_endpoint' = $2 OR payload->>'target_endpoint' = $2)) OR (kind = $5 AND payload->>'endpoint' = $2)",
                    &[&ENDPOINTS, &endpoint, &DEPLOYED_APIS, &LINKS, &LOG_SOURCES],
                )?;
                Ok(())
            })
            .map_err(core_postgres_error)
    }

    fn update_endpoint_health(
        &mut self,
        endpoint: &str,
        health: String,
        reachable: bool,
    ) -> orchestrator_core::Result<()> {
        validate_endpoint_id(endpoint)?;
        self.update_record::<Endpoint, _>(ENDPOINTS, endpoint, |value| {
            value.health = health;
            value.reachable = reachable;
        })
    }

    fn list_links(&self) -> orchestrator_core::Result<Vec<Link>> {
        self.list_records(LINKS)
    }

    fn get_link(
        &self,
        source_endpoint: &str,
        target_endpoint: &str,
    ) -> orchestrator_core::Result<Option<Link>> {
        self.get_record(LINKS, &key(&[source_endpoint, target_endpoint]))
    }

    fn upsert_link(&mut self, value: Link) -> orchestrator_core::Result<()> {
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
    ) -> orchestrator_core::Result<()> {
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
    ) -> orchestrator_core::Result<()> {
        validate_endpoint_id(source_endpoint)?;
        validate_endpoint_id(target_endpoint)?;
        self.update_record::<Link, _>(LINKS, &key(&[source_endpoint, target_endpoint]), |value| {
            value.health = health;
            value.latency_ms = latency_ms;
        })
    }

    fn create_operation(&mut self, value: Operation) -> orchestrator_core::Result<()> {
        self.upsert_record(OPERATIONS, &value.operation_id, &value.target_id, &value)
    }

    fn get_operation(&self, operation_id: &str) -> orchestrator_core::Result<Option<Operation>> {
        self.get_record(OPERATIONS, operation_id)
    }

    fn list_operations(&self) -> orchestrator_core::Result<Vec<Operation>> {
        self.list_records(OPERATIONS)
    }

    fn update_operation(&mut self, value: Operation) -> orchestrator_core::Result<()> {
        self.upsert_record(OPERATIONS, &value.operation_id, &value.target_id, &value)
    }

    fn update_operation_status(
        &mut self,
        operation_id: &str,
        status: OperationStatus,
        error_message: String,
    ) -> orchestrator_core::Result<()> {
        self.update_record::<Operation, _>(OPERATIONS, operation_id, |value| {
            value.status = status;
            value.error_message = error_message;
        })
    }

    fn update_operation_result(
        &mut self,
        operation_id: &str,
        result: serde_json::Value,
    ) -> orchestrator_core::Result<()> {
        self.update_record::<Operation, _>(OPERATIONS, operation_id, |value| value.result = result)
    }

    fn append_operation_log(
        &mut self,
        mut value: OperationLogRecord,
    ) -> orchestrator_core::Result<()> {
        self.pool
            .with_transaction(|transaction| {
                if !operation_exists(transaction, &value.operation_id)? {
                    return Err(PostgresError::InvalidConfiguration(format!(
                        "operation log references missing operation {}",
                        value.operation_id
                    )));
                }
                if value.created_at.is_empty() {
                    let sequence: i64 = transaction
                        .query_one(
                            "SELECT nextval(pg_get_serial_sequence('orchestrator_operation_logs_v2', 'sequence'))",
                            &[],
                        )?
                        .get(0);
                    value.created_at = format!("log-{sequence}");
                    let payload = serde_json::to_string(&value)?;
                    transaction.execute(
                        "INSERT INTO orchestrator_operation_logs_v2(sequence, operation_id, payload) VALUES ($1, $2, $3::text::jsonb)",
                        &[&sequence, &value.operation_id, &payload],
                    )?;
                } else {
                    let payload = serde_json::to_string(&value)?;
                    transaction.execute(
                        "INSERT INTO orchestrator_operation_logs_v2(operation_id, payload) VALUES ($1, $2::text::jsonb)",
                        &[&value.operation_id, &payload],
                    )?;
                }
                Ok(())
            })
            .map_err(core_postgres_error)
    }

    fn list_operation_logs(
        &self,
        operation_id: &str,
    ) -> orchestrator_core::Result<Vec<OperationLogRecord>> {
        self.pool
            .with_client(|client| {
                client
                    .query(
                        "SELECT payload::text FROM orchestrator_operation_logs_v2 WHERE operation_id = $1 ORDER BY sequence",
                        &[&operation_id],
                    )?
                    .into_iter()
                    .map(|row| {
                        serde_json::from_str::<OperationLogRecord>(&row.get::<_, String>(0))
                            .map_err(Into::into)
                    })
                    .collect()
            })
            .map_err(core_postgres_error)
    }

    fn acquire_operation_lock(&mut self, value: OperationLock) -> orchestrator_core::Result<bool> {
        self.pool
            .with_transaction(|transaction| {
                if !operation_exists(transaction, &value.operation_id)? {
                    return Err(PostgresError::InvalidConfiguration(format!(
                        "lock references missing operation {}",
                        value.operation_id
                    )));
                }
                let payload = serde_json::to_string(&value)?;
                Ok(transaction.execute(
                    "INSERT INTO orchestrator_records(kind, record_key, scope, payload) VALUES ($1, $2, $3, $4::text::jsonb) ON CONFLICT(kind, record_key) DO NOTHING",
                    &[&OPERATION_LOCKS, &value.lock_key, &value.operation_id, &payload],
                )? > 0)
            })
            .map_err(core_postgres_error)
    }

    fn release_operation_lock(
        &mut self,
        lock_key: &str,
        operation_id: &str,
    ) -> orchestrator_core::Result<()> {
        self.pool
            .with_client(|client| {
                client.execute(
                    "DELETE FROM orchestrator_records WHERE kind = $1 AND record_key = $2 AND scope = $3",
                    &[&OPERATION_LOCKS, &lock_key, &operation_id],
                )?;
                Ok(())
            })
            .map_err(core_postgres_error)
    }

    fn save_topology_snapshot(&mut self, value: TopologySnapshot) -> orchestrator_core::Result<()> {
        validate_topology(&value.topology)?;
        self.upsert_record(
            TOPOLOGY_SNAPSHOTS,
            &value.snapshot_id,
            &value.topology.root_endpoint,
            &value,
        )
    }

    fn get_latest_topology_snapshot(&self) -> orchestrator_core::Result<Option<TopologySnapshot>> {
        self.pool
            .with_client(|client| {
                client
                    .query_opt(
                        "SELECT payload::text FROM orchestrator_records WHERE kind = $1 ORDER BY updated_at DESC, record_key DESC LIMIT 1",
                        &[&TOPOLOGY_SNAPSHOTS],
                    )?
                    .map(|row| {
                        serde_json::from_str::<TopologySnapshot>(&row.get::<_, String>(0))
                            .map_err(Into::into)
                    })
                    .transpose()
            })
            .map_err(core_postgres_error)
    }

    fn build_topology_view(&self) -> orchestrator_core::Result<Topology> {
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

    fn delete_topology(&mut self, root_endpoint: &str) -> orchestrator_core::Result<()> {
        self.delete_scope(TOPOLOGY_SNAPSHOTS, root_endpoint)
    }

    fn list_log_sources(&self) -> orchestrator_core::Result<Vec<LogView>> {
        self.list_records(LOG_SOURCES)
    }

    fn upsert_log_source(&mut self, value: LogView) -> orchestrator_core::Result<()> {
        validate_log_view(&value)?;
        if self.get_endpoint(&value.endpoint)?.is_none() {
            return Err(OrchestratorError::Dependency(format!(
                "log view references missing endpoint {}",
                value.endpoint
            )));
        }
        self.upsert_record(LOG_SOURCES, &value.source_id, &value.endpoint, &value)
    }

    fn delete_log_source(&mut self, source_id: &str) -> orchestrator_core::Result<()> {
        self.delete_record(LOG_SOURCES, source_id).map(|_| ())
    }

    fn create_diagnostic_report(
        &mut self,
        value: DiagnosticReport,
    ) -> orchestrator_core::Result<()> {
        self.upsert_record(DIAGNOSTICS, &value.report_id, &value.target_id, &value)
    }

    fn get_diagnostic_report(
        &self,
        report_id: &str,
    ) -> orchestrator_core::Result<Option<DiagnosticReport>> {
        self.get_record(DIAGNOSTICS, report_id)
    }

    fn list_diagnostic_reports(&self) -> orchestrator_core::Result<Vec<DiagnosticReport>> {
        self.list_records(DIAGNOSTICS)
    }
}
