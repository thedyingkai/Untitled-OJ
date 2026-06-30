use crate::{
    DiagnosticReport, DockerComposeDriver, DriverRequest, DriverResult, Endpoint,
    EndpointHealthResult, EndpointProbe, ExecutionDriver, ExternalEndpointDriver, Link,
    LinkHealthResult, LocalProcessDriver, LogView, Operation, OperationLock, OperationLogRecord,
    OperationStatus, OrchestratorError, RenderedServiceConfig, Result, RuntimeMode,
    ServiceFrontendEntry, ServiceManifest, ServiceMigrationRecord, ServicePermissionRecord,
    ServiceRedisResource, ServiceRelease, ServiceReleaseManifest, ServiceRoute,
    ServiceStorageResource, StaticEndpointProbe, Topology, TopologySnapshot,
    build_diagnostic_report, build_topology, check_endpoint_health_with_probe, check_link_health,
    export_diagnostic_report, operation_log_record, operation_step_log_record, parse_endpoint_id,
    start_operation, succeed_operation, validate_endpoint, validate_endpoint_id, validate_link,
    validate_log_view, validate_rendered_service_config, validate_service_frontend_entry,
    validate_service_manifest, validate_service_migration_record,
    validate_service_permission_record, validate_service_redis_resource,
    validate_service_release_record, validate_service_route, validate_service_storage_resource,
    validate_topology,
};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use std::collections::BTreeMap;

pub trait OrchestratorStore {
    fn list_services(&self) -> Result<Vec<ServiceManifest>>;
    fn get_service(&self, service_id: &str) -> Result<Option<ServiceManifest>>;
    fn upsert_service(&mut self, service: ServiceManifest) -> Result<()>;
    fn delete_service(&mut self, service_id: &str) -> Result<()>;

    fn list_service_releases(&self) -> Result<Vec<ServiceRelease>>;
    fn get_service_release(
        &self,
        service_name: &str,
        version: &str,
    ) -> Result<Option<ServiceRelease>>;
    fn upsert_service_release(&mut self, release: ServiceRelease) -> Result<()>;
    fn delete_service_release(&mut self, service_name: &str, version: &str) -> Result<()>;

    fn list_service_routes(&self) -> Result<Vec<ServiceRoute>>;
    fn upsert_service_route(&mut self, route: ServiceRoute) -> Result<()>;
    fn delete_service_routes_for_service(&mut self, service_name: &str) -> Result<()>;

    fn list_service_migration_records(&self) -> Result<Vec<ServiceMigrationRecord>>;
    fn upsert_service_migration_record(&mut self, record: ServiceMigrationRecord) -> Result<()>;
    fn delete_service_migration_records_for_service(&mut self, service_name: &str) -> Result<()>;

    fn list_service_permission_records(&self) -> Result<Vec<ServicePermissionRecord>>;
    fn upsert_service_permission_record(&mut self, record: ServicePermissionRecord) -> Result<()>;
    fn delete_service_permission_records_for_service(&mut self, service_name: &str) -> Result<()>;

    fn list_service_frontend_entries(&self) -> Result<Vec<ServiceFrontendEntry>>;
    fn upsert_service_frontend_entry(&mut self, entry: ServiceFrontendEntry) -> Result<()>;
    fn delete_service_frontend_entry(&mut self, service_name: &str) -> Result<()>;

    fn list_service_redis_resources(&self) -> Result<Vec<ServiceRedisResource>>;
    fn upsert_service_redis_resource(&mut self, resource: ServiceRedisResource) -> Result<()>;
    fn delete_service_redis_resources_for_service(&mut self, service_name: &str) -> Result<()>;

    fn list_service_storage_resources(&self) -> Result<Vec<ServiceStorageResource>>;
    fn upsert_service_storage_resource(&mut self, resource: ServiceStorageResource) -> Result<()>;
    fn delete_service_storage_resources_for_service(&mut self, service_name: &str) -> Result<()>;

    fn list_rendered_service_configs(&self) -> Result<Vec<RenderedServiceConfig>>;
    fn upsert_rendered_service_config(&mut self, config: RenderedServiceConfig) -> Result<()>;
    fn delete_rendered_service_configs_for_service(&mut self, service_name: &str) -> Result<()>;

    fn list_endpoints(&self) -> Result<Vec<Endpoint>>;
    fn get_endpoint(&self, endpoint: &str) -> Result<Option<Endpoint>>;
    fn upsert_endpoint(&mut self, endpoint: Endpoint) -> Result<()>;
    fn delete_endpoint(&mut self, endpoint: &str) -> Result<()>;
    fn update_endpoint_health(
        &mut self,
        endpoint: &str,
        health: String,
        reachable: bool,
    ) -> Result<()>;

    fn list_links(&self) -> Result<Vec<Link>>;
    fn get_link(&self, source_endpoint: &str, target_endpoint: &str) -> Result<Option<Link>>;
    fn upsert_link(&mut self, link: Link) -> Result<()>;
    fn delete_link(&mut self, source_endpoint: &str, target_endpoint: &str) -> Result<()>;
    fn update_link_health(
        &mut self,
        source_endpoint: &str,
        target_endpoint: &str,
        health: String,
        latency_ms: Option<u32>,
    ) -> Result<()>;

    fn create_operation(&mut self, operation: Operation) -> Result<()>;
    fn get_operation(&self, operation_id: &str) -> Result<Option<Operation>>;
    fn list_operations(&self) -> Result<Vec<Operation>>;
    fn update_operation(&mut self, operation: Operation) -> Result<()>;
    fn update_operation_status(
        &mut self,
        operation_id: &str,
        status: OperationStatus,
        error_message: String,
    ) -> Result<()>;
    fn update_operation_result(
        &mut self,
        operation_id: &str,
        result: serde_json::Value,
    ) -> Result<()>;
    fn append_operation_log(&mut self, record: OperationLogRecord) -> Result<()>;
    fn list_operation_logs(&self, operation_id: &str) -> Result<Vec<OperationLogRecord>>;
    fn acquire_operation_lock(&mut self, lock: OperationLock) -> Result<bool>;
    fn release_operation_lock(&mut self, lock_key: &str, operation_id: &str) -> Result<()>;

    fn save_topology_snapshot(&mut self, snapshot: TopologySnapshot) -> Result<()>;
    fn get_latest_topology_snapshot(&self) -> Result<Option<TopologySnapshot>>;
    fn build_topology_view(&self) -> Result<Topology>;
    fn delete_topology(&mut self, root_endpoint: &str) -> Result<()>;

    fn list_log_sources(&self) -> Result<Vec<LogView>>;
    fn upsert_log_source(&mut self, log_view: LogView) -> Result<()>;
    fn delete_log_source(&mut self, source_id: &str) -> Result<()>;

    fn create_diagnostic_report(&mut self, report: DiagnosticReport) -> Result<()>;
    fn get_diagnostic_report(&self, report_id: &str) -> Result<Option<DiagnosticReport>>;
    fn list_diagnostic_reports(&self) -> Result<Vec<DiagnosticReport>>;

    fn put_service(&mut self, service: ServiceManifest) -> Result<()> {
        self.upsert_service(service)
    }

    fn put_endpoint(&mut self, endpoint: Endpoint) -> Result<()> {
        self.upsert_endpoint(endpoint)
    }

    fn put_link(&mut self, link: Link) -> Result<()> {
        self.upsert_link(link)
    }

    fn put_operation(&mut self, operation: Operation) -> Result<()> {
        self.update_operation(operation)
    }

    fn put_topology(&mut self, topology: Topology) -> Result<()> {
        self.save_topology_snapshot(TopologySnapshot {
            snapshot_id: topology.root_endpoint.clone(),
            topology,
            created_at: String::new(),
        })
    }

    fn put_log_view(&mut self, log_view: LogView) -> Result<()> {
        self.upsert_log_source(log_view)
    }

    fn put_diagnostic_report(&mut self, report: DiagnosticReport) -> Result<()> {
        self.create_diagnostic_report(report)
    }

    fn services(&self) -> Result<Vec<ServiceManifest>> {
        self.list_services()
    }

    fn service_releases(&self) -> Result<Vec<ServiceRelease>> {
        self.list_service_releases()
    }

    fn service_routes(&self) -> Result<Vec<ServiceRoute>> {
        self.list_service_routes()
    }

    fn service_migration_records(&self) -> Result<Vec<ServiceMigrationRecord>> {
        self.list_service_migration_records()
    }

    fn service_permission_records(&self) -> Result<Vec<ServicePermissionRecord>> {
        self.list_service_permission_records()
    }

    fn service_frontend_entries(&self) -> Result<Vec<ServiceFrontendEntry>> {
        self.list_service_frontend_entries()
    }

    fn service_redis_resources(&self) -> Result<Vec<ServiceRedisResource>> {
        self.list_service_redis_resources()
    }

    fn service_storage_resources(&self) -> Result<Vec<ServiceStorageResource>> {
        self.list_service_storage_resources()
    }

    fn rendered_service_configs(&self) -> Result<Vec<RenderedServiceConfig>> {
        self.list_rendered_service_configs()
    }

    fn endpoints(&self) -> Result<Vec<Endpoint>> {
        self.list_endpoints()
    }

    fn links(&self) -> Result<Vec<Link>> {
        self.list_links()
    }

    fn operations(&self) -> Result<Vec<Operation>> {
        self.list_operations()
    }

    fn operation_logs(&self, operation_id: &str) -> Result<Vec<OperationLogRecord>> {
        self.list_operation_logs(operation_id)
    }

    fn log_views(&self) -> Result<Vec<LogView>> {
        self.list_log_sources()
    }

    fn diagnostic_reports(&self) -> Result<Vec<DiagnosticReport>> {
        self.list_diagnostic_reports()
    }
}

#[derive(Debug, Default, Clone)]
pub struct MemoryOrchestratorStore {
    services: BTreeMap<String, ServiceManifest>,
    service_releases: BTreeMap<(String, String), ServiceRelease>,
    service_routes: BTreeMap<(String, String), ServiceRoute>,
    service_migration_records: BTreeMap<(String, String), ServiceMigrationRecord>,
    service_permission_records: BTreeMap<(String, String), ServicePermissionRecord>,
    service_frontend_entries: BTreeMap<String, ServiceFrontendEntry>,
    service_redis_resources: BTreeMap<(String, String), ServiceRedisResource>,
    service_storage_resources: BTreeMap<(String, String, String), ServiceStorageResource>,
    rendered_service_configs: BTreeMap<(String, String), RenderedServiceConfig>,
    endpoints: BTreeMap<String, Endpoint>,
    links: BTreeMap<(String, String), Link>,
    operations: BTreeMap<String, Operation>,
    operation_logs: Vec<OperationLogRecord>,
    topology_snapshots: BTreeMap<String, TopologySnapshot>,
    log_views: BTreeMap<String, LogView>,
    diagnostic_reports: BTreeMap<String, DiagnosticReport>,
    operation_locks: BTreeMap<String, OperationLock>,
}

impl MemoryOrchestratorStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn service(&self, service_id: &str) -> Option<&ServiceManifest> {
        self.services.get(service_id)
    }

    pub fn endpoint(&self, endpoint: &str) -> Option<&Endpoint> {
        self.endpoints.get(endpoint)
    }

    pub fn operation(&self, operation_id: &str) -> Option<&Operation> {
        self.operations.get(operation_id)
    }

    pub fn topology(&self, root_endpoint: &str) -> Option<&Topology> {
        self.topology_snapshots
            .values()
            .find(|snapshot| snapshot.topology.root_endpoint == root_endpoint)
            .map(|snapshot| &snapshot.topology)
    }

    pub fn services(&self) -> Vec<ServiceManifest> {
        self.services.values().cloned().collect()
    }

    pub fn service_releases(&self) -> Vec<ServiceRelease> {
        self.service_releases.values().cloned().collect()
    }

    pub fn service_routes(&self) -> Vec<ServiceRoute> {
        self.service_routes.values().cloned().collect()
    }

    pub fn service_migration_records(&self) -> Vec<ServiceMigrationRecord> {
        self.service_migration_records.values().cloned().collect()
    }

    pub fn service_permission_records(&self) -> Vec<ServicePermissionRecord> {
        self.service_permission_records.values().cloned().collect()
    }

    pub fn service_frontend_entries(&self) -> Vec<ServiceFrontendEntry> {
        self.service_frontend_entries.values().cloned().collect()
    }

    pub fn service_redis_resources(&self) -> Vec<ServiceRedisResource> {
        self.service_redis_resources.values().cloned().collect()
    }

    pub fn service_storage_resources(&self) -> Vec<ServiceStorageResource> {
        self.service_storage_resources.values().cloned().collect()
    }

    pub fn rendered_service_configs(&self) -> Vec<RenderedServiceConfig> {
        self.rendered_service_configs.values().cloned().collect()
    }

    pub fn endpoints(&self) -> Vec<Endpoint> {
        self.endpoints.values().cloned().collect()
    }

    pub fn links(&self) -> Vec<Link> {
        self.links.values().cloned().collect()
    }

    pub fn operations(&self) -> Vec<Operation> {
        self.operations.values().cloned().collect()
    }

    pub fn operation_logs(&self, operation_id: &str) -> Vec<OperationLogRecord> {
        self.operation_logs
            .iter()
            .filter(|record| record.operation_id == operation_id)
            .cloned()
            .collect()
    }

    pub fn topologies(&self) -> Vec<Topology> {
        self.topology_snapshots
            .values()
            .map(|snapshot| snapshot.topology.clone())
            .collect()
    }

    pub fn log_views(&self) -> Vec<LogView> {
        self.log_views.values().cloned().collect()
    }

    pub fn diagnostic_reports(&self) -> Vec<DiagnosticReport> {
        self.diagnostic_reports.values().cloned().collect()
    }
}

impl OrchestratorStore for MemoryOrchestratorStore {
    fn list_services(&self) -> Result<Vec<ServiceManifest>> {
        Ok(self.services())
    }

    fn get_service(&self, service_id: &str) -> Result<Option<ServiceManifest>> {
        Ok(self.services.get(service_id).cloned())
    }

    fn upsert_service(&mut self, service: ServiceManifest) -> Result<()> {
        validate_service_manifest(&service)?;
        self.services.insert(service.id.clone(), service);
        Ok(())
    }

    fn delete_service(&mut self, service_id: &str) -> Result<()> {
        self.services.remove(service_id);
        self.service_releases
            .retain(|(service_name, _), _| service_name != service_id);
        self.delete_service_routes_for_service(service_id)?;
        self.delete_service_migration_records_for_service(service_id)?;
        self.delete_service_permission_records_for_service(service_id)?;
        self.delete_service_frontend_entry(service_id)?;
        self.delete_service_redis_resources_for_service(service_id)?;
        self.delete_service_storage_resources_for_service(service_id)?;
        self.delete_rendered_service_configs_for_service(service_id)?;
        let removed_endpoints = self
            .endpoints
            .values()
            .filter(|endpoint| endpoint.service_id == service_id)
            .map(|endpoint| endpoint.endpoint.clone())
            .collect::<Vec<_>>();
        for endpoint in removed_endpoints {
            self.delete_endpoint(&endpoint)?;
        }
        Ok(())
    }

    fn list_service_releases(&self) -> Result<Vec<ServiceRelease>> {
        Ok(self.service_releases())
    }

    fn get_service_release(
        &self,
        service_name: &str,
        version: &str,
    ) -> Result<Option<ServiceRelease>> {
        Ok(self
            .service_releases
            .get(&(service_name.to_string(), version.to_string()))
            .cloned())
    }

    fn upsert_service_release(&mut self, release: ServiceRelease) -> Result<()> {
        validate_service_release_record(&release)?;
        self.service_releases.insert(
            (release.service_name.clone(), release.version.clone()),
            release,
        );
        Ok(())
    }

    fn delete_service_release(&mut self, service_name: &str, version: &str) -> Result<()> {
        self.service_releases
            .remove(&(service_name.to_string(), version.to_string()));
        Ok(())
    }

    fn list_service_routes(&self) -> Result<Vec<ServiceRoute>> {
        Ok(self.service_routes())
    }

    fn upsert_service_route(&mut self, route: ServiceRoute) -> Result<()> {
        validate_service_route(&route)?;
        self.service_routes
            .insert((route.path.clone(), route.method.clone()), route);
        Ok(())
    }

    fn delete_service_routes_for_service(&mut self, service_name: &str) -> Result<()> {
        self.service_routes
            .retain(|_, route| route.target_service_name != service_name);
        Ok(())
    }

    fn list_service_migration_records(&self) -> Result<Vec<ServiceMigrationRecord>> {
        Ok(self.service_migration_records())
    }

    fn upsert_service_migration_record(&mut self, record: ServiceMigrationRecord) -> Result<()> {
        validate_service_migration_record(&record)?;
        self.service_migration_records.insert(
            (
                record.service_name.clone(),
                record.migration_version.clone(),
            ),
            record,
        );
        Ok(())
    }

    fn delete_service_migration_records_for_service(&mut self, service_name: &str) -> Result<()> {
        self.service_migration_records
            .retain(|(stored_service_name, _), _| stored_service_name != service_name);
        Ok(())
    }

    fn list_service_permission_records(&self) -> Result<Vec<ServicePermissionRecord>> {
        Ok(self.service_permission_records())
    }

    fn upsert_service_permission_record(&mut self, record: ServicePermissionRecord) -> Result<()> {
        validate_service_permission_record(&record)?;
        self.service_permission_records.insert(
            (record.service_name.clone(), record.permission_key.clone()),
            record,
        );
        Ok(())
    }

    fn delete_service_permission_records_for_service(&mut self, service_name: &str) -> Result<()> {
        self.service_permission_records
            .retain(|(stored_service_name, _), _| stored_service_name != service_name);
        Ok(())
    }

    fn list_service_frontend_entries(&self) -> Result<Vec<ServiceFrontendEntry>> {
        Ok(self.service_frontend_entries())
    }

    fn upsert_service_frontend_entry(&mut self, entry: ServiceFrontendEntry) -> Result<()> {
        validate_service_frontend_entry(&entry)?;
        self.service_frontend_entries
            .insert(entry.service_name.clone(), entry);
        Ok(())
    }

    fn delete_service_frontend_entry(&mut self, service_name: &str) -> Result<()> {
        self.service_frontend_entries.remove(service_name);
        Ok(())
    }

    fn list_service_redis_resources(&self) -> Result<Vec<ServiceRedisResource>> {
        Ok(self.service_redis_resources())
    }

    fn upsert_service_redis_resource(&mut self, resource: ServiceRedisResource) -> Result<()> {
        validate_service_redis_resource(&resource)?;
        self.service_redis_resources.insert(
            (resource.service_name.clone(), resource.name.clone()),
            resource,
        );
        Ok(())
    }

    fn delete_service_redis_resources_for_service(&mut self, service_name: &str) -> Result<()> {
        self.service_redis_resources
            .retain(|(stored_service_name, _), _| stored_service_name != service_name);
        Ok(())
    }

    fn list_service_storage_resources(&self) -> Result<Vec<ServiceStorageResource>> {
        Ok(self.service_storage_resources())
    }

    fn upsert_service_storage_resource(&mut self, resource: ServiceStorageResource) -> Result<()> {
        validate_service_storage_resource(&resource)?;
        self.service_storage_resources.insert(
            (
                resource.service_name.clone(),
                resource.object_type.clone(),
                resource.bucket.clone(),
            ),
            resource,
        );
        Ok(())
    }

    fn delete_service_storage_resources_for_service(&mut self, service_name: &str) -> Result<()> {
        self.service_storage_resources
            .retain(|(stored_service_name, _, _), _| stored_service_name != service_name);
        Ok(())
    }

    fn list_rendered_service_configs(&self) -> Result<Vec<RenderedServiceConfig>> {
        Ok(self.rendered_service_configs())
    }

    fn upsert_rendered_service_config(&mut self, config: RenderedServiceConfig) -> Result<()> {
        validate_rendered_service_config(&config)?;
        self.rendered_service_configs.insert(
            (config.service_name.clone(), config.version.clone()),
            config,
        );
        Ok(())
    }

    fn delete_rendered_service_configs_for_service(&mut self, service_name: &str) -> Result<()> {
        self.rendered_service_configs
            .retain(|(stored_service_name, _), _| stored_service_name != service_name);
        Ok(())
    }

    fn list_endpoints(&self) -> Result<Vec<Endpoint>> {
        Ok(self.endpoints())
    }

    fn get_endpoint(&self, endpoint: &str) -> Result<Option<Endpoint>> {
        Ok(self.endpoints.get(endpoint).cloned())
    }

    fn upsert_endpoint(&mut self, endpoint: Endpoint) -> Result<()> {
        validate_endpoint(&endpoint)?;
        if !self.services.contains_key(&endpoint.service_id) {
            return Err(OrchestratorError::Dependency(format!(
                "endpoint references missing service {}",
                endpoint.service_id
            )));
        }
        self.endpoints.insert(endpoint.endpoint.clone(), endpoint);
        Ok(())
    }

    fn delete_endpoint(&mut self, endpoint: &str) -> Result<()> {
        validate_endpoint_id(endpoint)?;
        self.endpoints.remove(endpoint);
        self.links
            .retain(|(source, target), _| source != endpoint && target != endpoint);
        self.log_views
            .retain(|_, log_view| log_view.endpoint != endpoint);
        Ok(())
    }

    fn update_endpoint_health(
        &mut self,
        endpoint: &str,
        health: String,
        reachable: bool,
    ) -> Result<()> {
        validate_endpoint_id(endpoint)?;
        let item = self.endpoints.get_mut(endpoint).ok_or_else(|| {
            OrchestratorError::Dependency(format!("endpoint {endpoint} not found"))
        })?;
        item.health = health;
        item.reachable = reachable;
        Ok(())
    }

    fn list_links(&self) -> Result<Vec<Link>> {
        Ok(self.links())
    }

    fn get_link(&self, source_endpoint: &str, target_endpoint: &str) -> Result<Option<Link>> {
        Ok(self
            .links
            .get(&(source_endpoint.to_string(), target_endpoint.to_string()))
            .cloned())
    }

    fn upsert_link(&mut self, link: Link) -> Result<()> {
        let endpoints = self.endpoints();
        validate_link(&link, &endpoints)?;
        self.links.insert(
            (link.source_endpoint.clone(), link.target_endpoint.clone()),
            link,
        );
        Ok(())
    }

    fn delete_link(&mut self, source_endpoint: &str, target_endpoint: &str) -> Result<()> {
        validate_endpoint_id(source_endpoint)?;
        validate_endpoint_id(target_endpoint)?;
        self.links
            .remove(&(source_endpoint.to_string(), target_endpoint.to_string()))
            .map(|_| ())
            .ok_or_else(|| {
                OrchestratorError::Dependency(format!(
                    "link {source_endpoint} -> {target_endpoint} not found"
                ))
            })
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
        let item = self
            .links
            .get_mut(&(source_endpoint.to_string(), target_endpoint.to_string()))
            .ok_or_else(|| {
                OrchestratorError::Dependency(format!(
                    "link {source_endpoint} -> {target_endpoint} not found"
                ))
            })?;
        item.health = health;
        item.latency_ms = latency_ms;
        Ok(())
    }

    fn create_operation(&mut self, operation: Operation) -> Result<()> {
        self.operations
            .insert(operation.operation_id.clone(), operation);
        Ok(())
    }

    fn get_operation(&self, operation_id: &str) -> Result<Option<Operation>> {
        Ok(self.operations.get(operation_id).cloned())
    }

    fn list_operations(&self) -> Result<Vec<Operation>> {
        Ok(self.operations())
    }

    fn update_operation(&mut self, operation: Operation) -> Result<()> {
        self.operations
            .insert(operation.operation_id.clone(), operation);
        Ok(())
    }

    fn update_operation_status(
        &mut self,
        operation_id: &str,
        status: OperationStatus,
        error_message: String,
    ) -> Result<()> {
        let operation = self.operations.get_mut(operation_id).ok_or_else(|| {
            OrchestratorError::Dependency(format!("operation {operation_id} not found"))
        })?;
        operation.status = status;
        operation.error_message = error_message;
        Ok(())
    }

    fn update_operation_result(
        &mut self,
        operation_id: &str,
        result: serde_json::Value,
    ) -> Result<()> {
        let operation = self.operations.get_mut(operation_id).ok_or_else(|| {
            OrchestratorError::Dependency(format!("operation {operation_id} not found"))
        })?;
        operation.result = result;
        Ok(())
    }

    fn append_operation_log(&mut self, record: OperationLogRecord) -> Result<()> {
        if !self.operations.contains_key(&record.operation_id) {
            return Err(OrchestratorError::Dependency(format!(
                "operation log references missing operation {}",
                record.operation_id
            )));
        }
        let mut record = record;
        if record.created_at.is_empty() {
            record.created_at = format!("log-{}", self.operation_logs.len() + 1);
        }
        self.operation_logs.push(record);
        Ok(())
    }

    fn list_operation_logs(&self, operation_id: &str) -> Result<Vec<OperationLogRecord>> {
        Ok(self.operation_logs(operation_id))
    }

    fn acquire_operation_lock(&mut self, lock: OperationLock) -> Result<bool> {
        if !self.operations.contains_key(&lock.operation_id) {
            return Err(OrchestratorError::Dependency(format!(
                "lock references missing operation {}",
                lock.operation_id
            )));
        }
        if self.operation_locks.contains_key(&lock.lock_key) {
            return Ok(false);
        }
        self.operation_locks.insert(lock.lock_key.clone(), lock);
        Ok(true)
    }

    fn release_operation_lock(&mut self, lock_key: &str, operation_id: &str) -> Result<()> {
        if self
            .operation_locks
            .get(lock_key)
            .is_some_and(|lock| lock.operation_id == operation_id)
        {
            self.operation_locks.remove(lock_key);
        }
        Ok(())
    }

    fn save_topology_snapshot(&mut self, snapshot: TopologySnapshot) -> Result<()> {
        validate_topology(&snapshot.topology)?;
        self.topology_snapshots
            .insert(snapshot.snapshot_id.clone(), snapshot);
        Ok(())
    }

    fn get_latest_topology_snapshot(&self) -> Result<Option<TopologySnapshot>> {
        Ok(self.topology_snapshots.values().last().cloned())
    }

    fn build_topology_view(&self) -> Result<Topology> {
        let endpoints = self.endpoints();
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
            self.services.keys().cloned().collect(),
            endpoints,
            self.links(),
            self.operations(),
            self.log_views(),
            self.diagnostic_reports(),
        )
    }

    fn delete_topology(&mut self, root_endpoint: &str) -> Result<()> {
        self.topology_snapshots
            .retain(|_, snapshot| snapshot.topology.root_endpoint != root_endpoint);
        Ok(())
    }

    fn list_log_sources(&self) -> Result<Vec<LogView>> {
        Ok(self.log_views())
    }

    fn upsert_log_source(&mut self, log_view: LogView) -> Result<()> {
        validate_log_view(&log_view)?;
        if !self.endpoints.contains_key(&log_view.endpoint) {
            return Err(OrchestratorError::Dependency(format!(
                "log view references missing endpoint {}",
                log_view.endpoint
            )));
        }
        self.log_views.insert(log_view.source_id.clone(), log_view);
        Ok(())
    }

    fn delete_log_source(&mut self, source_id: &str) -> Result<()> {
        self.log_views.remove(source_id);
        Ok(())
    }

    fn create_diagnostic_report(&mut self, report: DiagnosticReport) -> Result<()> {
        self.diagnostic_reports
            .insert(report.report_id.clone(), report);
        Ok(())
    }

    fn get_diagnostic_report(&self, report_id: &str) -> Result<Option<DiagnosticReport>> {
        Ok(self.diagnostic_reports.get(report_id).cloned())
    }

    fn list_diagnostic_reports(&self) -> Result<Vec<DiagnosticReport>> {
        Ok(self.diagnostic_reports())
    }
}

pub struct OperationExecutor<'a, S: OrchestratorStore, P: EndpointProbe = StaticEndpointProbe> {
    store: &'a mut S,
    endpoint_probe: P,
    service_driver_execution_enabled: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
struct ReleaseInstallPreviousState {
    service: Option<ServiceManifest>,
    releases: Vec<ServiceRelease>,
    routes: Vec<ServiceRoute>,
    migrations: Vec<ServiceMigrationRecord>,
    permissions: Vec<ServicePermissionRecord>,
    frontends: Vec<ServiceFrontendEntry>,
    redis: Vec<ServiceRedisResource>,
    storage: Vec<ServiceStorageResource>,
    configs: Vec<RenderedServiceConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
struct ReleaseRecordPreviousState {
    release: Option<ServiceRelease>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
struct RegistryResourcePreviousState {
    routes: Vec<ServiceRoute>,
    migrations: Vec<ServiceMigrationRecord>,
    permissions: Vec<ServicePermissionRecord>,
    frontends: Vec<ServiceFrontendEntry>,
    redis: Vec<ServiceRedisResource>,
    storage: Vec<ServiceStorageResource>,
    configs: Vec<RenderedServiceConfig>,
}

impl<'a, S: OrchestratorStore> OperationExecutor<'a, S, StaticEndpointProbe> {
    pub fn new(store: &'a mut S) -> Self {
        Self {
            store,
            endpoint_probe: StaticEndpointProbe,
            service_driver_execution_enabled: false,
        }
    }
}

impl<'a, S: OrchestratorStore, P: EndpointProbe> OperationExecutor<'a, S, P> {
    pub fn with_endpoint_probe(store: &'a mut S, endpoint_probe: P) -> Self {
        Self {
            store,
            endpoint_probe,
            service_driver_execution_enabled: false,
        }
    }

    pub fn with_service_driver_execution_enabled(mut self) -> Self {
        self.service_driver_execution_enabled = true;
        self
    }

    pub fn apply(&mut self, operation_id: &str) -> Result<Operation> {
        let operation = self
            .store
            .get_operation(operation_id)?
            .ok_or_else(|| OrchestratorError::Dependency("operation not found".to_string()))?;
        let requires_confirmation = operation
            .plan
            .get("requires_confirmation")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(true);
        let can_apply = if requires_confirmation {
            matches!(operation.status, OperationStatus::AwaitingConfirmation)
        } else {
            matches!(
                operation.status,
                OperationStatus::Planned | OperationStatus::AwaitingConfirmation
            )
        };
        if !can_apply {
            return Err(OrchestratorError::Blocked(format!(
                "operation status {:?} cannot apply under current confirmation rule",
                operation.status
            )));
        }
        if operation
            .plan
            .get("steps")
            .and_then(serde_json::Value::as_array)
            .is_none_or(Vec::is_empty)
        {
            return Err(OrchestratorError::Blocked(
                "operation plan must contain at least one step".to_string(),
            ));
        }

        let lock_key = format!("operation:{operation_id}");
        let acquired = self.store.acquire_operation_lock(OperationLock {
            lock_key: lock_key.clone(),
            operation_id: operation_id.to_string(),
            owner: "orchestrator-core".to_string(),
            expires_at: "session".to_string(),
            created_at: String::new(),
        })?;
        if !acquired {
            return Err(OrchestratorError::Blocked(format!(
                "operation {operation_id} is locked"
            )));
        }

        let result = self.apply_with_acquired_lock(&operation);
        self.store.release_operation_lock(&lock_key, operation_id)?;
        result
    }

    fn apply_with_acquired_lock(&mut self, operation: &Operation) -> Result<Operation> {
        let running = start_operation(operation)?;
        self.store.update_operation(running.clone())?;
        self.store.append_operation_log(operation_log_record(
            &running.operation_id,
            "info",
            format!("operation {} started", running.action),
        ))?;
        for (index, step) in operation_steps(&running).iter().enumerate() {
            self.store.append_operation_log(operation_step_log_record(
                &running.operation_id,
                step_id(step, index),
                "info",
                format!("step {} planned", step_label(step)),
                step.clone(),
            ))?;
        }

        let result = match self.apply_operation_mutation(&running) {
            Ok(changed_objects) => {
                let operation_after_mutation = self
                    .store
                    .get_operation(&running.operation_id)?
                    .unwrap_or_else(|| running.clone());
                let result = serde_json::json!({
                    "operation_id": running.operation_id,
                    "status": "SUCCEEDED",
                    "started_at": running.started_at,
                    "finished_at": "finished",
                    "changed_objects": changed_objects,
                    "topology_snapshot_id": serde_json::Value::Null,
                });
                let succeeded = succeed_operation(&operation_after_mutation, result)?;
                self.store.update_operation(succeeded.clone())?;
                self.store.append_operation_log(operation_log_record(
                    &succeeded.operation_id,
                    "info",
                    format!("operation {} succeeded", succeeded.action),
                ))?;
                Ok(succeeded)
            }
            Err(err) => {
                let operation_after_mutation = self
                    .store
                    .get_operation(&running.operation_id)?
                    .unwrap_or_else(|| running.clone());
                let failed = crate::fail_operation(&operation_after_mutation, err.to_string())?;
                self.store.update_operation(failed.clone())?;
                self.store.append_operation_log(operation_log_record(
                    &failed.operation_id,
                    "error",
                    format!(
                        "operation {} failed: {}",
                        failed.action, failed.error_message
                    ),
                ))?;
                Err(err)
            }
        };
        result
    }

    pub fn rollback(&mut self, operation_id: &str) -> Result<Operation> {
        let operation = self
            .store
            .get_operation(operation_id)?
            .ok_or_else(|| OrchestratorError::Dependency("operation not found".to_string()))?;
        if !matches!(
            operation.status,
            OperationStatus::Failed | OperationStatus::Succeeded
        ) {
            return Err(OrchestratorError::Blocked(format!(
                "operation status {:?} cannot rollback",
                operation.status
            )));
        }
        if operation.rollback_plan.is_null() {
            return Err(OrchestratorError::Blocked(
                "operation rollback plan is not available".to_string(),
            ));
        }
        let lock_key = format!("operation:{operation_id}");
        let acquired = self.store.acquire_operation_lock(OperationLock {
            lock_key: lock_key.clone(),
            operation_id: operation_id.to_string(),
            owner: "orchestrator-core".to_string(),
            expires_at: "session".to_string(),
            created_at: String::new(),
        })?;
        if !acquired {
            return Err(OrchestratorError::Blocked(format!(
                "operation {operation_id} is locked"
            )));
        }

        let result = self.rollback_with_acquired_lock(&operation);
        self.store.release_operation_lock(&lock_key, operation_id)?;
        result
    }

    fn rollback_with_acquired_lock(&mut self, operation: &Operation) -> Result<Operation> {
        let operation_id = operation.operation_id.as_str();
        let prior_logs = self.store.list_operation_logs(operation_id)?;
        self.store.append_operation_log(operation_log_record(
            &operation.operation_id,
            "info",
            format!("rollback loaded {} prior operation logs", prior_logs.len()),
        ))?;
        for (index, step) in rollback_steps(&operation).iter().enumerate() {
            self.store.append_operation_log(operation_step_log_record(
                &operation.operation_id,
                format!("rollback:{}", step_id(step, index)),
                "info",
                format!("rollback step {} planned", step_label(step)),
                step.clone(),
            ))?;
        }
        let changed_objects = match self.rollback_operation_mutation(&operation) {
            Ok(changed_objects) => changed_objects,
            Err(err) => {
                self.store.append_operation_log(operation_log_record(
                    &operation.operation_id,
                    "error",
                    format!("operation {} rollback failed: {err}", operation.action),
                ))?;
                return Err(err);
            }
        };

        let result = serde_json::json!({
            "operation_id": operation.operation_id,
            "status": "ROLLED_BACK",
            "started_at": operation.started_at,
            "finished_at": "rolled_back",
            "changed_objects": changed_objects,
            "topology_snapshot_id": serde_json::Value::Null,
        });
        let rolled_back = crate::rollback_operation(&operation, result)?;
        self.store.update_operation(rolled_back.clone())?;
        self.store.append_operation_log(operation_log_record(
            &rolled_back.operation_id,
            "info",
            format!("operation {} rolled back", rolled_back.action),
        ))?;
        Ok(rolled_back)
    }

    fn apply_operation_mutation(
        &mut self,
        operation: &Operation,
    ) -> Result<Vec<serde_json::Value>> {
        let mut changed = Vec::new();
        match operation.action.as_str() {
            "release.create" | "release.update" => {
                let release = service_release_record_from_operation(operation)?;
                self.capture_release_record_previous_state(
                    operation,
                    &release.service_name,
                    &release.version,
                )?;
                self.store.upsert_service_release(release.clone())?;
                changed.push(changed_object(
                    "ServiceRelease",
                    &format!("{}@{}", release.service_name, release.version),
                ));
            }
            "release.install" => {
                let service: ServiceManifest = request_value(operation, "service_manifest")?;
                let release = release_manifest_from_operation(operation)?;
                let _previous_state =
                    self.capture_release_install_previous_state(operation, &service.id)?;
                if let Some(release) = release.as_ref() {
                    self.store
                        .upsert_service_release(service_release_record(release)?)?;
                    self.clear_release_resource_registries(&release.service_name)?;
                    for route in service_routes_from_release(release)? {
                        self.store.upsert_service_route(route)?;
                    }
                    for record in service_migration_records_from_release(release) {
                        self.store.upsert_service_migration_record(record)?;
                    }
                    for record in service_permission_records_from_release(release) {
                        self.store.upsert_service_permission_record(record)?;
                    }
                    self.store.upsert_service_frontend_entry(
                        service_frontend_entry_from_release(release)?,
                    )?;
                    for resource in service_redis_resources_from_release(release) {
                        self.store.upsert_service_redis_resource(resource)?;
                    }
                    for resource in service_storage_resources_from_release(release) {
                        self.store.upsert_service_storage_resource(resource)?;
                    }
                    self.store
                        .upsert_rendered_service_config(rendered_config_from_release(release)?)?;
                }
                self.store.put_service(service.clone())?;
                changed.extend(release_changed_objects(&service, release.as_ref()));
                if let Some(release) = release.as_ref() {
                    self.store.append_operation_log(release_install_log_record(
                        &operation.operation_id,
                        release,
                    ))?;
                }
                changed.push(changed_object("Service", &service.id));
            }
            "release.delete" => {
                let service_name = operation
                    .request
                    .get("service_id")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or(operation.target_id.as_str());
                let version = operation
                    .request
                    .get("version")
                    .and_then(serde_json::Value::as_str)
                    .filter(|value| !value.trim().is_empty());
                let _previous_state =
                    self.capture_release_install_previous_state(operation, service_name)?;
                match version {
                    Some(version) => {
                        self.store.delete_service_release(service_name, version)?;
                        changed.push(changed_object(
                            "ServiceRelease",
                            &format!("{service_name}@{version}"),
                        ));
                    }
                    None => {
                        for release in self
                            .store
                            .list_service_releases()?
                            .into_iter()
                            .filter(|release| release.service_name == service_name)
                        {
                            self.store
                                .delete_service_release(&release.service_name, &release.version)?;
                            changed.push(changed_object(
                                "ServiceRelease",
                                &format!("{}@{}", release.service_name, release.version),
                            ));
                        }
                    }
                }
                self.clear_release_resource_registries(service_name)?;
                changed.push(changed_object("ReleaseRegistry", service_name));
            }
            "release.rollback" => {
                let target_operation_id = self.resolve_release_rollback_target(operation)?;
                let rolled_back = self.rollback(&target_operation_id)?;
                self.store.append_operation_log(operation_step_log_record(
                    &operation.operation_id,
                    "release.rollback",
                    "info",
                    format!("release rollback dispatched to {target_operation_id}"),
                    serde_json::json!({
                        "target_operation_id": target_operation_id,
                        "target_status": rolled_back.result.get("status").and_then(serde_json::Value::as_str).unwrap_or("ROLLED_BACK"),
                    }),
                ))?;
                changed.extend(
                    rolled_back
                        .result
                        .get("changed_objects")
                        .and_then(serde_json::Value::as_array)
                        .into_iter()
                        .flatten()
                        .cloned(),
                );
                changed.push(changed_object("ServiceRelease", &operation.target_id));
            }
            "service.enable" | "service.disable" | "service.start" | "service.stop"
            | "service.restart" => {
                let service = ensure_service_exists(self.store, operation.target_id.as_str())?;
                let driver_result = execute_service_driver_action(
                    &service,
                    operation,
                    self.service_driver_execution_enabled,
                )?;
                self.store.append_operation_log(driver_result_log_record(
                    &operation.operation_id,
                    &driver_result,
                ))?;
                ensure_driver_result_succeeded(&driver_result)?;
                changed.push(changed_object("Service", &operation.target_id));
            }
            "service.delete" => {
                let service = ensure_service_exists(self.store, operation.target_id.as_str())?;
                let driver_result = execute_service_driver_action(
                    &service,
                    operation,
                    self.service_driver_execution_enabled,
                )?;
                self.store.append_operation_log(driver_result_log_record(
                    &operation.operation_id,
                    &driver_result,
                ))?;
                ensure_driver_result_succeeded(&driver_result)?;
                self.store.delete_service(&operation.target_id)?;
                changed.push(changed_object("Service", &operation.target_id));
            }
            "log.create" => {
                if let Some(log_view) = log_view_from_operation(operation) {
                    self.store.put_log_view(log_view.clone())?;
                    self.store.append_operation_log(log_view_log_record(
                        &operation.operation_id,
                        &log_view,
                    ))?;
                    changed.push(changed_object("LogView", &log_view.source_id));
                }
            }
            "service.health.check" => {
                ensure_service_exists(self.store, operation.target_id.as_str())?;
                let requested_endpoint = operation
                    .request
                    .get("endpoint")
                    .and_then(serde_json::Value::as_str)
                    .filter(|value| !value.is_empty());
                if let Some(endpoint_id) = requested_endpoint {
                    let endpoint = self.store.get_endpoint(endpoint_id)?.ok_or_else(|| {
                        OrchestratorError::Dependency(format!("endpoint {endpoint_id} not found"))
                    })?;
                    if endpoint.service_id != operation.target_id {
                        return Err(OrchestratorError::Dependency(format!(
                            "endpoint {endpoint_id} does not belong to service {}",
                            operation.target_id
                        )));
                    }
                    self.probe_endpoint_and_persist(&operation.operation_id, &endpoint)?;
                    changed.push(changed_object("Endpoint", endpoint_id));
                } else {
                    let endpoints = self
                        .store
                        .list_endpoints()?
                        .into_iter()
                        .filter(|endpoint| endpoint.service_id == operation.target_id)
                        .collect::<Vec<_>>();
                    if endpoints.is_empty() {
                        return Err(OrchestratorError::Dependency(format!(
                            "service {} has no registered endpoints",
                            operation.target_id
                        )));
                    }
                    for endpoint in endpoints {
                        self.probe_endpoint_and_persist(&operation.operation_id, &endpoint)?;
                        changed.push(changed_object("Endpoint", &endpoint.endpoint));
                    }
                }
            }
            "endpoint.create" | "endpoint.update" => {
                let endpoint = endpoint_from_operation(operation, self.store)?;
                let endpoint_id = endpoint.endpoint.clone();
                self.store.put_endpoint(endpoint)?;
                changed.push(changed_object("Endpoint", &endpoint_id));
            }
            "endpoint.delete" => {
                self.store.delete_endpoint(&operation.target_id)?;
                changed.push(changed_object("Endpoint", &operation.target_id));
            }
            "endpoint.health.check" => {
                let endpoint_id = operation.target_id.as_str();
                let endpoint = self.store.get_endpoint(endpoint_id)?.ok_or_else(|| {
                    OrchestratorError::Dependency(format!("endpoint {endpoint_id} not found"))
                })?;
                self.probe_endpoint_and_persist(&operation.operation_id, &endpoint)?;
                changed.push(changed_object("Endpoint", endpoint_id));
            }
            "link.create" | "link.update" => {
                let link = link_from_operation(operation);
                let target = link_target_id(&link);
                self.store.put_link(link)?;
                changed.push(changed_object("Link", &target));
            }
            "link.delete" => {
                let link = link_from_operation(operation);
                self.store
                    .delete_link(&link.source_endpoint, &link.target_endpoint)?;
                changed.push(changed_object("Link", &link_target_id(&link)));
            }
            "link.health.check" => {
                let requested = link_from_operation(operation);
                let link = self
                    .store
                    .get_link(&requested.source_endpoint, &requested.target_endpoint)?
                    .ok_or_else(|| {
                        OrchestratorError::Dependency(format!(
                            "link {} not found",
                            link_target_id(&requested)
                        ))
                    })?;
                let target = link_target_id(&link);
                let endpoints = self.store.list_endpoints()?;
                let target_health = if let Some(endpoint) = endpoints
                    .iter()
                    .find(|endpoint| endpoint.endpoint == link.target_endpoint)
                {
                    self.probe_endpoint_and_persist(&operation.operation_id, endpoint)?
                } else {
                    missing_target_health(&link)
                };
                let link_health = check_link_health(&link, &endpoints, &target_health)?;
                self.store.update_link_health(
                    &link.source_endpoint,
                    &link.target_endpoint,
                    link_health.health.clone(),
                    link_health.latency_ms,
                )?;
                self.store.append_operation_log(link_health_log_record(
                    &operation.operation_id,
                    &link_health,
                ))?;
                changed.push(changed_object("Link", &target));
            }
            "route.create" | "route.update" => {
                let route: ServiceRoute = request_value(operation, "resource")?;
                self.capture_registry_resource_previous_state(
                    operation,
                    &route.target_service_name,
                )?;
                self.store.upsert_service_route(route.clone())?;
                changed.push(changed_object("Route", &route_id(&route)));
            }
            "route.delete" => {
                let service_name = service_name_for_route_target(self.store, &operation.target_id)?;
                self.capture_registry_resource_previous_state(operation, &service_name)?;
                delete_service_route(self.store, &service_name, &operation.target_id)?;
                changed.push(changed_object("Route", &operation.target_id));
            }
            "frontend.create" | "frontend.update" => {
                let frontend: ServiceFrontendEntry = request_value(operation, "resource")?;
                self.capture_registry_resource_previous_state(operation, &frontend.service_name)?;
                self.store.upsert_service_frontend_entry(frontend.clone())?;
                changed.push(changed_object("Frontend", &frontend_id(&frontend)));
            }
            "frontend.delete" => {
                let service_name =
                    service_name_for_frontend_target(self.store, &operation.target_id)?;
                self.capture_registry_resource_previous_state(operation, &service_name)?;
                self.store.delete_service_frontend_entry(&service_name)?;
                changed.push(changed_object("Frontend", &operation.target_id));
            }
            "migration.create" | "migration.update" => {
                let migration: ServiceMigrationRecord = request_value(operation, "resource")?;
                self.capture_registry_resource_previous_state(operation, &migration.service_name)?;
                self.store
                    .upsert_service_migration_record(migration.clone())?;
                changed.push(changed_object("MigrationRecord", &migration_id(&migration)));
            }
            "migration.delete" => {
                let service_name =
                    service_name_for_migration_target(self.store, &operation.target_id)?;
                self.capture_registry_resource_previous_state(operation, &service_name)?;
                delete_service_migration(self.store, &service_name, &operation.target_id)?;
                changed.push(changed_object("MigrationRecord", &operation.target_id));
            }
            "permission.create" | "permission.update" => {
                let permission: ServicePermissionRecord = request_value(operation, "resource")?;
                self.capture_registry_resource_previous_state(operation, &permission.service_name)?;
                self.store
                    .upsert_service_permission_record(permission.clone())?;
                changed.push(changed_object("Permission", &permission.permission_key));
            }
            "permission.delete" => {
                let service_name =
                    service_name_for_permission_target(self.store, &operation.target_id)?;
                self.capture_registry_resource_previous_state(operation, &service_name)?;
                delete_service_permission(self.store, &service_name, &operation.target_id)?;
                changed.push(changed_object("Permission", &operation.target_id));
            }
            "redis.create" | "redis.update" => {
                let redis: ServiceRedisResource = request_value(operation, "resource")?;
                self.capture_registry_resource_previous_state(operation, &redis.service_name)?;
                self.store.upsert_service_redis_resource(redis.clone())?;
                changed.push(changed_object("RedisResource", &redis_id(&redis)));
            }
            "redis.delete" => {
                let service_name = service_name_for_redis_target(self.store, &operation.target_id)?;
                self.capture_registry_resource_previous_state(operation, &service_name)?;
                delete_service_redis(self.store, &service_name, &operation.target_id)?;
                changed.push(changed_object("RedisResource", &operation.target_id));
            }
            "storage.create" | "storage.update" => {
                let storage: ServiceStorageResource = request_value(operation, "resource")?;
                self.capture_registry_resource_previous_state(operation, &storage.service_name)?;
                self.store
                    .upsert_service_storage_resource(storage.clone())?;
                changed.push(changed_object("StorageResource", &storage_id(&storage)));
            }
            "storage.delete" => {
                let service_name =
                    service_name_for_storage_target(self.store, &operation.target_id)?;
                self.capture_registry_resource_previous_state(operation, &service_name)?;
                delete_service_storage(self.store, &service_name, &operation.target_id)?;
                changed.push(changed_object("StorageResource", &operation.target_id));
            }
            "config.create" | "config.update" => {
                let config: RenderedServiceConfig = request_value(operation, "resource")?;
                self.capture_registry_resource_previous_state(operation, &config.service_name)?;
                self.store.upsert_rendered_service_config(config.clone())?;
                changed.push(changed_object("RenderedConfig", &config_id(&config)));
            }
            "config.delete" => {
                let service_name =
                    service_name_for_config_target(self.store, &operation.target_id)?;
                self.capture_registry_resource_previous_state(operation, &service_name)?;
                delete_rendered_config(self.store, &service_name, &operation.target_id)?;
                changed.push(changed_object("RenderedConfig", &operation.target_id));
            }
            "topology.apply" => {
                let topology: Topology = request_value(operation, "topology_snapshot")?;
                self.store.put_topology(topology.clone())?;
                changed.push(changed_object("Topology", &topology.root_endpoint));
                for log_view in &topology.log_views {
                    self.store.put_log_view(log_view.clone())?;
                    changed.push(changed_object("LogView", &log_view.source_id));
                }
                for report in &topology.diagnostic_reports {
                    self.store.put_diagnostic_report(report.clone())?;
                    changed.push(changed_object("DiagnosticReport", &report.report_id));
                }
            }
            "diagnostic.create" => {
                let mut report = build_diagnostic_report(
                    self.store,
                    format!("diag-{}", operation.operation_id),
                )?;
                report.operation_id = operation.operation_id.clone();
                self.store.put_diagnostic_report(report.clone())?;
                changed.push(changed_object("DiagnosticReport", &report.report_id));
            }
            "log.query" => {
                let target_operation_id = operation
                    .request
                    .get("operation_id")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or(operation.target_id.as_str());
                let target_operation =
                    self.store
                        .get_operation(target_operation_id)?
                        .ok_or_else(|| {
                            OrchestratorError::Dependency(format!(
                                "operation {target_operation_id} not found"
                            ))
                        })?;
                let target_logs = self.store.list_operation_logs(target_operation_id)?;
                let endpoints = self.store.list_endpoints()?;
                let log_view = operation_log_view_from_target(&target_operation, &endpoints)?;
                self.store.put_log_view(log_view.clone())?;
                self.store.append_operation_log(operation_step_log_record(
                    &operation.operation_id,
                    "log.query",
                    "info",
                    format!(
                        "operation {} logs view opened with {} records",
                        target_operation_id,
                        target_logs.len()
                    ),
                    serde_json::json!({
                        "operation_id": target_operation_id,
                        "log_count": target_logs.len(),
                        "source_id": log_view.source_id,
                    }),
                ))?;
                changed.push(changed_object("LogView", &log_view.source_id));
            }
            "diagnostic.export" => {
                let report_id = operation
                    .request
                    .get("report_id")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or(operation.target_id.as_str());
                let format = operation
                    .request
                    .get("format")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("json");
                let report = match self.store.get_diagnostic_report(report_id)? {
                    Some(report) => report,
                    None => {
                        let mut report = build_diagnostic_report(self.store, report_id)?;
                        report.operation_id = operation.operation_id.clone();
                        self.store.put_diagnostic_report(report.clone())?;
                        report
                    }
                };
                let export = export_diagnostic_report(&report, format)?;
                self.store.append_operation_log(operation_step_log_record(
                    &operation.operation_id,
                    "diagnostic.export",
                    "info",
                    format!(
                        "diagnostic report {} exported as {}",
                        export.report_id, export.format
                    ),
                    serde_json::json!({
                        "report_id": export.report_id,
                        "format": export.format,
                        "content_bytes": export.content.len(),
                    }),
                ))?;
                changed.push(changed_object("DiagnosticReport", report_id));
            }
            _ => {
                return Err(OrchestratorError::Blocked(format!(
                    "action {} has no executor mutation",
                    operation.action
                )));
            }
        }
        Ok(changed)
    }

    fn rollback_operation_mutation(
        &mut self,
        operation: &Operation,
    ) -> Result<Vec<serde_json::Value>> {
        let mut changed = Vec::new();
        match operation.action.as_str() {
            "release.create" | "release.update" => {
                let release = service_release_record_from_operation(operation)?;
                let previous_state = release_record_previous_state_from_operation(operation)?
                    .ok_or_else(|| {
                        OrchestratorError::Dependency(format!(
                            "{} rollback requires previous_state",
                            operation.action
                        ))
                    })?;
                match previous_state.release {
                    Some(previous_release) => {
                        self.store
                            .upsert_service_release(previous_release.clone())?;
                        changed.push(changed_object(
                            "ServiceRelease",
                            &format!(
                                "{}@{}",
                                previous_release.service_name, previous_release.version
                            ),
                        ));
                    }
                    None => {
                        self.store
                            .delete_service_release(&release.service_name, &release.version)?;
                        changed.push(changed_object(
                            "ServiceRelease",
                            &format!("{}@{}", release.service_name, release.version),
                        ));
                    }
                }
            }
            "release.install" => {
                let already_known = operation
                    .request
                    .get("already_known")
                    .and_then(serde_json::Value::as_bool)
                    .unwrap_or(false);
                let previous_state = release_install_previous_state_from_operation(operation)?;
                if let Some(previous_state) = previous_state {
                    let service_name = operation.target_id.as_str();
                    self.clear_release_resource_registries(service_name)?;
                    if let Some(release) = release_manifest_from_operation(operation)? {
                        self.store
                            .delete_service_release(&release.service_name, &release.version)?;
                    }
                    changed.extend(
                        self.restore_release_install_previous_state(service_name, &previous_state)?,
                    );
                    return Ok(changed);
                }
                self.clear_release_resource_registries(&operation.target_id)?;
                if let Some(release) = release_manifest_from_operation(operation)? {
                    self.store
                        .delete_service_release(&release.service_name, &release.version)?;
                }
                if !already_known {
                    self.store.delete_service(&operation.target_id)?;
                    changed.push(changed_object("ServiceRelease", &operation.target_id));
                    changed.push(changed_object("Service", &operation.target_id));
                }
            }
            "release.delete" => {
                let previous_state = release_install_previous_state_from_operation(operation)?
                    .ok_or_else(|| {
                        OrchestratorError::Dependency(
                            "release.delete rollback requires previous_state".to_string(),
                        )
                    })?;
                let service_name = operation
                    .request
                    .get("service_id")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or(operation.target_id.as_str());
                self.clear_release_resource_registries(service_name)?;
                changed.extend(
                    self.restore_release_install_previous_state(service_name, &previous_state)?,
                );
            }
            "endpoint.create" => {
                self.store.delete_endpoint(&operation.target_id)?;
                changed.push(changed_object("Endpoint", &operation.target_id));
            }
            "link.create" => {
                let link = link_from_operation(operation);
                self.store
                    .delete_link(&link.source_endpoint, &link.target_endpoint)?;
                changed.push(changed_object("Link", &link_target_id(&link)));
            }
            "topology.apply" => {
                self.store.delete_topology(&operation.target_id)?;
                changed.push(changed_object("Topology", &operation.target_id));
            }
            "route.create" | "route.update" | "route.delete" | "frontend.create"
            | "frontend.update" | "frontend.delete" | "migration.create" | "migration.update"
            | "migration.delete" | "permission.create" | "permission.update"
            | "permission.delete" | "redis.create" | "redis.update" | "redis.delete"
            | "storage.create" | "storage.update" | "storage.delete" | "config.create"
            | "config.update" | "config.delete" => {
                let (service_name, previous_state) =
                    registry_resource_previous_state_from_operation(operation)?.ok_or_else(
                        || {
                            OrchestratorError::Dependency(format!(
                                "{} rollback requires previous_state",
                                operation.action
                            ))
                        },
                    )?;
                changed.extend(
                    self.restore_registry_resource_previous_state(&service_name, &previous_state)?,
                );
            }
            "service.delete" | "endpoint.delete" | "link.delete" => {
                changed.push(changed_object(&operation.target_type, &operation.target_id));
            }
            _ => {
                changed.push(changed_object(&operation.target_type, &operation.target_id));
            }
        }
        Ok(changed)
    }

    fn capture_release_install_previous_state(
        &mut self,
        operation: &Operation,
        service_name: &str,
    ) -> Result<ReleaseInstallPreviousState> {
        let previous_state = self.release_install_previous_state(service_name)?;
        let mut operation = operation.clone();
        let request = operation.request.as_object_mut().ok_or_else(|| {
            OrchestratorError::Dependency(
                "release.install operation request must be a JSON object".to_string(),
            )
        })?;
        request.insert(
            "previous_state".to_string(),
            serde_json::to_value(&previous_state)?,
        );
        self.store.update_operation(operation)?;
        Ok(previous_state)
    }

    fn release_install_previous_state(
        &mut self,
        service_name: &str,
    ) -> Result<ReleaseInstallPreviousState> {
        Ok(ReleaseInstallPreviousState {
            service: self.store.get_service(service_name)?,
            releases: self
                .store
                .list_service_releases()?
                .into_iter()
                .filter(|release| release.service_name == service_name)
                .collect(),
            routes: self
                .store
                .list_service_routes()?
                .into_iter()
                .filter(|route| route.target_service_name == service_name)
                .collect(),
            migrations: self
                .store
                .list_service_migration_records()?
                .into_iter()
                .filter(|record| record.service_name == service_name)
                .collect(),
            permissions: self
                .store
                .list_service_permission_records()?
                .into_iter()
                .filter(|record| record.service_name == service_name)
                .collect(),
            frontends: self
                .store
                .list_service_frontend_entries()?
                .into_iter()
                .filter(|entry| entry.service_name == service_name)
                .collect(),
            redis: self
                .store
                .list_service_redis_resources()?
                .into_iter()
                .filter(|resource| resource.service_name == service_name)
                .collect(),
            storage: self
                .store
                .list_service_storage_resources()?
                .into_iter()
                .filter(|resource| resource.service_name == service_name)
                .collect(),
            configs: self
                .store
                .list_rendered_service_configs()?
                .into_iter()
                .filter(|config| config.service_name == service_name)
                .collect(),
        })
    }

    fn restore_release_install_previous_state(
        &mut self,
        service_name: &str,
        previous_state: &ReleaseInstallPreviousState,
    ) -> Result<Vec<serde_json::Value>> {
        let mut changed = Vec::new();
        let Some(service) = previous_state.service.as_ref() else {
            self.store.delete_service(service_name)?;
            changed.push(changed_object("ServiceRelease", service_name));
            changed.push(changed_object("Service", service_name));
            return Ok(changed);
        };

        self.store.put_service(service.clone())?;
        changed.push(changed_object("Service", &service.id));
        for release in &previous_state.releases {
            self.store.upsert_service_release(release.clone())?;
            changed.push(changed_object(
                "ServiceRelease",
                &format!("{}@{}", release.service_name, release.version),
            ));
        }
        for route in &previous_state.routes {
            self.store.upsert_service_route(route.clone())?;
            changed.push(changed_object(
                "Route",
                &format!("{} {}", route.method, route.path),
            ));
        }
        for migration in &previous_state.migrations {
            self.store
                .upsert_service_migration_record(migration.clone())?;
            changed.push(changed_object(
                "MigrationRecord",
                &format!("{}@{}", migration.service_name, migration.migration_version),
            ));
        }
        for permission in &previous_state.permissions {
            self.store
                .upsert_service_permission_record(permission.clone())?;
            changed.push(changed_object("Permission", &permission.permission_key));
        }
        for frontend in &previous_state.frontends {
            self.store.upsert_service_frontend_entry(frontend.clone())?;
            changed.push(changed_object(
                "Frontend",
                &format!("{}:{}", frontend.service_name, frontend.route_prefix),
            ));
        }
        for redis in &previous_state.redis {
            self.store.upsert_service_redis_resource(redis.clone())?;
            changed.push(changed_object(
                "RedisResource",
                &format!("{}:{}", redis.service_name, redis.name),
            ));
        }
        for storage in &previous_state.storage {
            self.store
                .upsert_service_storage_resource(storage.clone())?;
            changed.push(changed_object(
                "StorageResource",
                &format!("{}:{}", storage.bucket, storage.object_type),
            ));
        }
        for config in &previous_state.configs {
            self.store.upsert_rendered_service_config(config.clone())?;
            changed.push(changed_object(
                "RenderedConfig",
                &format!("{}@{}", config.service_name, config.version),
            ));
        }
        Ok(changed)
    }

    fn capture_release_record_previous_state(
        &mut self,
        operation: &Operation,
        service_name: &str,
        version: &str,
    ) -> Result<ReleaseRecordPreviousState> {
        let previous_state = ReleaseRecordPreviousState {
            release: self.store.get_service_release(service_name, version)?,
        };
        let mut operation = operation.clone();
        let request = operation.request.as_object_mut().ok_or_else(|| {
            OrchestratorError::Dependency(format!(
                "{} operation request must be a JSON object",
                operation.action
            ))
        })?;
        request.insert(
            "previous_state".to_string(),
            serde_json::to_value(&previous_state)?,
        );
        self.store.update_operation(operation)?;
        Ok(previous_state)
    }

    fn capture_registry_resource_previous_state(
        &mut self,
        operation: &Operation,
        service_name: &str,
    ) -> Result<RegistryResourcePreviousState> {
        let previous_state = self.registry_resource_previous_state(service_name)?;
        let mut operation = operation.clone();
        let request = operation.request.as_object_mut().ok_or_else(|| {
            OrchestratorError::Dependency(format!(
                "{} operation request must be a JSON object",
                operation.action
            ))
        })?;
        request.insert("previous_service_name".to_string(), service_name.into());
        request.insert(
            "previous_state".to_string(),
            serde_json::to_value(&previous_state)?,
        );
        self.store.update_operation(operation)?;
        Ok(previous_state)
    }

    fn registry_resource_previous_state(
        &mut self,
        service_name: &str,
    ) -> Result<RegistryResourcePreviousState> {
        Ok(RegistryResourcePreviousState {
            routes: self
                .store
                .list_service_routes()?
                .into_iter()
                .filter(|route| route.target_service_name == service_name)
                .collect(),
            migrations: self
                .store
                .list_service_migration_records()?
                .into_iter()
                .filter(|record| record.service_name == service_name)
                .collect(),
            permissions: self
                .store
                .list_service_permission_records()?
                .into_iter()
                .filter(|record| record.service_name == service_name)
                .collect(),
            frontends: self
                .store
                .list_service_frontend_entries()?
                .into_iter()
                .filter(|entry| entry.service_name == service_name)
                .collect(),
            redis: self
                .store
                .list_service_redis_resources()?
                .into_iter()
                .filter(|resource| resource.service_name == service_name)
                .collect(),
            storage: self
                .store
                .list_service_storage_resources()?
                .into_iter()
                .filter(|resource| resource.service_name == service_name)
                .collect(),
            configs: self
                .store
                .list_rendered_service_configs()?
                .into_iter()
                .filter(|config| config.service_name == service_name)
                .collect(),
        })
    }

    fn restore_registry_resource_previous_state(
        &mut self,
        service_name: &str,
        previous_state: &RegistryResourcePreviousState,
    ) -> Result<Vec<serde_json::Value>> {
        self.clear_release_resource_registries(service_name)?;
        let mut changed = Vec::new();
        for route in &previous_state.routes {
            self.store.upsert_service_route(route.clone())?;
            changed.push(changed_object("Route", &route_id(route)));
        }
        for migration in &previous_state.migrations {
            self.store
                .upsert_service_migration_record(migration.clone())?;
            changed.push(changed_object("MigrationRecord", &migration_id(migration)));
        }
        for permission in &previous_state.permissions {
            self.store
                .upsert_service_permission_record(permission.clone())?;
            changed.push(changed_object("Permission", &permission.permission_key));
        }
        for frontend in &previous_state.frontends {
            self.store.upsert_service_frontend_entry(frontend.clone())?;
            changed.push(changed_object("Frontend", &frontend_id(frontend)));
        }
        for redis in &previous_state.redis {
            self.store.upsert_service_redis_resource(redis.clone())?;
            changed.push(changed_object("RedisResource", &redis_id(redis)));
        }
        for storage in &previous_state.storage {
            self.store
                .upsert_service_storage_resource(storage.clone())?;
            changed.push(changed_object("StorageResource", &storage_id(storage)));
        }
        for config in &previous_state.configs {
            self.store.upsert_rendered_service_config(config.clone())?;
            changed.push(changed_object("RenderedConfig", &config_id(config)));
        }
        changed.push(changed_object("ReleaseRegistry", service_name));
        Ok(changed)
    }

    fn resolve_release_rollback_target(&mut self, operation: &Operation) -> Result<String> {
        if let Some(target_operation_id) = operation
            .request
            .get("target_operation_id")
            .and_then(serde_json::Value::as_str)
            .filter(|value| !value.trim().is_empty())
        {
            let target = self
                .store
                .get_operation(target_operation_id)?
                .ok_or_else(|| {
                    OrchestratorError::Dependency(format!(
                        "release install operation {target_operation_id} not found"
                    ))
                })?;
            ensure_release_install_operation_matches_request(&target, operation)?;
            return Ok(target_operation_id.to_string());
        }

        let service_name = operation
            .request
            .get("service_id")
            .and_then(serde_json::Value::as_str)
            .unwrap_or(operation.target_id.as_str());
        let requested_version = operation
            .request
            .get("version")
            .and_then(serde_json::Value::as_str)
            .filter(|value| !value.trim().is_empty());
        self.store
            .list_operations()?
            .into_iter()
            .rev()
            .find(|candidate| {
                candidate.action == "release.install"
                    && matches!(candidate.status, OperationStatus::Succeeded)
                    && candidate
                        .request
                        .get("service_id")
                        .and_then(serde_json::Value::as_str)
                        == Some(service_name)
                    && requested_version.is_none_or(|version| {
                        candidate
                            .request
                            .get("version")
                            .and_then(serde_json::Value::as_str)
                            == Some(version)
                    })
            })
            .map(|operation| operation.operation_id)
            .ok_or_else(|| {
                OrchestratorError::Dependency(format!(
                    "no successful release.install operation found for {service_name}"
                ))
            })
    }

    fn probe_endpoint_and_persist(
        &mut self,
        operation_id: &str,
        endpoint: &Endpoint,
    ) -> Result<EndpointHealthResult> {
        let health = check_endpoint_health_with_probe(endpoint, &self.endpoint_probe)?;
        self.store.update_endpoint_health(
            &health.endpoint,
            health.health.clone(),
            health.reachable,
        )?;
        self.store
            .append_operation_log(endpoint_health_log_record(operation_id, &health))?;
        Ok(health)
    }

    fn clear_release_resource_registries(&mut self, service_name: &str) -> Result<()> {
        self.store.delete_service_routes_for_service(service_name)?;
        self.store
            .delete_service_migration_records_for_service(service_name)?;
        self.store
            .delete_service_permission_records_for_service(service_name)?;
        self.store.delete_service_frontend_entry(service_name)?;
        self.store
            .delete_service_redis_resources_for_service(service_name)?;
        self.store
            .delete_service_storage_resources_for_service(service_name)?;
        self.store
            .delete_rendered_service_configs_for_service(service_name)?;
        Ok(())
    }
}

fn request_value<T: DeserializeOwned>(operation: &Operation, field: &str) -> Result<T> {
    let value = operation.request.get(field).cloned().ok_or_else(|| {
        OrchestratorError::Dependency(format!(
            "operation {} request missing {field}",
            operation.operation_id
        ))
    })?;
    serde_json::from_value(value).map_err(OrchestratorError::Json)
}

fn release_manifest_from_operation(
    operation: &Operation,
) -> Result<Option<ServiceReleaseManifest>> {
    match operation.request.get("release_manifest") {
        Some(value) if value.is_null() => Ok(None),
        Some(value) => serde_json::from_value(value.clone())
            .map(Some)
            .map_err(OrchestratorError::Json),
        None => Ok(None),
    }
}

fn release_install_previous_state_from_operation(
    operation: &Operation,
) -> Result<Option<ReleaseInstallPreviousState>> {
    operation
        .request
        .get("previous_state")
        .cloned()
        .map(serde_json::from_value)
        .transpose()
        .map_err(OrchestratorError::Json)
}

fn release_record_previous_state_from_operation(
    operation: &Operation,
) -> Result<Option<ReleaseRecordPreviousState>> {
    operation
        .request
        .get("previous_state")
        .cloned()
        .map(serde_json::from_value)
        .transpose()
        .map_err(OrchestratorError::Json)
}

fn registry_resource_previous_state_from_operation(
    operation: &Operation,
) -> Result<Option<(String, RegistryResourcePreviousState)>> {
    let Some(value) = operation.request.get("previous_state").cloned() else {
        return Ok(None);
    };
    let service_name = operation
        .request
        .get("previous_service_name")
        .and_then(serde_json::Value::as_str)
        .unwrap_or(operation.target_id.as_str())
        .to_string();
    let previous_state = serde_json::from_value(value).map_err(OrchestratorError::Json)?;
    Ok(Some((service_name, previous_state)))
}

fn ensure_release_install_operation_matches_request(
    target: &Operation,
    request: &Operation,
) -> Result<()> {
    if target.action != "release.install" {
        return Err(OrchestratorError::Dependency(format!(
            "target operation {} is not release.install",
            target.operation_id
        )));
    }
    if !matches!(
        target.status,
        OperationStatus::Succeeded | OperationStatus::Failed
    ) {
        return Err(OrchestratorError::Blocked(format!(
            "target release install operation {} status {:?} cannot rollback",
            target.operation_id, target.status
        )));
    }
    let request_service = request
        .request
        .get("service_id")
        .and_then(serde_json::Value::as_str);
    let target_service = target
        .request
        .get("service_id")
        .and_then(serde_json::Value::as_str);
    if request_service != target_service {
        return Err(OrchestratorError::Dependency(format!(
            "release rollback target {} does not match requested service",
            target.operation_id
        )));
    }
    let request_version = request
        .request
        .get("version")
        .and_then(serde_json::Value::as_str)
        .filter(|value| !value.trim().is_empty());
    let target_version = target
        .request
        .get("version")
        .and_then(serde_json::Value::as_str);
    if request_version.is_some_and(|version| target_version != Some(version)) {
        return Err(OrchestratorError::Dependency(format!(
            "release rollback target {} does not match requested version",
            target.operation_id
        )));
    }
    Ok(())
}

fn release_changed_objects(
    service: &ServiceManifest,
    release: Option<&ServiceReleaseManifest>,
) -> Vec<serde_json::Value> {
    let mut changed = vec![changed_object(
        "ServiceRelease",
        &format!("{}@{}", service.id, service.version),
    )];
    if let Some(release) = release {
        changed.extend(
            release
                .permissions
                .iter()
                .map(|permission| changed_object("Permission", permission)),
        );
        changed.extend(release.routes.iter().map(|route| {
            let method = if route.method.trim().is_empty() {
                "ANY"
            } else {
                route.method.trim()
            };
            changed_object(
                "Route",
                &format!("{} {}", method.to_ascii_uppercase(), route.path),
            )
        }));
        if release.frontend.enabled {
            changed.push(changed_object(
                "Frontend",
                &format!("{}:{}", release.service_name, release.frontend.route_prefix),
            ));
        }
        changed.extend(release.migrations.iter().map(|migration| {
            changed_object(
                "MigrationRecord",
                &format!("{}@{}", release.service_name, migration.version),
            )
        }));
        changed.extend(release.redis.iter().map(|redis| {
            changed_object(
                "RedisResource",
                &format!("{}:{}", release.service_name, redis.name),
            )
        }));
        changed.extend(release.storage.iter().map(|storage| {
            changed_object(
                "StorageResource",
                &format!("{}:{}", storage.bucket, storage.object_type),
            )
        }));
        changed.push(changed_object(
            "RenderedConfig",
            &format!("{}@{}", release.service_name, release.version),
        ));
    }
    changed
}

fn delete_service_route<S: OrchestratorStore>(
    store: &mut S,
    service_name: &str,
    target_id: &str,
) -> Result<()> {
    let routes = store
        .list_service_routes()?
        .into_iter()
        .filter(|route| route.target_service_name == service_name)
        .filter(|route| !route_matches_target(route, target_id))
        .collect::<Vec<_>>();
    store.delete_service_routes_for_service(service_name)?;
    for route in routes {
        store.upsert_service_route(route)?;
    }
    Ok(())
}

fn delete_service_migration<S: OrchestratorStore>(
    store: &mut S,
    service_name: &str,
    target_id: &str,
) -> Result<()> {
    let migrations = store
        .list_service_migration_records()?
        .into_iter()
        .filter(|record| record.service_name == service_name)
        .filter(|record| !migration_matches_target(record, target_id))
        .collect::<Vec<_>>();
    store.delete_service_migration_records_for_service(service_name)?;
    for migration in migrations {
        store.upsert_service_migration_record(migration)?;
    }
    Ok(())
}

fn delete_service_permission<S: OrchestratorStore>(
    store: &mut S,
    service_name: &str,
    target_id: &str,
) -> Result<()> {
    let permissions = store
        .list_service_permission_records()?
        .into_iter()
        .filter(|record| record.service_name == service_name)
        .filter(|record| record.permission_key != target_id)
        .collect::<Vec<_>>();
    store.delete_service_permission_records_for_service(service_name)?;
    for permission in permissions {
        store.upsert_service_permission_record(permission)?;
    }
    Ok(())
}

fn delete_service_redis<S: OrchestratorStore>(
    store: &mut S,
    service_name: &str,
    target_id: &str,
) -> Result<()> {
    let resources = store
        .list_service_redis_resources()?
        .into_iter()
        .filter(|resource| resource.service_name == service_name)
        .filter(|resource| !redis_matches_target(resource, target_id))
        .collect::<Vec<_>>();
    store.delete_service_redis_resources_for_service(service_name)?;
    for resource in resources {
        store.upsert_service_redis_resource(resource)?;
    }
    Ok(())
}

fn delete_service_storage<S: OrchestratorStore>(
    store: &mut S,
    service_name: &str,
    target_id: &str,
) -> Result<()> {
    let resources = store
        .list_service_storage_resources()?
        .into_iter()
        .filter(|resource| resource.service_name == service_name)
        .filter(|resource| !storage_matches_target(resource, target_id))
        .collect::<Vec<_>>();
    store.delete_service_storage_resources_for_service(service_name)?;
    for resource in resources {
        store.upsert_service_storage_resource(resource)?;
    }
    Ok(())
}

fn delete_rendered_config<S: OrchestratorStore>(
    store: &mut S,
    service_name: &str,
    target_id: &str,
) -> Result<()> {
    let configs = store
        .list_rendered_service_configs()?
        .into_iter()
        .filter(|config| config.service_name == service_name)
        .filter(|config| !config_matches_target(config, target_id))
        .collect::<Vec<_>>();
    store.delete_rendered_service_configs_for_service(service_name)?;
    for config in configs {
        store.upsert_rendered_service_config(config)?;
    }
    Ok(())
}

fn service_name_for_route_target<S: OrchestratorStore>(
    store: &S,
    target_id: &str,
) -> Result<String> {
    store
        .list_service_routes()?
        .into_iter()
        .find(|route| route_matches_target(route, target_id))
        .map(|route| route.target_service_name)
        .ok_or_else(|| OrchestratorError::Dependency(format!("route {target_id} not found")))
}

fn service_name_for_frontend_target<S: OrchestratorStore>(
    store: &S,
    target_id: &str,
) -> Result<String> {
    store
        .list_service_frontend_entries()?
        .into_iter()
        .find(|frontend| {
            frontend.service_name == target_id
                || frontend_id(frontend) == target_id
                || frontend.route_prefix == target_id
        })
        .map(|frontend| frontend.service_name)
        .ok_or_else(|| OrchestratorError::Dependency(format!("frontend {target_id} not found")))
}

fn service_name_for_migration_target<S: OrchestratorStore>(
    store: &S,
    target_id: &str,
) -> Result<String> {
    store
        .list_service_migration_records()?
        .into_iter()
        .find(|migration| migration_matches_target(migration, target_id))
        .map(|migration| migration.service_name)
        .ok_or_else(|| OrchestratorError::Dependency(format!("migration {target_id} not found")))
}

fn service_name_for_permission_target<S: OrchestratorStore>(
    store: &S,
    target_id: &str,
) -> Result<String> {
    store
        .list_service_permission_records()?
        .into_iter()
        .find(|permission| permission.permission_key == target_id)
        .map(|permission| permission.service_name)
        .ok_or_else(|| OrchestratorError::Dependency(format!("permission {target_id} not found")))
}

fn service_name_for_redis_target<S: OrchestratorStore>(
    store: &S,
    target_id: &str,
) -> Result<String> {
    store
        .list_service_redis_resources()?
        .into_iter()
        .find(|redis| redis_matches_target(redis, target_id))
        .map(|redis| redis.service_name)
        .ok_or_else(|| {
            OrchestratorError::Dependency(format!("redis resource {target_id} not found"))
        })
}

fn service_name_for_storage_target<S: OrchestratorStore>(
    store: &S,
    target_id: &str,
) -> Result<String> {
    store
        .list_service_storage_resources()?
        .into_iter()
        .find(|storage| storage_matches_target(storage, target_id))
        .map(|storage| storage.service_name)
        .ok_or_else(|| {
            OrchestratorError::Dependency(format!("storage resource {target_id} not found"))
        })
}

fn service_name_for_config_target<S: OrchestratorStore>(
    store: &S,
    target_id: &str,
) -> Result<String> {
    store
        .list_rendered_service_configs()?
        .into_iter()
        .find(|config| config_matches_target(config, target_id))
        .map(|config| config.service_name)
        .ok_or_else(|| {
            OrchestratorError::Dependency(format!("rendered config {target_id} not found"))
        })
}

fn route_matches_target(route: &ServiceRoute, target_id: &str) -> bool {
    route.path == target_id || route_id(route) == target_id
}

fn migration_matches_target(migration: &ServiceMigrationRecord, target_id: &str) -> bool {
    migration.migration_version == target_id || migration_id(migration) == target_id
}

fn redis_matches_target(redis: &ServiceRedisResource, target_id: &str) -> bool {
    redis.name == target_id || redis_id(redis) == target_id
}

fn storage_matches_target(storage: &ServiceStorageResource, target_id: &str) -> bool {
    storage.object_type == target_id || storage_id(storage) == target_id
}

fn config_matches_target(config: &RenderedServiceConfig, target_id: &str) -> bool {
    config.service_name == target_id
        || config.version == target_id
        || config_id(config) == target_id
}

fn route_id(route: &ServiceRoute) -> String {
    format!("{} {}", route.method, route.path)
}

fn frontend_id(frontend: &ServiceFrontendEntry) -> String {
    format!("{}:{}", frontend.service_name, frontend.route_prefix)
}

fn migration_id(migration: &ServiceMigrationRecord) -> String {
    format!("{}@{}", migration.service_name, migration.migration_version)
}

fn redis_id(redis: &ServiceRedisResource) -> String {
    format!("{}:{}", redis.service_name, redis.name)
}

fn storage_id(storage: &ServiceStorageResource) -> String {
    format!(
        "{}:{}:{}",
        storage.service_name, storage.object_type, storage.bucket
    )
}

fn config_id(config: &RenderedServiceConfig) -> String {
    format!("{}@{}", config.service_name, config.version)
}

fn service_release_record(release: &ServiceReleaseManifest) -> Result<ServiceRelease> {
    Ok(ServiceRelease {
        service_name: release.service_name.clone(),
        version: release.version.clone(),
        release_url: release.source.url.clone(),
        manifest: serde_json::to_value(release)?,
        checksum: release.source.checksum.clone(),
        created_at: String::new(),
    })
}

fn service_release_record_from_operation(operation: &Operation) -> Result<ServiceRelease> {
    let release = release_manifest_from_operation(operation)?.ok_or_else(|| {
        OrchestratorError::Dependency(format!(
            "operation {} request missing release_manifest",
            operation.operation_id
        ))
    })?;
    let mut record = service_release_record(&release)?;
    if let Some(release_url) = operation
        .request
        .get("release_url")
        .and_then(serde_json::Value::as_str)
        .filter(|value| !value.trim().is_empty())
    {
        record.release_url = release_url.to_string();
    }
    Ok(record)
}

fn service_routes_from_release(release: &ServiceReleaseManifest) -> Result<Vec<ServiceRoute>> {
    release
        .routes
        .iter()
        .map(|route| {
            let method = if route.method.trim().is_empty() {
                "ANY"
            } else {
                route.method.trim()
            };
            Ok(ServiceRoute {
                path: route.path.clone(),
                method: method.to_ascii_uppercase(),
                target_type: route.target_type.clone(),
                target_service_name: route_target_service_name(release, route)?,
                target_selector: route_target_selector(route),
                permission: route.permission.clone(),
                enabled: true,
                created_at: String::new(),
                updated_at: String::new(),
            })
        })
        .collect()
}

fn route_target_service_name(
    release: &ServiceReleaseManifest,
    route: &crate::ReleaseRouteDecl,
) -> Result<String> {
    match route.target_type.as_str() {
        "endpoint" => Ok(parse_endpoint_id(&route.target)?.service_name.to_string()),
        "endpoint-group" => Ok(route.target.trim_end_matches("[*]").to_string()),
        "frontend" => Ok(release.service_name.clone()),
        _ => Err(OrchestratorError::InvalidManifest(
            "route target_type is invalid".to_string(),
        )),
    }
}

fn route_target_selector(route: &crate::ReleaseRouteDecl) -> serde_json::Value {
    if route.target_type == "endpoint" {
        serde_json::json!({
            "endpoint": route.target
        })
    } else if route.target_type == "endpoint-group" {
        serde_json::json!({
            "group": route.target
        })
    } else {
        serde_json::json!({
            "frontend": route.target
        })
    }
}

fn service_migration_records_from_release(
    release: &ServiceReleaseManifest,
) -> Vec<ServiceMigrationRecord> {
    release
        .migrations
        .iter()
        .map(|migration| ServiceMigrationRecord {
            service_name: release.service_name.clone(),
            migration_version: migration.version.clone(),
            checksum: migration.checksum.clone(),
            status: "registered".to_string(),
            applied_at: String::new(),
            created_at: String::new(),
            updated_at: String::new(),
        })
        .collect()
}

fn service_permission_records_from_release(
    release: &ServiceReleaseManifest,
) -> Vec<ServicePermissionRecord> {
    release
        .permissions
        .iter()
        .map(|permission| ServicePermissionRecord {
            service_name: release.service_name.clone(),
            permission_key: permission.clone(),
            source: "release".to_string(),
            created_at: String::new(),
            updated_at: String::new(),
        })
        .collect()
}

fn service_frontend_entry_from_release(
    release: &ServiceReleaseManifest,
) -> Result<ServiceFrontendEntry> {
    Ok(ServiceFrontendEntry {
        service_name: release.service_name.clone(),
        enabled: release.frontend.enabled,
        route_prefix: release.frontend.route_prefix.clone(),
        remote_entry: release.frontend.remote_entry.clone(),
        menu_items: release.frontend.menu_items.clone(),
        created_at: String::new(),
        updated_at: String::new(),
    })
}

fn service_redis_resources_from_release(
    release: &ServiceReleaseManifest,
) -> Vec<ServiceRedisResource> {
    release
        .redis
        .iter()
        .map(|redis| ServiceRedisResource {
            service_name: release.service_name.clone(),
            name: redis.name.clone(),
            kind: redis.kind.clone(),
            usage: redis.usage.clone(),
            created_at: String::new(),
            updated_at: String::new(),
        })
        .collect()
}

fn service_storage_resources_from_release(
    release: &ServiceReleaseManifest,
) -> Vec<ServiceStorageResource> {
    release
        .storage
        .iter()
        .map(|storage| ServiceStorageResource {
            service_name: release.service_name.clone(),
            object_type: storage.object_type.clone(),
            bucket: storage.bucket.clone(),
            path_prefix: storage.path_prefix.clone(),
            created_at: String::new(),
            updated_at: String::new(),
        })
        .collect()
}

fn rendered_config_from_release(release: &ServiceReleaseManifest) -> Result<RenderedServiceConfig> {
    Ok(RenderedServiceConfig {
        service_name: release.service_name.clone(),
        version: release.version.clone(),
        config: serde_json::json!({
            "service_name": release.service_name,
            "version": release.version,
            "backend": release.backend,
            "dependencies": release.dependencies,
            "config_schema": release.config_schema,
            "secrets": release.secrets,
            "observability": release.observability,
        }),
        created_at: String::new(),
        updated_at: String::new(),
    })
}

fn endpoint_from_operation<S: OrchestratorStore>(
    operation: &Operation,
    store: &S,
) -> Result<Endpoint> {
    let endpoint_id = operation
        .request
        .get("endpoint")
        .and_then(serde_json::Value::as_str)
        .unwrap_or(operation.target_id.as_str());
    let endpoint_identity = parse_endpoint_id(endpoint_id)?;
    let current = store.get_endpoint(endpoint_id)?;
    Ok(Endpoint {
        endpoint: endpoint_id.to_string(),
        service_id: operation
            .request
            .get("service_id")
            .and_then(serde_json::Value::as_str)
            .or_else(|| {
                current
                    .as_ref()
                    .map(|endpoint| endpoint.service_id.as_str())
            })
            .unwrap_or(endpoint_identity.service_name)
            .to_string(),
        protocol: operation
            .request
            .get("protocol")
            .and_then(serde_json::Value::as_str)
            .or_else(|| current.as_ref().map(|endpoint| endpoint.protocol.as_str()))
            .unwrap_or("http")
            .to_string(),
        health_path: operation
            .request
            .get("health_path")
            .and_then(serde_json::Value::as_str)
            .or_else(|| {
                current
                    .as_ref()
                    .map(|endpoint| endpoint.health_path.as_str())
            })
            .unwrap_or("")
            .to_string(),
        health: current
            .as_ref()
            .map(|endpoint| endpoint.health.clone())
            .unwrap_or_else(|| "unknown".to_string()),
        reachable: current.as_ref().is_some_and(|endpoint| endpoint.reachable),
        display_name: operation
            .request
            .get("display_name")
            .and_then(serde_json::Value::as_str)
            .or_else(|| {
                current
                    .as_ref()
                    .map(|endpoint| endpoint.display_name.as_str())
            })
            .unwrap_or("")
            .to_string(),
        note: operation
            .request
            .get("note")
            .and_then(serde_json::Value::as_str)
            .or_else(|| current.as_ref().map(|endpoint| endpoint.note.as_str()))
            .unwrap_or("")
            .to_string(),
        config: operation
            .request
            .get("config")
            .cloned()
            .or_else(|| current.as_ref().map(|endpoint| endpoint.config.clone()))
            .unwrap_or_else(|| serde_json::json!({})),
        created_at: current
            .as_ref()
            .map(|endpoint| endpoint.created_at.clone())
            .unwrap_or_default(),
        updated_at: String::new(),
    })
}

fn link_from_operation(operation: &Operation) -> Link {
    Link {
        source_endpoint: operation
            .request
            .get("source_endpoint")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("")
            .to_string(),
        target_endpoint: operation
            .request
            .get("target_endpoint")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("")
            .to_string(),
        protocol: operation
            .request
            .get("protocol")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("http")
            .to_string(),
        auth_mode: operation
            .request
            .get("auth_mode")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("internal")
            .to_string(),
        scope: operation
            .request
            .get("scope")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("")
            .to_string(),
        health: "unknown".to_string(),
        latency_ms: None,
        config_ref: operation
            .request
            .get("config_ref")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("")
            .to_string(),
        secret_ref: operation
            .request
            .get("secret_ref")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("")
            .to_string(),
        policy: operation
            .request
            .get("policy")
            .cloned()
            .unwrap_or_else(|| serde_json::json!({})),
        created_at: String::new(),
        updated_at: String::new(),
    }
}

fn log_view_from_operation(operation: &Operation) -> Option<LogView> {
    let endpoint = operation
        .request
        .get("endpoint")
        .and_then(serde_json::Value::as_str)
        .filter(|value| !value.is_empty())?;
    let service_id = operation
        .request
        .get("service_id")
        .and_then(serde_json::Value::as_str)
        .unwrap_or(operation.target_id.as_str());
    Some(LogView {
        source_id: format!("{service_id}:{endpoint}"),
        service_id: service_id.to_string(),
        endpoint: endpoint.to_string(),
        operation_id: operation.operation_id.clone(),
        path: "/logs".to_string(),
        driver: "external-endpoint".to_string(),
        read_policy: "service-scoped".to_string(),
        display_name: format!("{service_id} logs"),
    })
}

fn operation_log_view_from_target(
    operation: &Operation,
    endpoints: &[Endpoint],
) -> Result<LogView> {
    let requested_endpoint = operation
        .request
        .get("endpoint")
        .and_then(serde_json::Value::as_str)
        .filter(|value| !value.is_empty());
    let endpoint = requested_endpoint
        .and_then(|value| {
            endpoints
                .iter()
                .find(|endpoint| endpoint.endpoint == value)
                .map(|endpoint| endpoint.endpoint.clone())
        })
        .or_else(|| {
            endpoints
                .iter()
                .find(|endpoint| endpoint.service_id == operation.target_id)
                .map(|endpoint| endpoint.endpoint.clone())
        })
        .or_else(|| endpoints.first().map(|endpoint| endpoint.endpoint.clone()))
        .ok_or_else(|| {
            OrchestratorError::Dependency(
                "operation log view requires at least one registered endpoint".to_string(),
            )
        })?;
    Ok(LogView {
        source_id: format!("operation:{}", operation.operation_id),
        service_id: operation.target_id.clone(),
        endpoint,
        operation_id: operation.operation_id.clone(),
        path: "/operations/logs".to_string(),
        driver: "external-endpoint".to_string(),
        read_policy: "operation-scoped".to_string(),
        display_name: format!("{} logs", operation.operation_id),
    })
}

fn ensure_service_exists<S: OrchestratorStore>(
    store: &S,
    service_id: &str,
) -> Result<ServiceManifest> {
    store
        .get_service(service_id)?
        .ok_or_else(|| OrchestratorError::Dependency(format!("service {service_id} not found")))
}

fn execute_service_driver_action(
    service: &ServiceManifest,
    operation: &Operation,
    execute_fixed_commands: bool,
) -> Result<DriverResult> {
    let request = DriverRequest {
        action: operation.action.clone(),
        service_id: service.id.clone(),
        endpoint: operation
            .request
            .get("endpoint")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("")
            .to_string(),
        link: None,
        log_source: None,
    };
    match service.runtime.mode {
        RuntimeMode::Container => {
            let driver = DockerComposeDriver::new(".", "deploy/compose/docker-compose.yml");
            if execute_fixed_commands {
                driver.with_execution_enabled().execute(&request)
            } else {
                driver.execute(&request)
            }
        }
        RuntimeMode::LocalProcess => LocalProcessDriver::new().execute(&request),
        RuntimeMode::External => ExternalEndpointDriver.execute(&request),
    }
}

fn ensure_driver_result_succeeded(result: &DriverResult) -> Result<()> {
    match result.status.as_str() {
        "SUCCEEDED" => Ok(()),
        "PLANNED" => Err(OrchestratorError::Blocked(format!(
            "driver action {} built a fixed command but execution is not enabled",
            result.action
        ))),
        "SUPPORTED" => Err(OrchestratorError::Blocked(format!(
            "driver action {} is metadata-only and has no lifecycle effect",
            result.action
        ))),
        "FAILED" => Err(OrchestratorError::Dependency(format!(
            "driver action {} failed: {}",
            result.action, result.message
        ))),
        other => Err(OrchestratorError::Dependency(format!(
            "driver action {} returned unsupported status {other}",
            result.action
        ))),
    }
}

fn driver_result_log_record(operation_id: &str, result: &DriverResult) -> OperationLogRecord {
    operation_step_log_record(
        operation_id,
        format!("driver:{}", result.action),
        "info",
        format!(
            "driver action {} returned {}: {}",
            result.action, result.status, result.message
        ),
        serde_json::json!({
            "action": result.action,
            "status": result.status,
            "message": result.message,
            "command": result.command,
        }),
    )
}

fn log_view_log_record(operation_id: &str, log_view: &LogView) -> OperationLogRecord {
    operation_step_log_record(
        operation_id,
        format!("log-view:{}", log_view.source_id),
        "info",
        format!("log view {} opened", log_view.source_id),
        serde_json::json!({
            "source_id": log_view.source_id,
            "service_id": log_view.service_id,
            "endpoint": log_view.endpoint,
            "operation_id": log_view.operation_id,
            "read_policy": log_view.read_policy,
        }),
    )
}

fn release_install_log_record(
    operation_id: &str,
    release: &ServiceReleaseManifest,
) -> OperationLogRecord {
    operation_step_log_record(
        operation_id,
        format!("release:{}", release.service_name),
        "info",
        format!(
            "release {}@{} registered routes={}, permissions={}, migrations={}, redis={}, storage={}",
            release.service_name,
            release.version,
            release.routes.len(),
            release.permissions.len(),
            release.migrations.len(),
            release.redis.len(),
            release.storage.len()
        ),
        serde_json::json!({
            "service_name": release.service_name,
            "version": release.version,
            "routes": release.routes.len(),
            "permissions": release.permissions.len(),
            "frontend_enabled": release.frontend.enabled,
            "migrations": release.migrations.len(),
            "redis": release.redis.len(),
            "storage": release.storage.len(),
            "dependencies": release.dependencies,
        }),
    )
}

fn endpoint_health_log_record(
    operation_id: &str,
    result: &EndpointHealthResult,
) -> OperationLogRecord {
    operation_step_log_record(
        operation_id,
        format!("health:endpoint:{}", result.endpoint),
        if result.reachable { "info" } else { "warn" },
        format!(
            "endpoint {} health {}: {}",
            result.endpoint, result.health, result.message
        ),
        serde_json::json!({
            "endpoint": result.endpoint,
            "health": result.health,
            "reachable": result.reachable,
            "latency_ms": result.latency_ms,
            "message": result.message,
        }),
    )
}

fn link_health_log_record(operation_id: &str, result: &LinkHealthResult) -> OperationLogRecord {
    operation_step_log_record(
        operation_id,
        format!(
            "health:link:{}>{}",
            result.source_endpoint, result.target_endpoint
        ),
        if result.health == "healthy" {
            "info"
        } else {
            "warn"
        },
        format!(
            "link {} -> {} health {}: {}",
            result.source_endpoint, result.target_endpoint, result.health, result.message
        ),
        serde_json::json!({
            "source_endpoint": result.source_endpoint,
            "target_endpoint": result.target_endpoint,
            "health": result.health,
            "latency_ms": result.latency_ms,
            "message": result.message,
        }),
    )
}

fn missing_target_health(link: &Link) -> EndpointHealthResult {
    EndpointHealthResult {
        endpoint: link.target_endpoint.clone(),
        health: "blocked".to_string(),
        reachable: false,
        latency_ms: None,
        message: "target endpoint is missing".to_string(),
    }
}

fn operation_steps(operation: &Operation) -> Vec<serde_json::Value> {
    value_steps(&operation.plan)
}

fn rollback_steps(operation: &Operation) -> Vec<serde_json::Value> {
    value_steps(&operation.rollback_plan)
}

fn value_steps(value: &serde_json::Value) -> Vec<serde_json::Value> {
    value
        .get("steps")
        .and_then(serde_json::Value::as_array)
        .cloned()
        .unwrap_or_default()
}

fn step_id(step: &serde_json::Value, index: usize) -> String {
    step.get("id")
        .and_then(serde_json::Value::as_str)
        .or_else(|| step.get("action").and_then(serde_json::Value::as_str))
        .map(str::to_string)
        .unwrap_or_else(|| format!("step-{}", index + 1))
}

fn step_label(step: &serde_json::Value) -> String {
    step.get("action")
        .and_then(serde_json::Value::as_str)
        .or_else(|| step.as_str())
        .unwrap_or("operation-step")
        .to_string()
}

fn changed_object(object_type: &str, id: &str) -> serde_json::Value {
    serde_json::json!({
        "type": object_type,
        "id": id
    })
}

fn link_target_id(link: &Link) -> String {
    format!("{} -> {}", link.source_endpoint, link.target_endpoint)
}
