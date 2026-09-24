//! Explicit ephemeral repository; never a mirror of durable state.
use super::OrchestratorStore;
use orchestrator_core::{
    DeployedServiceApi, DiagnosticReport, Endpoint, HostService, Link, LogView, NodeRecord,
    Operation, OperationLock, OperationLogRecord, OperationStatus, OrchestratorError,
    RenderedServiceConfig, Result, ServiceApiSurface, ServiceFrontendEntry, ServiceManifest,
    ServiceMigrationRecord, ServicePermissionRecord, ServiceRedisResource, ServiceRelease,
    ServiceRoute, ServiceStorageResource, Topology, TopologySnapshot, build_topology,
    validate_deployed_service_api, validate_endpoint, validate_endpoint_id, validate_host_service,
    validate_link, validate_log_view, validate_node_record, validate_node_tree_upsert,
    validate_rendered_service_config, validate_service_api_surface,
    validate_service_frontend_entry, validate_service_manifest, validate_service_migration_record,
    validate_service_permission_record, validate_service_redis_resource,
    validate_service_release_record, validate_service_route, validate_service_storage_resource,
    validate_topology,
};
use std::collections::BTreeMap;

#[derive(Debug, Default, Clone)]
pub struct MemoryOrchestratorStore {
    services: BTreeMap<String, ServiceManifest>,
    host_services: BTreeMap<(String, String), HostService>,
    service_releases: BTreeMap<(String, String), ServiceRelease>,
    service_routes: BTreeMap<(String, String), ServiceRoute>,
    service_migration_records: BTreeMap<(String, String), ServiceMigrationRecord>,
    service_permission_records: BTreeMap<(String, String), ServicePermissionRecord>,
    service_frontend_entries: BTreeMap<String, ServiceFrontendEntry>,
    service_redis_resources: BTreeMap<(String, String), ServiceRedisResource>,
    service_storage_resources: BTreeMap<(String, String, String), ServiceStorageResource>,
    rendered_service_configs: BTreeMap<(String, String), RenderedServiceConfig>,
    nodes: BTreeMap<String, NodeRecord>,
    service_api_surfaces: BTreeMap<(String, String, String), ServiceApiSurface>,
    deployed_service_apis: BTreeMap<(String, String, String, String), DeployedServiceApi>,
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

    pub fn host_services(&self) -> Vec<HostService> {
        self.host_services.values().cloned().collect()
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

    pub fn nodes(&self) -> Vec<NodeRecord> {
        self.nodes.values().cloned().collect()
    }

    pub fn service_api_surfaces(&self) -> Vec<ServiceApiSurface> {
        self.service_api_surfaces.values().cloned().collect()
    }

    pub fn deployed_service_apis(&self) -> Vec<DeployedServiceApi> {
        self.deployed_service_apis.values().cloned().collect()
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
        self.delete_host_services_for_service(service_id)?;
        self.service_releases
            .retain(|(service_name, _), _| service_name != service_id);
        self.delete_service_routes_for_service(service_id)?;
        self.delete_service_migration_records_for_service(service_id)?;
        self.delete_service_permission_records_for_service(service_id)?;
        self.delete_service_frontend_entry(service_id)?;
        self.delete_service_redis_resources_for_service(service_id)?;
        self.delete_service_storage_resources_for_service(service_id)?;
        self.delete_rendered_service_configs_for_service(service_id)?;
        self.delete_service_api_surfaces_for_service(service_id)?;
        self.delete_deployed_service_apis_for_service(service_id)?;
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

    fn list_host_services(&self) -> Result<Vec<HostService>> {
        Ok(self.host_services())
    }

    fn get_host_service(&self, host_ip: &str, service_name: &str) -> Result<Option<HostService>> {
        Ok(self
            .host_services
            .get(&(host_ip.to_string(), service_name.to_string()))
            .cloned())
    }

    fn upsert_host_service(&mut self, host_service: HostService) -> Result<()> {
        validate_host_service(&host_service)?;
        self.host_services.insert(
            (
                host_service.host_ip.clone(),
                host_service.service_name.clone(),
            ),
            host_service,
        );
        Ok(())
    }

    fn delete_host_service(&mut self, host_ip: &str, service_name: &str) -> Result<()> {
        self.host_services
            .remove(&(host_ip.to_string(), service_name.to_string()));
        Ok(())
    }

    fn delete_host_services_for_service(&mut self, service_name: &str) -> Result<()> {
        self.host_services
            .retain(|(_, stored_service_name), _| stored_service_name != service_name);
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

    fn list_nodes(&self) -> Result<Vec<NodeRecord>> {
        Ok(self.nodes())
    }

    fn get_node(&self, node_id: &str) -> Result<Option<NodeRecord>> {
        Ok(self.nodes.get(node_id).cloned())
    }

    fn upsert_node(&mut self, node: NodeRecord) -> Result<()> {
        validate_node_record(&node)?;
        validate_node_tree_upsert(self.nodes.values(), &node)?;
        self.nodes.insert(node.node_id.clone(), node);
        Ok(())
    }

    fn delete_node(&mut self, node_id: &str) -> Result<()> {
        if self
            .nodes
            .values()
            .any(|node| node.parent_node_id == node_id)
        {
            return Err(OrchestratorError::Dependency(format!(
                "node {node_id} has child nodes"
            )));
        }
        self.nodes.remove(node_id);
        Ok(())
    }

    fn list_service_api_surfaces(&self) -> Result<Vec<ServiceApiSurface>> {
        Ok(self.service_api_surfaces())
    }

    fn upsert_service_api_surface(&mut self, api: ServiceApiSurface) -> Result<()> {
        validate_service_api_surface(&api)?;
        self.service_api_surfaces.insert(
            (
                api.service_name.clone(),
                api.version.clone(),
                api.api_id.clone(),
            ),
            api,
        );
        Ok(())
    }

    fn delete_service_api_surfaces_for_service(&mut self, service_name: &str) -> Result<()> {
        self.service_api_surfaces
            .retain(|(stored_service_name, _, _), _| stored_service_name != service_name);
        Ok(())
    }

    fn list_deployed_service_apis(&self) -> Result<Vec<DeployedServiceApi>> {
        Ok(self.deployed_service_apis())
    }

    fn upsert_deployed_service_api(&mut self, api: DeployedServiceApi) -> Result<()> {
        validate_deployed_service_api(&api)?;
        if !self.nodes.values().any(|node| node.host_ip == api.host_ip) {
            return Err(OrchestratorError::Dependency(format!(
                "deployed api references host_ip {} without node",
                api.host_ip
            )));
        }
        if !self.endpoints.contains_key(&api.endpoint) {
            return Err(OrchestratorError::Dependency(format!(
                "deployed api references missing endpoint {}",
                api.endpoint
            )));
        }
        if !self.service_api_surfaces.contains_key(&(
            api.service_name.clone(),
            api.version.clone(),
            api.api_id.clone(),
        )) {
            return Err(OrchestratorError::Dependency(format!(
                "deployed api references missing api surface {}@{}:{}",
                api.service_name, api.version, api.api_id
            )));
        }
        self.deployed_service_apis.insert(
            (
                api.host_ip.clone(),
                api.service_name.clone(),
                api.api_id.clone(),
                api.endpoint.clone(),
            ),
            api,
        );
        Ok(())
    }

    fn delete_deployed_service_apis_for_service(&mut self, service_name: &str) -> Result<()> {
        self.deployed_service_apis
            .retain(|(_, stored_service_name, _, _), _| stored_service_name != service_name);
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
        self.deployed_service_apis
            .retain(|(_, _, _, stored_endpoint), _| stored_endpoint != endpoint);
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
