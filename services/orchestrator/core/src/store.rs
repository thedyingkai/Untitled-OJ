use crate::{
    DeployedServiceApi, DiagnosticReport, DockerComposeDriver, DriverRequest, DriverResult,
    EffectiveApiRoute, Endpoint, EndpointHealthResult, EndpointProbe, ExecutionDriver,
    ExternalEndpointDriver, HostService, Link, LinkHealthResult, LocalProcessDriver, LogView,
    NodeRecord, Operation, OperationLock, OperationLogRecord, OperationStatus, OrchestratorError,
    RenderedServiceConfig, Result, RuntimeMode, ServiceApiSurface, ServiceFrontendEntry,
    ServiceManifest, ServiceMigrationRecord, ServicePermissionRecord, ServiceRedisResource,
    ServiceRelease, ServiceReleaseManifest, ServiceRoute, ServiceStorageResource,
    StaticEndpointProbe, Topology, TopologySnapshot, build_diagnostic_report, build_topology,
    check_endpoint_health_with_probe, check_link_health, export_diagnostic_report,
    operation_log_record, operation_step_log_record, parse_endpoint_id, start_operation,
    succeed_operation, validate_deployed_service_api, validate_endpoint, validate_endpoint_id,
    validate_host_service, validate_link, validate_log_view, validate_node_record,
    validate_rendered_service_config, validate_service_api_surface,
    validate_service_frontend_entry, validate_service_manifest, validate_service_migration_record,
    validate_service_permission_record, validate_service_redis_resource, validate_service_release,
    validate_service_release_record, validate_service_route, validate_service_storage_resource,
    validate_topology,
};
use postgres::NoTls;
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::path::{Component, Path, PathBuf};
use std::time::Duration;
use tar::Archive;
use ureq::Agent;
use zip::ZipArchive;

pub trait OrchestratorStore {
    fn list_services(&self) -> Result<Vec<ServiceManifest>>;
    fn get_service(&self, service_id: &str) -> Result<Option<ServiceManifest>>;
    fn upsert_service(&mut self, service: ServiceManifest) -> Result<()>;
    fn delete_service(&mut self, service_id: &str) -> Result<()>;

    fn list_host_services(&self) -> Result<Vec<HostService>>;
    fn get_host_service(&self, host_ip: &str, service_name: &str) -> Result<Option<HostService>>;
    fn upsert_host_service(&mut self, host_service: HostService) -> Result<()>;
    fn delete_host_service(&mut self, host_ip: &str, service_name: &str) -> Result<()>;
    fn delete_host_services_for_service(&mut self, service_name: &str) -> Result<()>;

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

    fn list_nodes(&self) -> Result<Vec<NodeRecord>>;
    fn get_node(&self, node_id: &str) -> Result<Option<NodeRecord>>;
    fn upsert_node(&mut self, node: NodeRecord) -> Result<()>;
    fn delete_node(&mut self, node_id: &str) -> Result<()>;

    fn list_service_api_surfaces(&self) -> Result<Vec<ServiceApiSurface>>;
    fn upsert_service_api_surface(&mut self, api: ServiceApiSurface) -> Result<()>;
    fn delete_service_api_surfaces_for_service(&mut self, service_name: &str) -> Result<()>;

    fn list_deployed_service_apis(&self) -> Result<Vec<DeployedServiceApi>>;
    fn upsert_deployed_service_api(&mut self, api: DeployedServiceApi) -> Result<()>;
    fn delete_deployed_service_apis_for_service(&mut self, service_name: &str) -> Result<()>;

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

    fn host_services(&self) -> Result<Vec<HostService>> {
        self.list_host_services()
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

    fn nodes(&self) -> Result<Vec<NodeRecord>> {
        self.list_nodes()
    }

    fn service_api_surfaces(&self) -> Result<Vec<ServiceApiSurface>> {
        self.list_service_api_surfaces()
    }

    fn deployed_service_apis(&self) -> Result<Vec<DeployedServiceApi>> {
        self.list_deployed_service_apis()
    }

    fn ancestors_of(&self, node_id: &str) -> Result<Vec<NodeRecord>> {
        ancestors_of_from_nodes(self.list_nodes()?, node_id)
    }

    fn descendants_of(&self, node_id: &str) -> Result<Vec<NodeRecord>> {
        descendants_of_from_nodes(self.list_nodes()?, node_id)
    }

    fn effective_api_routes(&self, node_id: &str) -> Result<Vec<EffectiveApiRoute>> {
        effective_api_routes_from_registry(
            node_id,
            self.list_nodes()?,
            self.list_service_api_surfaces()?,
            self.list_deployed_service_apis()?,
        )
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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AuthPermissionRegistration {
    pub service_name: String,
    pub permissions: Vec<String>,
    pub service_identity: Option<AuthServiceIdentityRegistration>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AuthServiceIdentityRegistration {
    pub service_name: String,
    pub allowed_apis: Vec<String>,
    pub grants: Vec<AuthServiceIdentityGrant>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AuthServiceIdentityGrant {
    pub api_id: String,
    pub permission: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AuthPermissionRegistrationResult {
    pub status: String,
    pub message: String,
    pub endpoint: String,
    pub registered: usize,
}

pub trait AuthPermissionRegistrar {
    fn register_permissions(
        &self,
        request: &AuthPermissionRegistration,
    ) -> Result<AuthPermissionRegistrationResult>;
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RedisProvisionRequest {
    pub service_name: String,
    pub resources: Vec<ServiceRedisResource>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RedisProvisionedResource {
    pub name: String,
    pub kind: String,
    pub stream: String,
    pub consumer_group: String,
    pub status: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RedisProvisionResult {
    pub status: String,
    pub message: String,
    pub endpoint: String,
    pub provisioned: Vec<RedisProvisionedResource>,
}

pub trait RedisResourceProvisioner {
    fn provision_resources(&self, request: &RedisProvisionRequest) -> Result<RedisProvisionResult>;
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StorageProvisionRequest {
    pub service_name: String,
    pub resources: Vec<ServiceStorageResource>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StorageProvisionedResource {
    pub object_type: String,
    pub bucket: String,
    pub status: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StorageProvisionResult {
    pub status: String,
    pub message: String,
    pub endpoint: String,
    pub provisioned: Vec<StorageProvisionedResource>,
}

pub trait StorageResourceProvisioner {
    fn provision_resources(
        &self,
        request: &StorageProvisionRequest,
    ) -> Result<StorageProvisionResult>;
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MigrationExecutionRequest {
    pub service_name: String,
    pub migrations: Vec<crate::ReleaseMigrationDecl>,
    pub release_source_url: String,
    pub dry_run: bool,
    pub allow_destructive: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MigrationExecutionRecord {
    pub migration_version: String,
    pub path: String,
    pub checksum: String,
    pub status: String,
    pub applied_at: String,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MigrationExecutionResult {
    pub status: String,
    pub message: String,
    pub runner: String,
    pub dry_run: bool,
    pub executed: Vec<MigrationExecutionRecord>,
}

pub trait MigrationRunner {
    fn execute_migrations(
        &self,
        request: &MigrationExecutionRequest,
    ) -> Result<MigrationExecutionResult>;
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ReleasePackageLoadRequest {
    pub service_name: String,
    pub version: String,
    pub source_url: String,
    pub expected_manifest: Option<ServiceReleaseManifest>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ReleasePackageLoadResult {
    pub status: String,
    pub message: String,
    pub source_url: String,
    pub manifest_loaded: bool,
    pub checksum: String,
}

pub trait ReleasePackageLoader {
    fn load_release_package(
        &self,
        request: &ReleasePackageLoadRequest,
    ) -> Result<ReleasePackageLoadResult>;
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GatewayRoutePublishRequest {
    pub operation_id: String,
    pub service_name: String,
    pub routes: Vec<ServiceRoute>,
    pub effective_routes: Vec<EffectiveApiRoute>,
    pub node_id: String,
    pub api_count: usize,
    pub force_reload: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GatewayRoutePublishResult {
    pub status: String,
    pub message: String,
    pub endpoint: String,
    pub route_count: usize,
    pub reloaded: bool,
}

pub trait GatewayRoutePublisher {
    fn publish_routes(
        &self,
        request: &GatewayRoutePublishRequest,
    ) -> Result<GatewayRoutePublishResult>;
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct NodeServiceDispatchRequest {
    pub operation_id: String,
    pub service: ServiceManifest,
    pub release: Option<ServiceReleaseManifest>,
    pub host_service: HostService,
    pub endpoint: Endpoint,
    pub rendered_config: serde_json::Value,
    pub package_load: Option<ReleasePackageLoadResult>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct NodeServiceDispatchResult {
    pub status: String,
    pub message: String,
    pub endpoint: String,
    pub accepted: bool,
}

pub trait NodeServiceDispatcher {
    fn dispatch_service(
        &self,
        request: &NodeServiceDispatchRequest,
    ) -> Result<NodeServiceDispatchResult>;
}

#[derive(Debug, Default, Clone)]
pub struct DeferredAuthPermissionRegistrar;

#[derive(Debug, Default, Clone)]
pub struct ConfiguredAuthPermissionRegistrar {
    sync_enabled: bool,
    http: Option<HttpAuthPermissionRegistrar>,
}

#[derive(Debug, Clone)]
pub struct HttpAuthPermissionRegistrar {
    endpoint: String,
    token: String,
    timeout: Duration,
}

#[derive(Debug, Default, Clone)]
pub struct DeferredRedisResourceProvisioner;

#[derive(Debug, Default, Clone)]
pub struct ConfiguredRedisResourceProvisioner {
    sync_enabled: bool,
    tcp: Option<TcpRedisResourceProvisioner>,
}

#[derive(Debug, Clone)]
pub struct TcpRedisResourceProvisioner {
    endpoint: String,
    timeout: Duration,
}

#[derive(Debug, Default, Clone)]
pub struct DeferredStorageResourceProvisioner;

#[derive(Debug, Default, Clone)]
pub struct ConfiguredStorageResourceProvisioner {
    sync_enabled: bool,
    http: Option<HttpStorageResourceProvisioner>,
}

#[derive(Debug, Clone)]
pub struct HttpStorageResourceProvisioner {
    endpoint: String,
    timeout: Duration,
}

#[derive(Debug, Default, Clone)]
pub struct DeferredMigrationRunner;

#[derive(Debug, Default, Clone)]
pub struct ConfiguredMigrationRunner {
    execution_enabled: bool,
    runner: Option<LocalSqlMigrationRunner>,
}

#[derive(Debug, Clone)]
pub struct LocalSqlMigrationRunner {
    root: PathBuf,
    database_url: Option<String>,
    service_database_urls: BTreeMap<String, String>,
    dry_run: bool,
    allow_destructive: bool,
}

#[derive(Debug, Default, Clone)]
pub struct DeferredReleasePackageLoader;

#[derive(Debug, Default, Clone)]
pub struct ConfiguredReleasePackageLoader {
    load_enabled: bool,
    loader: Option<LocalReleasePackageLoader>,
}

#[derive(Debug, Clone)]
pub struct LocalReleasePackageLoader {
    root: PathBuf,
    timeout: Duration,
    max_manifest_bytes: usize,
    max_package_bytes: usize,
}

#[derive(Debug, Default, Clone)]
pub struct DeferredGatewayRoutePublisher;

#[derive(Debug, Default, Clone)]
pub struct ConfiguredGatewayRoutePublisher {
    publish_enabled: bool,
    http: Option<HttpGatewayRoutePublisher>,
}

#[derive(Debug, Clone)]
pub struct HttpGatewayRoutePublisher {
    endpoint: String,
    token: Option<String>,
    timeout: Duration,
}

#[derive(Debug, Default, Clone)]
pub struct DeferredNodeServiceDispatcher;

#[derive(Debug, Default, Clone)]
pub struct ConfiguredNodeServiceDispatcher {
    dispatch_enabled: bool,
    http: Option<HttpNodeServiceDispatcher>,
}

#[derive(Debug, Clone)]
pub struct HttpNodeServiceDispatcher {
    endpoint: String,
    token: Option<String>,
    timeout: Duration,
}

pub struct OperationExecutor<
    'a,
    S: OrchestratorStore,
    P: EndpointProbe = StaticEndpointProbe,
    A: AuthPermissionRegistrar = DeferredAuthPermissionRegistrar,
    R: RedisResourceProvisioner = DeferredRedisResourceProvisioner,
    T: StorageResourceProvisioner = DeferredStorageResourceProvisioner,
    M: MigrationRunner = DeferredMigrationRunner,
    L: ReleasePackageLoader = DeferredReleasePackageLoader,
    G: GatewayRoutePublisher = DeferredGatewayRoutePublisher,
    N: NodeServiceDispatcher = DeferredNodeServiceDispatcher,
> {
    store: &'a mut S,
    endpoint_probe: P,
    auth_permission_registrar: A,
    redis_resource_provisioner: R,
    storage_resource_provisioner: T,
    migration_runner: M,
    release_package_loader: L,
    gateway_route_publisher: G,
    node_service_dispatcher: N,
    service_driver_execution_enabled: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
struct ReleaseInstallPreviousState {
    service: Option<ServiceManifest>,
    host_services: Vec<HostService>,
    endpoints: Vec<Endpoint>,
    links: Vec<Link>,
    log_views: Vec<LogView>,
    releases: Vec<ServiceRelease>,
    routes: Vec<ServiceRoute>,
    migrations: Vec<ServiceMigrationRecord>,
    permissions: Vec<ServicePermissionRecord>,
    frontends: Vec<ServiceFrontendEntry>,
    redis: Vec<ServiceRedisResource>,
    storage: Vec<ServiceStorageResource>,
    configs: Vec<RenderedServiceConfig>,
    api_surfaces: Vec<ServiceApiSurface>,
    deployed_apis: Vec<DeployedServiceApi>,
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
    api_surfaces: Vec<ServiceApiSurface>,
    deployed_apis: Vec<DeployedServiceApi>,
}

#[derive(Debug, Clone, Default)]
struct PendingMigrationPlan {
    pending: Vec<crate::ReleaseMigrationDecl>,
    already_applied: Vec<MigrationExecutionRecord>,
}

impl AuthPermissionRegistrar for DeferredAuthPermissionRegistrar {
    fn register_permissions(
        &self,
        request: &AuthPermissionRegistration,
    ) -> Result<AuthPermissionRegistrationResult> {
        Ok(AuthPermissionRegistrationResult {
            status: "skipped".to_string(),
            message: format!(
                "auth-service permission registrar is not configured for {}",
                request.service_name
            ),
            endpoint: String::new(),
            registered: 0,
        })
    }
}

impl ConfiguredAuthPermissionRegistrar {
    pub fn from_env() -> Self {
        let sync_enabled = std::env::var("ORCHESTRATOR_AUTH_PERMISSION_SYNC")
            .ok()
            .is_some_and(|value| {
                matches!(
                    value.trim().to_ascii_lowercase().as_str(),
                    "1" | "true" | "yes" | "on"
                )
            });
        let http = if sync_enabled {
            HttpAuthPermissionRegistrar::from_env()
        } else {
            None
        };
        Self { sync_enabled, http }
    }
}

impl AuthPermissionRegistrar for ConfiguredAuthPermissionRegistrar {
    fn register_permissions(
        &self,
        request: &AuthPermissionRegistration,
    ) -> Result<AuthPermissionRegistrationResult> {
        if let Some(http) = self.http.as_ref() {
            return http.register_permissions(request);
        }
        Ok(AuthPermissionRegistrationResult {
            status: "skipped".to_string(),
            message: if self.sync_enabled {
                "AUTH_SERVICE_ENDPOINT or AUTH_SERVICE_ADMIN_TOKEN is not configured".to_string()
            } else {
                "ORCHESTRATOR_AUTH_PERMISSION_SYNC is not enabled".to_string()
            },
            endpoint: String::new(),
            registered: 0,
        })
    }
}

impl HttpAuthPermissionRegistrar {
    pub fn new(endpoint: impl Into<String>, token: impl Into<String>) -> Self {
        Self {
            endpoint: endpoint.into().trim_end_matches('/').to_string(),
            token: token.into(),
            timeout: Duration::from_secs(5),
        }
    }

    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    pub fn from_env() -> Option<Self> {
        let endpoint = std::env::var("AUTH_SERVICE_ENDPOINT")
            .ok()
            .map(|value| value.trim().trim_end_matches('/').to_string())
            .filter(|value| !value.is_empty())?;
        let token = std::env::var("AUTH_SERVICE_ADMIN_TOKEN")
            .ok()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())?;
        Some(Self::new(endpoint, token))
    }
}

impl AuthPermissionRegistrar for HttpAuthPermissionRegistrar {
    fn register_permissions(
        &self,
        request: &AuthPermissionRegistration,
    ) -> Result<AuthPermissionRegistrationResult> {
        if request.permissions.is_empty() && request.service_identity.is_none() {
            return Ok(AuthPermissionRegistrationResult {
                status: "skipped".to_string(),
                message: "release declares no permissions".to_string(),
                endpoint: self.endpoint.clone(),
                registered: 0,
            });
        }
        if self.endpoint.trim().is_empty() || self.token.trim().is_empty() {
            return Ok(AuthPermissionRegistrationResult {
                status: "skipped".to_string(),
                message: "auth-service endpoint or admin token is not configured".to_string(),
                endpoint: self.endpoint.clone(),
                registered: 0,
            });
        }
        let url = format!(
            "{}/auth/admin/services/{}/permissions",
            self.endpoint.trim_end_matches('/'),
            request.service_name
        );
        let mut body = serde_json::json!({
            "permissions": request.permissions.iter().map(|permission| {
                serde_json::json!({
                    "code": permission,
                    "name": permission,
                    "description": format!("{} release permission", request.service_name),
                })
            }).collect::<Vec<_>>(),
            "default_role_bindings": [],
        });
        if let Some(identity) = request.service_identity.as_ref() {
            body["service_identity"] = serde_json::json!({
                "service_name": identity.service_name,
                "allowed_apis": identity.allowed_apis,
                "grants": identity.grants.iter().map(|grant| {
                    serde_json::json!({
                        "api_id": grant.api_id,
                        "permission": grant.permission,
                    })
                }).collect::<Vec<_>>(),
            });
        }
        let agent: Agent = Agent::config_builder()
            .timeout_global(Some(self.timeout))
            .http_status_as_error(false)
            .max_redirects(0)
            .proxy(None)
            .build()
            .into();
        let response = agent
            .post(&url)
            .header("Content-Type", "application/json")
            .header("Authorization", format!("Bearer {}", self.token.trim()))
            .send(serde_json::to_string(&body)?)
            .map_err(|err| {
                OrchestratorError::Dependency(format!(
                    "auth-service permission registration request failed: {err}"
                ))
            })?;
        let status = response.status().as_u16();
        if (200..=299).contains(&status) {
            Ok(AuthPermissionRegistrationResult {
                status: "registered".to_string(),
                message: format!("auth-service returned {status}"),
                endpoint: self.endpoint.clone(),
                registered: request.permissions.len(),
            })
        } else {
            Err(OrchestratorError::Dependency(format!(
                "auth-service permission registration failed: http {status}"
            )))
        }
    }
}

impl RedisResourceProvisioner for DeferredRedisResourceProvisioner {
    fn provision_resources(&self, request: &RedisProvisionRequest) -> Result<RedisProvisionResult> {
        Ok(RedisProvisionResult {
            status: "skipped".to_string(),
            message: format!(
                "redis resource provisioner is not configured for {}",
                request.service_name
            ),
            endpoint: String::new(),
            provisioned: Vec::new(),
        })
    }
}

impl ConfiguredRedisResourceProvisioner {
    pub fn from_env() -> Self {
        let sync_enabled = std::env::var("ORCHESTRATOR_REDIS_RESOURCE_SYNC")
            .ok()
            .is_some_and(|value| {
                matches!(
                    value.trim().to_ascii_lowercase().as_str(),
                    "1" | "true" | "yes" | "on"
                )
            });
        let tcp = if sync_enabled {
            TcpRedisResourceProvisioner::from_env()
        } else {
            None
        };
        Self { sync_enabled, tcp }
    }
}

impl RedisResourceProvisioner for ConfiguredRedisResourceProvisioner {
    fn provision_resources(&self, request: &RedisProvisionRequest) -> Result<RedisProvisionResult> {
        if let Some(tcp) = self.tcp.as_ref() {
            return tcp.provision_resources(request);
        }
        Ok(RedisProvisionResult {
            status: "skipped".to_string(),
            message: if self.sync_enabled {
                "REDIS_URL or REDIS_ENDPOINT is not configured".to_string()
            } else {
                "ORCHESTRATOR_REDIS_RESOURCE_SYNC is not enabled".to_string()
            },
            endpoint: String::new(),
            provisioned: Vec::new(),
        })
    }
}

impl TcpRedisResourceProvisioner {
    pub fn new(endpoint: impl Into<String>) -> Self {
        Self {
            endpoint: endpoint.into(),
            timeout: Duration::from_secs(5),
        }
    }

    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    pub fn from_env() -> Option<Self> {
        let endpoint = std::env::var("REDIS_ENDPOINT")
            .ok()
            .or_else(|| std::env::var("REDIS_URL").ok())
            .map(|value| redis_socket_from_endpoint(&value))
            .filter(|value| !value.trim().is_empty())?;
        Some(Self::new(endpoint))
    }
}

impl RedisResourceProvisioner for TcpRedisResourceProvisioner {
    fn provision_resources(&self, request: &RedisProvisionRequest) -> Result<RedisProvisionResult> {
        if request.resources.is_empty() {
            return Ok(RedisProvisionResult {
                status: "skipped".to_string(),
                message: "release declares no redis resources".to_string(),
                endpoint: self.endpoint.clone(),
                provisioned: Vec::new(),
            });
        }
        let mut connection = SimpleRedisConnection::connect(&self.endpoint, self.timeout)?;
        let mut provisioned = Vec::new();
        for resource in &request.resources {
            match resource.kind.as_str() {
                "stream" => {
                    let stream = redis_stream_name(resource);
                    connection.send_command(&[
                        "XGROUP",
                        "CREATE",
                        &stream,
                        "bootstrap",
                        "$",
                        "MKSTREAM",
                    ])?;
                    provisioned.push(RedisProvisionedResource {
                        name: resource.name.clone(),
                        kind: resource.kind.clone(),
                        stream,
                        consumer_group: String::new(),
                        status: "created".to_string(),
                    });
                }
                "consumer-group" => {
                    let stream = redis_stream_name(resource);
                    let group = redis_consumer_group_name(resource);
                    connection
                        .send_command(&["XGROUP", "CREATE", &stream, &group, "$", "MKSTREAM"])?;
                    provisioned.push(RedisProvisionedResource {
                        name: resource.name.clone(),
                        kind: resource.kind.clone(),
                        stream,
                        consumer_group: group,
                        status: "created".to_string(),
                    });
                }
                "pubsub" | "hash" | "string" | "zset" | "lock" => {
                    provisioned.push(RedisProvisionedResource {
                        name: resource.name.clone(),
                        kind: resource.kind.clone(),
                        stream: redis_stream_name(resource),
                        consumer_group: String::new(),
                        status: "registry-only".to_string(),
                    });
                }
                value => {
                    return Err(OrchestratorError::InvalidManifest(format!(
                        "unsupported redis resource kind {value}"
                    )));
                }
            }
        }
        Ok(RedisProvisionResult {
            status: "created".to_string(),
            message: format!("provisioned {} redis resources", provisioned.len()),
            endpoint: self.endpoint.clone(),
            provisioned,
        })
    }
}

impl StorageResourceProvisioner for DeferredStorageResourceProvisioner {
    fn provision_resources(
        &self,
        request: &StorageProvisionRequest,
    ) -> Result<StorageProvisionResult> {
        Ok(StorageProvisionResult {
            status: "skipped".to_string(),
            message: format!(
                "storage resource provisioner is not configured for {}",
                request.service_name
            ),
            endpoint: String::new(),
            provisioned: Vec::new(),
        })
    }
}

impl ConfiguredStorageResourceProvisioner {
    pub fn from_env() -> Self {
        let sync_enabled = std::env::var("ORCHESTRATOR_STORAGE_RESOURCE_SYNC")
            .ok()
            .is_some_and(|value| {
                matches!(
                    value.trim().to_ascii_lowercase().as_str(),
                    "1" | "true" | "yes" | "on"
                )
            });
        let http = if sync_enabled {
            HttpStorageResourceProvisioner::from_env()
        } else {
            None
        };
        Self { sync_enabled, http }
    }
}

impl StorageResourceProvisioner for ConfiguredStorageResourceProvisioner {
    fn provision_resources(
        &self,
        request: &StorageProvisionRequest,
    ) -> Result<StorageProvisionResult> {
        if let Some(http) = self.http.as_ref() {
            return http.provision_resources(request);
        }
        Ok(StorageProvisionResult {
            status: "skipped".to_string(),
            message: if self.sync_enabled {
                "STORAGE_SERVICE_ENDPOINT is not configured".to_string()
            } else {
                "ORCHESTRATOR_STORAGE_RESOURCE_SYNC is not enabled".to_string()
            },
            endpoint: String::new(),
            provisioned: Vec::new(),
        })
    }
}

impl HttpStorageResourceProvisioner {
    pub fn new(endpoint: impl Into<String>) -> Self {
        Self {
            endpoint: endpoint.into().trim_end_matches('/').to_string(),
            timeout: Duration::from_secs(5),
        }
    }

    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    pub fn from_env() -> Option<Self> {
        let endpoint = std::env::var("STORAGE_SERVICE_ENDPOINT")
            .ok()
            .map(|value| value.trim().trim_end_matches('/').to_string())
            .filter(|value| !value.is_empty())?;
        Some(Self::new(endpoint))
    }
}

impl StorageResourceProvisioner for HttpStorageResourceProvisioner {
    fn provision_resources(
        &self,
        request: &StorageProvisionRequest,
    ) -> Result<StorageProvisionResult> {
        if request.resources.is_empty() {
            return Ok(StorageProvisionResult {
                status: "skipped".to_string(),
                message: "release declares no storage resources".to_string(),
                endpoint: self.endpoint.clone(),
                provisioned: Vec::new(),
            });
        }
        if self.endpoint.trim().is_empty() {
            return Ok(StorageProvisionResult {
                status: "skipped".to_string(),
                message: "storage-service endpoint is not configured".to_string(),
                endpoint: String::new(),
                provisioned: Vec::new(),
            });
        }
        let agent: Agent = Agent::config_builder()
            .timeout_global(Some(self.timeout))
            .http_status_as_error(false)
            .max_redirects(0)
            .proxy(None)
            .build()
            .into();
        let mut provisioned = Vec::new();
        let mut seen_buckets = BTreeMap::<String, Vec<String>>::new();
        for resource in &request.resources {
            seen_buckets
                .entry(resource.bucket.clone())
                .or_default()
                .push(resource.object_type.clone());
        }
        for (bucket, object_types) in seen_buckets {
            let url = format!(
                "{}/api/storage/buckets/{}",
                self.endpoint.trim_end_matches('/'),
                bucket
            );
            let response = agent.put(&url).send_empty().map_err(|err| {
                OrchestratorError::Dependency(format!(
                    "storage-service bucket ensure request failed: {err}"
                ))
            })?;
            let status = response.status().as_u16();
            if !(200..=299).contains(&status) {
                return Err(OrchestratorError::Dependency(format!(
                    "storage-service bucket ensure failed for {bucket}: http {status}"
                )));
            }
            for object_type in object_types {
                provisioned.push(StorageProvisionedResource {
                    object_type,
                    bucket: bucket.clone(),
                    status: "ensured".to_string(),
                });
            }
        }
        Ok(StorageProvisionResult {
            status: "ensured".to_string(),
            message: format!("ensured {} storage resources", provisioned.len()),
            endpoint: self.endpoint.clone(),
            provisioned,
        })
    }
}

impl MigrationRunner for DeferredMigrationRunner {
    fn execute_migrations(
        &self,
        request: &MigrationExecutionRequest,
    ) -> Result<MigrationExecutionResult> {
        Ok(MigrationExecutionResult {
            status: if request.migrations.is_empty() {
                "skipped".to_string()
            } else {
                "deferred".to_string()
            },
            message: if request.migrations.is_empty() {
                "release declares no migrations".to_string()
            } else {
                format!(
                    "migration runner is not configured for {}",
                    request.service_name
                )
            },
            runner: "deferred".to_string(),
            dry_run: request.dry_run,
            executed: request
                .migrations
                .iter()
                .map(|migration| MigrationExecutionRecord {
                    migration_version: migration.version.clone(),
                    path: migration.path.clone(),
                    checksum: migration.checksum.clone(),
                    status: if request.migrations.is_empty() {
                        "skipped".to_string()
                    } else {
                        "registered".to_string()
                    },
                    applied_at: String::new(),
                    message: "migration remains registered until a runner is configured"
                        .to_string(),
                })
                .collect(),
        })
    }
}

impl ReleasePackageLoader for DeferredReleasePackageLoader {
    fn load_release_package(
        &self,
        request: &ReleasePackageLoadRequest,
    ) -> Result<ReleasePackageLoadResult> {
        Ok(ReleasePackageLoadResult {
            status: "planned".to_string(),
            message: format!(
                "release package fetch/load is deferred for {}",
                request.source_url
            ),
            source_url: request.source_url.clone(),
            manifest_loaded: false,
            checksum: String::new(),
        })
    }
}

impl ConfiguredReleasePackageLoader {
    pub fn from_env() -> Self {
        let load_enabled = std::env::var("ORCHESTRATOR_RELEASE_PACKAGE_LOAD")
            .ok()
            .is_some_and(|value| truthy(&value));
        let loader = if load_enabled {
            Some(LocalReleasePackageLoader::from_env())
        } else {
            None
        };
        Self {
            load_enabled,
            loader,
        }
    }
}

impl ReleasePackageLoader for ConfiguredReleasePackageLoader {
    fn load_release_package(
        &self,
        request: &ReleasePackageLoadRequest,
    ) -> Result<ReleasePackageLoadResult> {
        if let Some(loader) = self.loader.as_ref() {
            return loader.load_release_package(request);
        }
        Ok(ReleasePackageLoadResult {
            status: "planned".to_string(),
            message: if self.load_enabled {
                "release package loader is enabled but not configured".to_string()
            } else {
                "ORCHESTRATOR_RELEASE_PACKAGE_LOAD is not enabled".to_string()
            },
            source_url: request.source_url.clone(),
            manifest_loaded: false,
            checksum: String::new(),
        })
    }
}

impl LocalReleasePackageLoader {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            timeout: Duration::from_secs(15),
            max_manifest_bytes: 1024 * 1024,
            max_package_bytes: 64 * 1024 * 1024,
        }
    }

    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    pub fn with_max_manifest_bytes(mut self, max_manifest_bytes: usize) -> Self {
        self.max_manifest_bytes = max_manifest_bytes;
        self
    }

    pub fn with_max_package_bytes(mut self, max_package_bytes: usize) -> Self {
        self.max_package_bytes = max_package_bytes;
        self
    }

    pub fn from_env() -> Self {
        let root = std::env::var("ORCHESTRATOR_RELEASE_PACKAGE_ROOT")
            .ok()
            .filter(|value| !value.trim().is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("."));
        Self::new(root)
    }

    fn resolve_release_yaml(&self, source_url: &str) -> Result<PathBuf> {
        let source_url = source_url.trim();
        if source_url.starts_with("http://") || source_url.starts_with("https://") {
            return Err(OrchestratorError::Blocked(
                "network release package source should be fetched through fetch_remote_release_yaml"
                    .to_string(),
            ));
        }
        let without_scheme = source_url
            .strip_prefix("file://")
            .or_else(|| source_url.strip_prefix("local://"))
            .unwrap_or(source_url);
        let mut release_yaml = safe_child_path(&self.root, without_scheme)?;
        if release_yaml.is_dir() {
            release_yaml = release_yaml.join("release.yaml");
        }
        Ok(release_yaml)
    }

    fn fetch_remote_release_package(&self, source_url: &str) -> Result<Vec<u8>> {
        let agent: Agent = Agent::config_builder()
            .timeout_global(Some(self.timeout))
            .http_status_as_error(false)
            .max_redirects(0)
            .proxy(None)
            .build()
            .into();
        let response = agent.get(source_url).call().map_err(|err| {
            OrchestratorError::Dependency(format!(
                "release package fetch request failed for {source_url}: {err}"
            ))
        })?;
        let status = response.status().as_u16();
        if !(200..=299).contains(&status) {
            return Err(OrchestratorError::Dependency(format!(
                "release package fetch failed for {source_url}: http {status}"
            )));
        }
        let mut reader = response.into_body().into_reader();
        let mut body = Vec::new();
        reader
            .by_ref()
            .take((self.max_package_bytes as u64) + 1)
            .read_to_end(&mut body)
            .map_err(|err| {
                OrchestratorError::Dependency(format!(
                    "release package body read failed for {source_url}: {err}"
                ))
            })?;
        if body.len() > self.max_package_bytes {
            return Err(OrchestratorError::InvalidManifest(format!(
                "release package exceeds {} bytes",
                self.max_package_bytes
            )));
        }
        Ok(body)
    }

    fn load_remote_release_yaml(&self, source_url: &str) -> Result<String> {
        let body = self.fetch_remote_release_package(source_url)?;
        if release_package_source_is_yaml(source_url) || looks_like_yaml_manifest(&body) {
            return release_yaml_text_from_bytes(&body, source_url, self.max_manifest_bytes);
        }
        self.release_yaml_from_archive(source_url, &body)
    }

    fn release_yaml_from_archive(&self, source_url: &str, body: &[u8]) -> Result<String> {
        let package_root = self.archive_extract_root(source_url)?;
        fs::create_dir_all(&package_root).map_err(|err| {
            OrchestratorError::Dependency(format!(
                "create release package extract root {} failed: {err}",
                package_root.display()
            ))
        })?;
        if release_package_source_is_zip(source_url) || looks_like_zip(body) {
            extract_zip_release_package(body, &package_root)?;
        } else if release_package_source_is_tar(source_url) || looks_like_gzip(body) {
            extract_tar_release_package(source_url, body, &package_root)?;
        } else {
            return Err(OrchestratorError::InvalidManifest(format!(
                "release package {source_url} is not release.yaml or a supported archive"
            )));
        }
        let release_yaml = find_release_yaml_in_package(&package_root)?;
        let bytes = fs::read(&release_yaml).map_err(|err| {
            OrchestratorError::Dependency(format!(
                "read extracted release package manifest {} failed: {err}",
                release_yaml.display()
            ))
        })?;
        release_yaml_text_from_bytes(
            &bytes,
            &release_yaml.display().to_string(),
            self.max_manifest_bytes,
        )
    }

    fn archive_extract_root(&self, source_url: &str) -> Result<PathBuf> {
        let digest = Sha256::digest(source_url.as_bytes());
        let dirname = format!("release-package-{:x}", digest);
        Ok(self.root.join(".orchestrator-release-cache").join(dirname))
    }
}

fn release_yaml_text_from_bytes(
    body: &[u8],
    source_display: &str,
    max_manifest_bytes: usize,
) -> Result<String> {
    if body.len() > max_manifest_bytes {
        return Err(OrchestratorError::InvalidManifest(format!(
            "release package manifest exceeds {} bytes",
            max_manifest_bytes
        )));
    }
    String::from_utf8(body.to_vec()).map_err(|err| {
        OrchestratorError::InvalidManifest(format!(
            "release package manifest from {source_display} is not UTF-8: {err}"
        ))
    })
}

fn release_package_source_is_yaml(source_url: &str) -> bool {
    let source = source_url.to_ascii_lowercase();
    source.ends_with(".yaml") || source.ends_with(".yml")
}

fn release_package_source_is_zip(source_url: &str) -> bool {
    source_url.to_ascii_lowercase().ends_with(".zip")
}

fn release_package_source_is_tar(source_url: &str) -> bool {
    let source = source_url.to_ascii_lowercase();
    source.ends_with(".tar") || source.ends_with(".tar.gz") || source.ends_with(".tgz")
}

fn looks_like_yaml_manifest(body: &[u8]) -> bool {
    std::str::from_utf8(body).ok().is_some_and(|text| {
        let trimmed = text.trim_start_matches('\u{feff}').trim_start();
        trimmed.starts_with("service_name:") || trimmed.contains("\nservice_name:")
    })
}

fn looks_like_zip(body: &[u8]) -> bool {
    body.starts_with(b"PK\x03\x04")
        || body.starts_with(b"PK\x05\x06")
        || body.starts_with(b"PK\x07\x08")
}

fn looks_like_gzip(body: &[u8]) -> bool {
    body.starts_with(&[0x1f, 0x8b])
}

fn extract_zip_release_package(body: &[u8], package_root: &Path) -> Result<()> {
    clear_release_package_extract_root(package_root)?;
    let cursor = std::io::Cursor::new(body);
    let mut archive = ZipArchive::new(cursor).map_err(|err| {
        OrchestratorError::InvalidManifest(format!("release package zip is invalid: {err}"))
    })?;
    for index in 0..archive.len() {
        let mut file = archive.by_index(index).map_err(|err| {
            OrchestratorError::InvalidManifest(format!(
                "read release package zip entry failed: {err}"
            ))
        })?;
        let Some(enclosed) = file.enclosed_name().map(|path| path.to_path_buf()) else {
            return Err(OrchestratorError::UnsafePath(
                "release package zip entry escapes package root".to_string(),
            ));
        };
        let entry_path = archive_entry_path_str(&enclosed)?;
        let out_path = safe_child_path(package_root, entry_path)?;
        if file.is_dir() {
            fs::create_dir_all(&out_path).map_err(|err| {
                OrchestratorError::Dependency(format!(
                    "create release package directory {} failed: {err}",
                    out_path.display()
                ))
            })?;
            continue;
        }
        if let Some(parent) = out_path.parent() {
            fs::create_dir_all(parent).map_err(|err| {
                OrchestratorError::Dependency(format!(
                    "create release package directory {} failed: {err}",
                    parent.display()
                ))
            })?;
        }
        let mut out = fs::File::create(&out_path).map_err(|err| {
            OrchestratorError::Dependency(format!(
                "create release package file {} failed: {err}",
                out_path.display()
            ))
        })?;
        std::io::copy(&mut file, &mut out).map_err(|err| {
            OrchestratorError::Dependency(format!(
                "extract release package file {} failed: {err}",
                out_path.display()
            ))
        })?;
    }
    Ok(())
}

fn extract_tar_release_package(source_url: &str, body: &[u8], package_root: &Path) -> Result<()> {
    clear_release_package_extract_root(package_root)?;
    let lower = source_url.to_ascii_lowercase();
    if lower.ends_with(".tar.gz") || lower.ends_with(".tgz") || looks_like_gzip(body) {
        let decoder = flate2::read::GzDecoder::new(std::io::Cursor::new(body));
        extract_tar_entries(Archive::new(decoder), package_root)
    } else {
        extract_tar_entries(Archive::new(std::io::Cursor::new(body)), package_root)
    }
}

fn extract_tar_entries<R: Read>(mut archive: Archive<R>, package_root: &Path) -> Result<()> {
    let entries = archive.entries().map_err(|err| {
        OrchestratorError::InvalidManifest(format!("release package tar is invalid: {err}"))
    })?;
    for entry in entries {
        let mut entry = entry.map_err(|err| {
            OrchestratorError::InvalidManifest(format!(
                "read release package tar entry failed: {err}"
            ))
        })?;
        let path = entry.path().map_err(|err| {
            OrchestratorError::InvalidManifest(format!(
                "read release package tar path failed: {err}"
            ))
        })?;
        let entry_path = archive_entry_path_str(&path)?;
        let out_path = safe_child_path(package_root, entry_path)?;
        let entry_type = entry.header().entry_type();
        if entry_type.is_dir() {
            fs::create_dir_all(&out_path).map_err(|err| {
                OrchestratorError::Dependency(format!(
                    "create release package directory {} failed: {err}",
                    out_path.display()
                ))
            })?;
            continue;
        }
        if !entry_type.is_file() {
            continue;
        }
        if let Some(parent) = out_path.parent() {
            fs::create_dir_all(parent).map_err(|err| {
                OrchestratorError::Dependency(format!(
                    "create release package directory {} failed: {err}",
                    parent.display()
                ))
            })?;
        }
        entry.unpack(&out_path).map_err(|err| {
            OrchestratorError::Dependency(format!(
                "extract release package file {} failed: {err}",
                out_path.display()
            ))
        })?;
    }
    Ok(())
}

fn archive_entry_path_str(path: &Path) -> Result<&str> {
    path.to_str().ok_or_else(|| {
        OrchestratorError::InvalidManifest(
            "release package entry path must be valid UTF-8".to_string(),
        )
    })
}

fn clear_release_package_extract_root(package_root: &Path) -> Result<()> {
    if package_root.exists() {
        fs::remove_dir_all(package_root).map_err(|err| {
            OrchestratorError::Dependency(format!(
                "clear release package extract root {} failed: {err}",
                package_root.display()
            ))
        })?;
    }
    fs::create_dir_all(package_root).map_err(|err| {
        OrchestratorError::Dependency(format!(
            "create release package extract root {} failed: {err}",
            package_root.display()
        ))
    })
}

fn find_release_yaml_in_package(package_root: &Path) -> Result<PathBuf> {
    let root_manifest = package_root.join("release.yaml");
    if root_manifest.is_file() {
        return Ok(root_manifest);
    }
    let mut found = Vec::new();
    collect_release_yaml_paths(package_root, package_root, &mut found)?;
    match found.len() {
        1 => Ok(found.remove(0)),
        0 => Err(OrchestratorError::InvalidManifest(
            "release package archive does not contain release.yaml".to_string(),
        )),
        _ => Err(OrchestratorError::InvalidManifest(
            "release package archive contains multiple release.yaml files".to_string(),
        )),
    }
}

fn collect_release_yaml_paths(root: &Path, dir: &Path, found: &mut Vec<PathBuf>) -> Result<()> {
    for entry in fs::read_dir(dir).map_err(|err| {
        OrchestratorError::Dependency(format!(
            "read release package directory {} failed: {err}",
            dir.display()
        ))
    })? {
        let entry = entry.map_err(|err| {
            OrchestratorError::Dependency(format!(
                "read release package directory entry under {} failed: {err}",
                dir.display()
            ))
        })?;
        let path = entry.path();
        if path.is_dir() {
            collect_release_yaml_paths(root, &path, found)?;
        } else if path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name == "release.yaml")
        {
            if !path.starts_with(root) {
                return Err(OrchestratorError::UnsafePath(
                    "release package manifest escapes package root".to_string(),
                ));
            }
            found.push(path);
        }
    }
    Ok(())
}

impl ReleasePackageLoader for LocalReleasePackageLoader {
    fn load_release_package(
        &self,
        request: &ReleasePackageLoadRequest,
    ) -> Result<ReleasePackageLoadResult> {
        let source_url = request.source_url.trim();
        let (text, source_display) =
            if source_url.starts_with("http://") || source_url.starts_with("https://") {
                (
                    self.load_remote_release_yaml(source_url)?,
                    source_url.to_string(),
                )
            } else {
                let release_yaml = self.resolve_release_yaml(&request.source_url)?;
                (
                    fs::read_to_string(&release_yaml).map_err(|err| {
                        OrchestratorError::Dependency(format!(
                            "read release package manifest {} failed: {err}",
                            release_yaml.display()
                        ))
                    })?,
                    release_yaml.display().to_string(),
                )
            };
        let loaded: ServiceReleaseManifest = serde_yaml::from_str(&text)?;
        validate_service_release(&loaded)?;
        if loaded.service_name != request.service_name || loaded.version != request.version {
            return Err(OrchestratorError::InvalidManifest(format!(
                "release package manifest {}@{} does not match requested {}@{}",
                loaded.service_name, loaded.version, request.service_name, request.version
            )));
        }
        if let Some(expected) = request.expected_manifest.as_ref() {
            let expected_value = serde_json::to_value(expected)?;
            let loaded_value = serde_json::to_value(&loaded)?;
            if expected_value != loaded_value {
                return Err(OrchestratorError::InvalidManifest(
                    "loaded release package manifest differs from operation release_manifest"
                        .to_string(),
                ));
            }
        }
        let checksum = format!("sha256:{:x}", Sha256::digest(text.as_bytes()));
        Ok(ReleasePackageLoadResult {
            status: "loaded".to_string(),
            message: format!("loaded release package manifest {source_display}"),
            source_url: request.source_url.clone(),
            manifest_loaded: true,
            checksum,
        })
    }
}

impl GatewayRoutePublisher for DeferredGatewayRoutePublisher {
    fn publish_routes(
        &self,
        request: &GatewayRoutePublishRequest,
    ) -> Result<GatewayRoutePublishResult> {
        Ok(GatewayRoutePublishResult {
            status: if !request.routes.is_empty() || request.force_reload {
                "planned".to_string()
            } else {
                "skipped".to_string()
            },
            message: if !request.routes.is_empty() || request.force_reload {
                format!(
                    "gateway route publisher is not configured for {}",
                    request.service_name
                )
            } else {
                format!(
                    "release declares no gateway routes or API surface changes for {}",
                    request.service_name
                )
            },
            endpoint: String::new(),
            route_count: gateway_publish_route_count(request),
            reloaded: false,
        })
    }
}

impl ConfiguredGatewayRoutePublisher {
    pub fn from_env() -> Self {
        let publish_enabled = env_flag("ORCHESTRATOR_GATEWAY_ROUTE_PUBLISH");
        let http = if publish_enabled {
            HttpGatewayRoutePublisher::from_env()
        } else {
            None
        };
        Self {
            publish_enabled,
            http,
        }
    }
}

impl GatewayRoutePublisher for ConfiguredGatewayRoutePublisher {
    fn publish_routes(
        &self,
        request: &GatewayRoutePublishRequest,
    ) -> Result<GatewayRoutePublishResult> {
        if let Some(http) = self.http.as_ref() {
            return http.publish_routes(request);
        }
        Ok(GatewayRoutePublishResult {
            status: if !request.routes.is_empty() || request.force_reload {
                "planned".to_string()
            } else {
                "skipped".to_string()
            },
            message: if !request.routes.is_empty() || request.force_reload {
                if self.publish_enabled {
                    "GATEWAY_ENDPOINT is not configured".to_string()
                } else {
                    "ORCHESTRATOR_GATEWAY_ROUTE_PUBLISH is not enabled".to_string()
                }
            } else {
                format!(
                    "release declares no gateway routes or API surface changes for {}",
                    request.service_name
                )
            },
            endpoint: String::new(),
            route_count: gateway_publish_route_count(request),
            reloaded: false,
        })
    }
}

impl HttpGatewayRoutePublisher {
    pub fn new(endpoint: impl Into<String>) -> Self {
        Self {
            endpoint: endpoint.into().trim_end_matches('/').to_string(),
            token: None,
            timeout: Duration::from_secs(5),
        }
    }

    pub fn with_token(mut self, token: impl Into<String>) -> Self {
        let token = token.into().trim().to_string();
        if !token.is_empty() {
            self.token = Some(token);
        }
        self
    }

    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    pub fn from_env() -> Option<Self> {
        let endpoint = std::env::var("GATEWAY_ENDPOINT")
            .ok()
            .map(|value| value.trim().trim_end_matches('/').to_string())
            .filter(|value| !value.is_empty())?;
        let mut publisher = Self::new(endpoint);
        if let Ok(token) = std::env::var("GATEWAY_ADMIN_TOKEN") {
            publisher = publisher.with_token(token);
        } else if let Ok(token) = std::env::var("ORCHESTRATOR_GATEWAY_TOKEN") {
            publisher = publisher.with_token(token);
        }
        Some(publisher)
    }
}

fn gateway_publish_route_count(request: &GatewayRoutePublishRequest) -> usize {
    if request.effective_routes.is_empty() {
        request.routes.len()
    } else {
        request.effective_routes.len()
    }
}

fn gateway_effective_route_items(routes: &[EffectiveApiRoute]) -> Result<Vec<serde_json::Value>> {
    routes
        .iter()
        .map(|route| {
            let upstream = endpoint_upstream_base_from_id(&route.provider_endpoint, &route.protocol)?;
            let enabled = route.status == "running";
            let proxy_enabled = enabled && !upstream.is_empty();
            Ok(serde_json::json!({
                "route_id": format!("{}:{}", route.provider_service_name, route.api_id),
                "node_id": route.node_id,
                "api_id": route.api_id,
                "provider_node_id": route.provider_node_id,
                "provider_host_ip": route.provider_host_ip,
                "provider_service_name": route.provider_service_name,
                "provider_endpoint": route.provider_endpoint,
                "owner_service_id": route.provider_service_name,
                "prefix": route.path_prefix,
                "path_prefix": route.path_prefix,
                "service_id": route.provider_service_name,
                "target_service": route.provider_service_name,
                "upstream_base": upstream,
                "auth_mode": route.auth_mode,
                "required_permission": required_permission_for_gateway(&route.permission),
                "permission": route.permission,
                "methods": route.methods,
                "enabled": enabled,
                "proxy_enabled": proxy_enabled,
                "priority": route.path_prefix.len(),
                "strip_prefix": "",
                "rewrite_prefix": "",
                "health_check_id": format!("{}-health", route.provider_service_name),
                "created_from": "orchestrator_effective_api_view",
                "visibility_source": route.visibility_source,
                "distance": route.distance,
                "status": if proxy_enabled { "active" } else if enabled { "blocked" } else { "disabled" },
                "service_status": route.status,
                "service_health": if enabled { "ok" } else { "unknown" },
                "conflicts": [],
                "warnings": [],
                "blocked_by": if upstream.is_empty() { vec!["missing endpoint"] } else { Vec::<&str>::new() },
            }))
        })
        .collect()
}

fn endpoint_upstream_base_from_id(endpoint: &str, protocol: &str) -> Result<String> {
    let identity = parse_endpoint_id(endpoint)?;
    Ok(format!(
        "{}://{}:{}",
        protocol.trim(),
        identity.host,
        identity.port
    ))
}

fn required_permission_for_gateway(permission: &str) -> String {
    let permission = permission.trim();
    if permission == "public" {
        String::new()
    } else {
        permission.to_string()
    }
}

impl GatewayRoutePublisher for HttpGatewayRoutePublisher {
    fn publish_routes(
        &self,
        request: &GatewayRoutePublishRequest,
    ) -> Result<GatewayRoutePublishResult> {
        if request.routes.is_empty() && !request.force_reload {
            return Ok(GatewayRoutePublishResult {
                status: "skipped".to_string(),
                message: format!(
                    "release declares no gateway routes or API surface changes for {}",
                    request.service_name
                ),
                endpoint: self.endpoint.clone(),
                route_count: 0,
                reloaded: false,
            });
        }
        if self.endpoint.trim().is_empty() {
            return Ok(GatewayRoutePublishResult {
                status: "planned".to_string(),
                message: "gateway endpoint is not configured".to_string(),
                endpoint: String::new(),
                route_count: gateway_publish_route_count(request),
                reloaded: false,
            });
        }
        let url = format!(
            "{}/api/admin/orchestrator/routes/reload",
            self.endpoint.trim_end_matches('/')
        );
        let body = serde_json::json!({
            "operation_id": request.operation_id,
            "service_name": request.service_name,
            "version": "1",
            "node_id": request.node_id,
            "pushed_route_table": true,
            "routes": gateway_effective_route_items(&request.effective_routes)?,
            "warnings": [],
            "can_proxy": !request.effective_routes.is_empty(),
        });
        let agent: Agent = Agent::config_builder()
            .timeout_global(Some(self.timeout))
            .http_status_as_error(false)
            .max_redirects(0)
            .proxy(None)
            .build()
            .into();
        let mut builder = agent.post(&url).header("Content-Type", "application/json");
        if let Some(token) = self.token.as_ref() {
            builder = builder.header("Authorization", format!("Bearer {}", token.trim()));
        }
        let response = builder.send(serde_json::to_string(&body)?).map_err(|err| {
            OrchestratorError::Dependency(format!("gateway route publish request failed: {err}"))
        })?;
        let status = response.status().as_u16();
        if (200..=299).contains(&status) {
            Ok(GatewayRoutePublishResult {
                status: "published".to_string(),
                message: format!("gateway route table reload accepted: http {status}"),
                endpoint: self.endpoint.clone(),
                route_count: gateway_publish_route_count(request),
                reloaded: true,
            })
        } else {
            Err(OrchestratorError::Dependency(format!(
                "gateway route publish failed: http {status}"
            )))
        }
    }
}

impl NodeServiceDispatcher for DeferredNodeServiceDispatcher {
    fn dispatch_service(
        &self,
        request: &NodeServiceDispatchRequest,
    ) -> Result<NodeServiceDispatchResult> {
        Ok(NodeServiceDispatchResult {
            status: "planned".to_string(),
            message: format!(
                "node-mode dispatch is not configured for {}",
                request.service.id
            ),
            endpoint: String::new(),
            accepted: false,
        })
    }
}

impl ConfiguredNodeServiceDispatcher {
    pub fn from_env() -> Self {
        let dispatch_enabled = env_flag("ORCHESTRATOR_NODE_DISPATCH");
        let http = if dispatch_enabled {
            HttpNodeServiceDispatcher::from_env()
        } else {
            None
        };
        Self {
            dispatch_enabled,
            http,
        }
    }
}

impl NodeServiceDispatcher for ConfiguredNodeServiceDispatcher {
    fn dispatch_service(
        &self,
        request: &NodeServiceDispatchRequest,
    ) -> Result<NodeServiceDispatchResult> {
        if let Some(http) = self.http.as_ref() {
            return http.dispatch_service(request);
        }
        Ok(NodeServiceDispatchResult {
            status: "planned".to_string(),
            message: if self.dispatch_enabled {
                "ORCHESTRATOR_NODE_ENDPOINT is not configured".to_string()
            } else {
                "ORCHESTRATOR_NODE_DISPATCH is not enabled".to_string()
            },
            endpoint: String::new(),
            accepted: false,
        })
    }
}

impl HttpNodeServiceDispatcher {
    pub fn new(endpoint: impl Into<String>) -> Self {
        Self {
            endpoint: endpoint.into().trim_end_matches('/').to_string(),
            token: None,
            timeout: Duration::from_secs(10),
        }
    }

    pub fn with_token(mut self, token: impl Into<String>) -> Self {
        let token = token.into().trim().to_string();
        if !token.is_empty() {
            self.token = Some(token);
        }
        self
    }

    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    pub fn from_env() -> Option<Self> {
        let endpoint = std::env::var("ORCHESTRATOR_NODE_ENDPOINT")
            .ok()
            .map(|value| value.trim().trim_end_matches('/').to_string())
            .filter(|value| !value.is_empty())?;
        let mut dispatcher = Self::new(endpoint);
        if let Ok(token) = std::env::var("ORCHESTRATOR_NODE_TOKEN") {
            dispatcher = dispatcher.with_token(token);
        }
        Some(dispatcher)
    }
}

impl NodeServiceDispatcher for HttpNodeServiceDispatcher {
    fn dispatch_service(
        &self,
        request: &NodeServiceDispatchRequest,
    ) -> Result<NodeServiceDispatchResult> {
        if self.endpoint.trim().is_empty() {
            return Ok(NodeServiceDispatchResult {
                status: "planned".to_string(),
                message: "node endpoint is not configured".to_string(),
                endpoint: String::new(),
                accepted: false,
            });
        }
        let url = format!(
            "{}/api/node/services/install",
            self.endpoint.trim_end_matches('/')
        );
        let body = serde_json::json!({
            "operation_id": request.operation_id,
            "service": request.service,
            "release": request.release,
            "host_service": request.host_service,
            "endpoint": request.endpoint,
            "rendered_config": request.rendered_config,
            "package_load": request.package_load,
        });
        let agent: Agent = Agent::config_builder()
            .timeout_global(Some(self.timeout))
            .http_status_as_error(false)
            .max_redirects(0)
            .proxy(None)
            .build()
            .into();
        let mut builder = agent.post(&url).header("Content-Type", "application/json");
        if let Some(token) = self.token.as_ref() {
            builder = builder.header("Authorization", format!("Bearer {}", token.trim()));
        }
        let response = builder.send(serde_json::to_string(&body)?).map_err(|err| {
            OrchestratorError::Dependency(format!("node service dispatch request failed: {err}"))
        })?;
        let status = response.status().as_u16();
        if (200..=299).contains(&status) {
            Ok(NodeServiceDispatchResult {
                status: "dispatched".to_string(),
                message: format!("node-mode orchestrator accepted install request: http {status}"),
                endpoint: self.endpoint.clone(),
                accepted: true,
            })
        } else {
            Err(OrchestratorError::Dependency(format!(
                "node service dispatch failed: http {status}"
            )))
        }
    }
}

impl ConfiguredMigrationRunner {
    pub fn from_env() -> Self {
        let execution_enabled = std::env::var("ORCHESTRATOR_MIGRATION_EXECUTION")
            .ok()
            .is_some_and(|value| {
                matches!(
                    value.trim().to_ascii_lowercase().as_str(),
                    "1" | "true" | "yes" | "on"
                )
            });
        let runner = if execution_enabled {
            LocalSqlMigrationRunner::from_env()
        } else {
            None
        };
        Self {
            execution_enabled,
            runner,
        }
    }
}

impl MigrationRunner for ConfiguredMigrationRunner {
    fn execute_migrations(
        &self,
        request: &MigrationExecutionRequest,
    ) -> Result<MigrationExecutionResult> {
        if let Some(runner) = self.runner.as_ref() {
            return runner.execute_migrations(request);
        }
        Ok(MigrationExecutionResult {
            status: if request.migrations.is_empty() {
                "skipped".to_string()
            } else {
                "deferred".to_string()
            },
            message: if self.execution_enabled {
                "ORCHESTRATOR_MIGRATION_ROOT is not configured".to_string()
            } else {
                "ORCHESTRATOR_MIGRATION_EXECUTION is not enabled".to_string()
            },
            runner: "configured-deferred".to_string(),
            dry_run: request.dry_run,
            executed: request
                .migrations
                .iter()
                .map(|migration| MigrationExecutionRecord {
                    migration_version: migration.version.clone(),
                    path: migration.path.clone(),
                    checksum: migration.checksum.clone(),
                    status: "registered".to_string(),
                    applied_at: String::new(),
                    message: "migration execution deferred".to_string(),
                })
                .collect(),
        })
    }
}

impl LocalSqlMigrationRunner {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            database_url: None,
            service_database_urls: BTreeMap::new(),
            dry_run: false,
            allow_destructive: false,
        }
    }

    pub fn with_database_url(mut self, database_url: impl Into<String>) -> Self {
        let database_url = database_url.into();
        self.database_url = (!database_url.trim().is_empty()).then_some(database_url);
        self
    }

    pub fn with_service_database_url(
        mut self,
        service_name: impl Into<String>,
        database_url: impl Into<String>,
    ) -> Self {
        let service_name = service_name.into();
        let database_url = database_url.into();
        if !service_name.trim().is_empty() && !database_url.trim().is_empty() {
            self.service_database_urls.insert(
                service_name.trim().to_string(),
                database_url.trim().to_string(),
            );
        }
        self
    }

    pub fn with_dry_run(mut self, dry_run: bool) -> Self {
        self.dry_run = dry_run;
        self
    }

    pub fn with_allow_destructive(mut self, allow_destructive: bool) -> Self {
        self.allow_destructive = allow_destructive;
        self
    }

    pub fn from_env() -> Option<Self> {
        let root = std::env::var("ORCHESTRATOR_MIGRATION_ROOT")
            .ok()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())?;
        let dry_run = env_flag("ORCHESTRATOR_MIGRATION_DRY_RUN");
        let allow_destructive = env_flag("ORCHESTRATOR_MIGRATION_ALLOW_DESTRUCTIVE");
        let runner = Self::new(root)
            .with_dry_run(dry_run)
            .with_allow_destructive(allow_destructive);
        Some(runner)
    }

    pub fn database_url_for_service(&self, service_name: &str) -> Option<String> {
        if let Some(database_url) = self.service_database_urls.get(service_name.trim()) {
            return Some(database_url.clone());
        }
        for env_name in service_database_url_env_candidates(service_name) {
            if let Ok(database_url) = std::env::var(&env_name) {
                let database_url = database_url.trim();
                if !database_url.is_empty() {
                    return Some(database_url.to_string());
                }
            }
        }
        self.database_url.clone()
    }
}

impl MigrationRunner for LocalSqlMigrationRunner {
    fn execute_migrations(
        &self,
        request: &MigrationExecutionRequest,
    ) -> Result<MigrationExecutionResult> {
        if request.migrations.is_empty() {
            return Ok(MigrationExecutionResult {
                status: "skipped".to_string(),
                message: "release declares no migrations".to_string(),
                runner: "local-sql-file".to_string(),
                dry_run: self.dry_run || request.dry_run,
                executed: Vec::new(),
            });
        }
        let dry_run = self.dry_run || request.dry_run;
        let allow_destructive = self.allow_destructive || request.allow_destructive;
        let mut client = if dry_run {
            None
        } else {
            let database_url = self
                .database_url_for_service(&request.service_name)
                .ok_or_else(|| {
                    OrchestratorError::Blocked(
                        format!(
                            "migration apply for {} requires a service-owned database URL",
                            request.service_name
                        )
                        .to_string(),
                    )
                })?;
            Some(
                postgres::Client::connect(&database_url, NoTls).map_err(|err| {
                    OrchestratorError::Dependency(format!(
                        "connect migration database failed: {err}"
                    ))
                })?,
            )
        };
        let mut executed = Vec::new();
        for migration in &request.migrations {
            if migration.destructive && !allow_destructive {
                return Err(OrchestratorError::Blocked(format!(
                    "destructive migration {} requires ORCHESTRATOR_MIGRATION_ALLOW_DESTRUCTIVE or allow_destructive=true",
                    migration.version
                )));
            }
            let path = safe_child_path(&self.root, &migration.path)?;
            let sql = fs::read_to_string(&path).map_err(|err| {
                OrchestratorError::Dependency(format!(
                    "read migration {} failed: {err}",
                    migration.path
                ))
            })?;
            validate_migration_checksum(migration, sql.as_bytes())?;
            if let Some(client) = client.as_mut() {
                client.batch_execute(&sql).map_err(|err| {
                    OrchestratorError::Dependency(format!(
                        "apply migration {} failed: {err}",
                        migration.version
                    ))
                })?;
            }
            executed.push(MigrationExecutionRecord {
                migration_version: migration.version.clone(),
                path: migration.path.clone(),
                checksum: migration.checksum.clone(),
                status: if dry_run {
                    "dry-run".to_string()
                } else {
                    "applied".to_string()
                },
                applied_at: if dry_run {
                    String::new()
                } else {
                    "applied".to_string()
                },
                message: if dry_run {
                    format!("validated {} bytes without apply", sql.len())
                } else {
                    format!("executed {} bytes against migration database", sql.len())
                },
            });
        }
        Ok(MigrationExecutionResult {
            status: if dry_run {
                "dry-run".to_string()
            } else {
                "applied".to_string()
            },
            message: format!(
                "{} {} migrations for {}",
                if dry_run { "validated" } else { "applied" },
                executed.len(),
                request.service_name
            ),
            runner: "local-sql-file".to_string(),
            dry_run,
            executed,
        })
    }
}

impl<'a, S: OrchestratorStore>
    OperationExecutor<
        'a,
        S,
        StaticEndpointProbe,
        DeferredAuthPermissionRegistrar,
        DeferredRedisResourceProvisioner,
        DeferredStorageResourceProvisioner,
        DeferredMigrationRunner,
        DeferredReleasePackageLoader,
        DeferredGatewayRoutePublisher,
    >
{
    pub fn new(store: &'a mut S) -> Self {
        Self {
            store,
            endpoint_probe: StaticEndpointProbe,
            auth_permission_registrar: DeferredAuthPermissionRegistrar,
            redis_resource_provisioner: DeferredRedisResourceProvisioner,
            storage_resource_provisioner: DeferredStorageResourceProvisioner,
            migration_runner: DeferredMigrationRunner,
            release_package_loader: DeferredReleasePackageLoader,
            gateway_route_publisher: DeferredGatewayRoutePublisher,
            node_service_dispatcher: DeferredNodeServiceDispatcher,
            service_driver_execution_enabled: false,
        }
    }
}

impl<'a, S: OrchestratorStore, P: EndpointProbe>
    OperationExecutor<
        'a,
        S,
        P,
        DeferredAuthPermissionRegistrar,
        DeferredRedisResourceProvisioner,
        DeferredStorageResourceProvisioner,
        DeferredMigrationRunner,
        DeferredReleasePackageLoader,
        DeferredGatewayRoutePublisher,
    >
{
    pub fn with_endpoint_probe(store: &'a mut S, endpoint_probe: P) -> Self {
        Self {
            store,
            endpoint_probe,
            auth_permission_registrar: DeferredAuthPermissionRegistrar,
            redis_resource_provisioner: DeferredRedisResourceProvisioner,
            storage_resource_provisioner: DeferredStorageResourceProvisioner,
            migration_runner: DeferredMigrationRunner,
            release_package_loader: DeferredReleasePackageLoader,
            gateway_route_publisher: DeferredGatewayRoutePublisher,
            node_service_dispatcher: DeferredNodeServiceDispatcher,
            service_driver_execution_enabled: false,
        }
    }
}

impl<'a, S: OrchestratorStore, P: EndpointProbe, A: AuthPermissionRegistrar>
    OperationExecutor<
        'a,
        S,
        P,
        A,
        DeferredRedisResourceProvisioner,
        DeferredStorageResourceProvisioner,
        DeferredMigrationRunner,
        DeferredReleasePackageLoader,
        DeferredGatewayRoutePublisher,
    >
{
    pub fn with_endpoint_probe_and_auth_registrar(
        store: &'a mut S,
        endpoint_probe: P,
        auth_permission_registrar: A,
    ) -> Self {
        Self {
            store,
            endpoint_probe,
            auth_permission_registrar,
            redis_resource_provisioner: DeferredRedisResourceProvisioner,
            storage_resource_provisioner: DeferredStorageResourceProvisioner,
            migration_runner: DeferredMigrationRunner,
            release_package_loader: DeferredReleasePackageLoader,
            gateway_route_publisher: DeferredGatewayRoutePublisher,
            node_service_dispatcher: DeferredNodeServiceDispatcher,
            service_driver_execution_enabled: false,
        }
    }
}

impl<
    'a,
    S: OrchestratorStore,
    P: EndpointProbe,
    A: AuthPermissionRegistrar,
    R: RedisResourceProvisioner,
    T: StorageResourceProvisioner,
    M: MigrationRunner,
>
    OperationExecutor<
        'a,
        S,
        P,
        A,
        R,
        T,
        M,
        ConfiguredReleasePackageLoader,
        ConfiguredGatewayRoutePublisher,
        ConfiguredNodeServiceDispatcher,
    >
{
    pub fn with_runtime_provisioners(
        store: &'a mut S,
        endpoint_probe: P,
        auth_permission_registrar: A,
        redis_resource_provisioner: R,
        storage_resource_provisioner: T,
        migration_runner: M,
    ) -> Self {
        Self {
            store,
            endpoint_probe,
            auth_permission_registrar,
            redis_resource_provisioner,
            storage_resource_provisioner,
            migration_runner,
            release_package_loader: ConfiguredReleasePackageLoader::from_env(),
            gateway_route_publisher: ConfiguredGatewayRoutePublisher::from_env(),
            node_service_dispatcher: ConfiguredNodeServiceDispatcher::from_env(),
            service_driver_execution_enabled: false,
        }
    }
}

impl<
    'a,
    S: OrchestratorStore,
    P: EndpointProbe,
    A: AuthPermissionRegistrar,
    R: RedisResourceProvisioner,
    T: StorageResourceProvisioner,
    M: MigrationRunner,
    L: ReleasePackageLoader,
>
    OperationExecutor<
        'a,
        S,
        P,
        A,
        R,
        T,
        M,
        L,
        DeferredGatewayRoutePublisher,
        DeferredNodeServiceDispatcher,
    >
{
    pub fn with_runtime_provisioners_and_release_loader(
        store: &'a mut S,
        endpoint_probe: P,
        auth_permission_registrar: A,
        redis_resource_provisioner: R,
        storage_resource_provisioner: T,
        migration_runner: M,
        release_package_loader: L,
    ) -> Self {
        Self {
            store,
            endpoint_probe,
            auth_permission_registrar,
            redis_resource_provisioner,
            storage_resource_provisioner,
            migration_runner,
            release_package_loader,
            gateway_route_publisher: DeferredGatewayRoutePublisher,
            node_service_dispatcher: DeferredNodeServiceDispatcher,
            service_driver_execution_enabled: false,
        }
    }
}

impl<
    'a,
    S: OrchestratorStore,
    P: EndpointProbe,
    A: AuthPermissionRegistrar,
    R: RedisResourceProvisioner,
    T: StorageResourceProvisioner,
    M: MigrationRunner,
    L: ReleasePackageLoader,
    N: NodeServiceDispatcher,
> OperationExecutor<'a, S, P, A, R, T, M, L, DeferredGatewayRoutePublisher, N>
{
    pub fn with_runtime_provisioners_release_loader_and_node_dispatcher(
        store: &'a mut S,
        endpoint_probe: P,
        auth_permission_registrar: A,
        redis_resource_provisioner: R,
        storage_resource_provisioner: T,
        migration_runner: M,
        release_package_loader: L,
        node_service_dispatcher: N,
    ) -> Self {
        Self {
            store,
            endpoint_probe,
            auth_permission_registrar,
            redis_resource_provisioner,
            storage_resource_provisioner,
            migration_runner,
            release_package_loader,
            gateway_route_publisher: DeferredGatewayRoutePublisher,
            node_service_dispatcher,
            service_driver_execution_enabled: false,
        }
    }
}

impl<
    'a,
    S: OrchestratorStore,
    P: EndpointProbe,
    A: AuthPermissionRegistrar,
    R: RedisResourceProvisioner,
    T: StorageResourceProvisioner,
    M: MigrationRunner,
    L: ReleasePackageLoader,
    G: GatewayRoutePublisher,
    N: NodeServiceDispatcher,
> OperationExecutor<'a, S, P, A, R, T, M, L, G, N>
{
    pub fn with_runtime_provisioners_release_loader_gateway_publisher_and_node_dispatcher(
        store: &'a mut S,
        endpoint_probe: P,
        auth_permission_registrar: A,
        redis_resource_provisioner: R,
        storage_resource_provisioner: T,
        migration_runner: M,
        release_package_loader: L,
        gateway_route_publisher: G,
        node_service_dispatcher: N,
    ) -> Self {
        Self {
            store,
            endpoint_probe,
            auth_permission_registrar,
            redis_resource_provisioner,
            storage_resource_provisioner,
            migration_runner,
            release_package_loader,
            gateway_route_publisher,
            node_service_dispatcher,
            service_driver_execution_enabled: false,
        }
    }

    pub fn with_service_driver_execution_enabled(mut self) -> Self {
        self.service_driver_execution_enabled = true;
        self
    }

    fn load_release_package(
        &mut self,
        operation_id: &str,
        release: &ServiceReleaseManifest,
    ) -> Result<ReleasePackageLoadResult> {
        let request = ReleasePackageLoadRequest {
            service_name: release.service_name.clone(),
            version: release.version.clone(),
            source_url: release.source.url.clone(),
            expected_manifest: Some(release.clone()),
        };
        let result = self.release_package_loader.load_release_package(&request)?;
        self.store.append_operation_log(release_package_log_record(
            operation_id,
            &release.service_name,
            &result,
        ))?;
        Ok(result)
    }

    fn register_release_permissions(
        &mut self,
        operation_id: &str,
        release: &ServiceReleaseManifest,
    ) -> Result<AuthPermissionRegistrationResult> {
        let service_identity = service_identity_registration_from_release(&*self.store, release)?;
        if release.permissions.is_empty() && service_identity.is_none() {
            let result = AuthPermissionRegistrationResult {
                status: "skipped".to_string(),
                message: "release declares no permissions or service identity".to_string(),
                endpoint: String::new(),
                registered: 0,
            };
            self.store.append_operation_log(auth_permission_log_record(
                operation_id,
                &release.service_name,
                &result,
            ))?;
            return Ok(result);
        }

        let request = AuthPermissionRegistration {
            service_name: release.service_name.clone(),
            permissions: release.permissions.clone(),
            service_identity,
        };
        let result = self
            .auth_permission_registrar
            .register_permissions(&request)?;
        self.store.append_operation_log(auth_permission_log_record(
            operation_id,
            &release.service_name,
            &result,
        ))?;
        Ok(result)
    }

    fn publish_gateway_routes(
        &mut self,
        operation_id: &str,
        release: &ServiceReleaseManifest,
        routes: &[ServiceRoute],
        effective_routes: &[EffectiveApiRoute],
        node_id: &str,
        api_count: usize,
    ) -> Result<GatewayRoutePublishResult> {
        let request = GatewayRoutePublishRequest {
            operation_id: operation_id.to_string(),
            service_name: release.service_name.clone(),
            routes: routes.to_vec(),
            effective_routes: effective_routes.to_vec(),
            node_id: node_id.to_string(),
            api_count,
            force_reload: api_count > 0 || !routes.is_empty() || !effective_routes.is_empty(),
        };
        let result = self.gateway_route_publisher.publish_routes(&request)?;
        self.store.append_operation_log(gateway_route_log_record(
            operation_id,
            &release.service_name,
            api_count,
            request.force_reload,
            &result,
        ))?;
        Ok(result)
    }

    fn provision_release_redis_resources(
        &mut self,
        operation_id: &str,
        release: &ServiceReleaseManifest,
        resources: &[ServiceRedisResource],
    ) -> Result<RedisProvisionResult> {
        if resources.is_empty() {
            let result = RedisProvisionResult {
                status: "skipped".to_string(),
                message: "release declares no redis resources".to_string(),
                endpoint: String::new(),
                provisioned: Vec::new(),
            };
            self.store.append_operation_log(redis_provision_log_record(
                operation_id,
                &release.service_name,
                &result,
            ))?;
            return Ok(result);
        }

        let request = RedisProvisionRequest {
            service_name: release.service_name.clone(),
            resources: resources.to_vec(),
        };
        let result = self
            .redis_resource_provisioner
            .provision_resources(&request)?;
        self.store.append_operation_log(redis_provision_log_record(
            operation_id,
            &release.service_name,
            &result,
        ))?;
        Ok(result)
    }

    fn provision_release_storage_resources(
        &mut self,
        operation_id: &str,
        release: &ServiceReleaseManifest,
        resources: &[ServiceStorageResource],
    ) -> Result<StorageProvisionResult> {
        if resources.is_empty() {
            let result = StorageProvisionResult {
                status: "skipped".to_string(),
                message: "release declares no storage resources".to_string(),
                endpoint: String::new(),
                provisioned: Vec::new(),
            };
            self.store
                .append_operation_log(storage_provision_log_record(
                    operation_id,
                    &release.service_name,
                    &result,
                ))?;
            return Ok(result);
        }

        let request = StorageProvisionRequest {
            service_name: release.service_name.clone(),
            resources: resources.to_vec(),
        };
        let result = self
            .storage_resource_provisioner
            .provision_resources(&request)?;
        self.store
            .append_operation_log(storage_provision_log_record(
                operation_id,
                &release.service_name,
                &result,
            ))?;
        Ok(result)
    }

    fn dispatch_release_to_node(
        &mut self,
        operation: &Operation,
        service: &ServiceManifest,
        release: Option<&ServiceReleaseManifest>,
        host_service: &HostService,
        endpoint: &Endpoint,
        rendered_config: serde_json::Value,
        package_load: Option<&ReleasePackageLoadResult>,
    ) -> Result<NodeServiceDispatchResult> {
        let request = NodeServiceDispatchRequest {
            operation_id: operation.operation_id.clone(),
            service: service.clone(),
            release: release.cloned(),
            host_service: host_service.clone(),
            endpoint: endpoint.clone(),
            rendered_config,
            package_load: package_load.cloned(),
        };
        let result = self.node_service_dispatcher.dispatch_service(&request)?;
        self.store.append_operation_log(node_dispatch_log_record(
            &operation.operation_id,
            &service.id,
            &result,
        ))?;
        ensure_node_dispatch_accepted(&result)?;
        Ok(result)
    }

    fn stop_local_process_release(
        &mut self,
        operation_id: &str,
        service: &ServiceManifest,
        release: &ServiceReleaseManifest,
        endpoint: String,
    ) -> Result<DriverResult> {
        if !release
            .runtime
            .kind
            .trim()
            .eq_ignore_ascii_case("local-process")
        {
            return Ok(DriverResult {
                action: "service.stop".to_string(),
                status: "SUPPORTED".to_string(),
                message: format!(
                    "release runtime {} does not require local-process stop",
                    release.runtime.kind
                ),
                command: Vec::new(),
                pid: None,
                pid_file: String::new(),
            });
        }
        let request = DriverRequest {
            action: "service.stop".to_string(),
            service_id: service.id.clone(),
            endpoint,
            link: None,
            log_source: None,
            release_runtime: Some(release.runtime.clone()),
        };
        let result = LocalProcessDriver::new().execute(&request)?;
        self.store
            .append_operation_log(driver_result_log_record(operation_id, &result))?;
        ensure_driver_result_succeeded(&result)?;
        Ok(result)
    }

    fn stop_installed_local_process_release(
        &mut self,
        operation: &Operation,
        service_name: &str,
    ) -> Result<Option<DriverResult>> {
        let Some(service) = self.store.get_service(service_name)? else {
            return Ok(None);
        };
        let endpoint = operation
            .request
            .get("endpoint")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string)
            .or_else(|| {
                self.store
                    .list_endpoints()
                    .ok()
                    .and_then(|endpoints| {
                        endpoints
                            .into_iter()
                            .find(|endpoint| endpoint.service_id == service_name)
                    })
                    .map(|endpoint| endpoint.endpoint)
            })
            .unwrap_or_default();
        let release = release_manifest_from_operation(operation)?.or_else(|| {
            self.store
                .list_service_releases()
                .ok()
                .and_then(|releases| {
                    releases
                        .into_iter()
                        .find(|release| release.service_name == service_name)
                })
                .and_then(|release| serde_json::from_value(release.manifest).ok())
        });
        let Some(release) = release else {
            return Ok(None);
        };
        if !release
            .runtime
            .kind
            .trim()
            .eq_ignore_ascii_case("local-process")
        {
            return Ok(None);
        }
        self.stop_local_process_release(&operation.operation_id, &service, &release, endpoint)
            .map(Some)
    }

    fn execute_release_migrations(
        &mut self,
        operation: &Operation,
        release: &ServiceReleaseManifest,
    ) -> Result<MigrationExecutionResult> {
        let dry_run = operation
            .request
            .get("migration_dry_run")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false)
            || operation
                .request
                .get("migration_dry_run")
                .and_then(serde_json::Value::as_str)
                .is_some_and(|value| truthy(value));
        let allow_destructive = operation
            .request
            .get("allow_destructive_migrations")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false)
            || operation
                .request
                .get("allow_destructive_migrations")
                .and_then(serde_json::Value::as_str)
                .is_some_and(|value| truthy(value));
        let plan = self.pending_release_migrations(release)?;
        let request = MigrationExecutionRequest {
            service_name: release.service_name.clone(),
            migrations: plan.pending.clone(),
            release_source_url: release.source.url.clone(),
            dry_run,
            allow_destructive,
        };
        if release.migrations.is_empty() {
            let result = MigrationExecutionResult {
                status: "skipped".to_string(),
                message: "release declares no migrations".to_string(),
                runner: "none".to_string(),
                dry_run,
                executed: Vec::new(),
            };
            self.store
                .append_operation_log(migration_execution_log_record(
                    &operation.operation_id,
                    &release.service_name,
                    &result,
                ))?;
            return Ok(result);
        }
        if plan.pending.is_empty() {
            let result = MigrationExecutionResult {
                status: "skipped".to_string(),
                message: format!(
                    "all {} migrations for {} are already applied",
                    release.migrations.len(),
                    release.service_name
                ),
                runner: "already-applied".to_string(),
                dry_run,
                executed: plan.already_applied,
            };
            self.persist_migration_execution_result(release, &result)?;
            self.store
                .append_operation_log(migration_execution_log_record(
                    &operation.operation_id,
                    &release.service_name,
                    &result,
                ))?;
            return Ok(result);
        }

        let result = match self.migration_runner.execute_migrations(&request) {
            Ok(mut result) => {
                if !plan.already_applied.is_empty() {
                    let skipped = plan.already_applied.len();
                    result.message = format!(
                        "{}; skipped {skipped} already applied migrations",
                        result.message
                    );
                    result.executed.extend(plan.already_applied);
                }
                result
            }
            Err(err) => {
                let mut executed = request
                    .migrations
                    .iter()
                    .map(|migration| MigrationExecutionRecord {
                        migration_version: migration.version.clone(),
                        path: migration.path.clone(),
                        checksum: migration.checksum.clone(),
                        status: "failed".to_string(),
                        applied_at: String::new(),
                        message: err.to_string(),
                    })
                    .collect::<Vec<_>>();
                executed.extend(plan.already_applied);
                let result = MigrationExecutionResult {
                    status: "failed".to_string(),
                    message: err.to_string(),
                    runner: "failed-before-completion".to_string(),
                    dry_run,
                    executed,
                };
                self.persist_migration_execution_result(release, &result)?;
                self.store
                    .append_operation_log(migration_execution_log_record(
                        &operation.operation_id,
                        &release.service_name,
                        &result,
                    ))?;
                return Err(err);
            }
        };
        self.persist_migration_execution_result(release, &result)?;
        self.store
            .append_operation_log(migration_execution_log_record(
                &operation.operation_id,
                &release.service_name,
                &result,
            ))?;
        Ok(result)
    }

    fn pending_release_migrations(
        &self,
        release: &ServiceReleaseManifest,
    ) -> Result<PendingMigrationPlan> {
        let existing = self
            .store
            .list_service_migration_records()?
            .into_iter()
            .filter(|record| record.service_name == release.service_name)
            .map(|record| (record.migration_version.clone(), record))
            .collect::<BTreeMap<_, _>>();
        let mut plan = PendingMigrationPlan::default();
        for migration in &release.migrations {
            if let Some(record) = existing.get(&migration.version) {
                if record.status == "applied" {
                    if record.checksum != migration.checksum {
                        return Err(OrchestratorError::Dependency(format!(
                            "migration {}@{} was already applied with checksum {}, release declares {}",
                            release.service_name,
                            migration.version,
                            empty_checksum_label(&record.checksum),
                            empty_checksum_label(&migration.checksum)
                        )));
                    }
                    plan.already_applied.push(MigrationExecutionRecord {
                        migration_version: migration.version.clone(),
                        path: migration.path.clone(),
                        checksum: migration.checksum.clone(),
                        status: "applied".to_string(),
                        applied_at: record.applied_at.clone(),
                        message: "already applied; skipped for this install".to_string(),
                    });
                    continue;
                }
            }
            plan.pending.push(migration.clone());
        }
        Ok(plan)
    }

    fn persist_migration_execution_result(
        &mut self,
        release: &ServiceReleaseManifest,
        result: &MigrationExecutionResult,
    ) -> Result<()> {
        let executed_by_version = result
            .executed
            .iter()
            .map(|record| (record.migration_version.as_str(), record))
            .collect::<BTreeMap<_, _>>();
        for migration in &release.migrations {
            let executed = executed_by_version.get(migration.version.as_str());
            let status = executed.map(|record| record.status.as_str()).unwrap_or(
                match result.status.as_str() {
                    "applied" => "applied",
                    "dry-run" => "dry-run",
                    "failed" => "failed",
                    "skipped" => "skipped",
                    "deferred" => "registered",
                    _ => "registered",
                },
            );
            self.store
                .upsert_service_migration_record(ServiceMigrationRecord {
                    service_name: release.service_name.clone(),
                    migration_version: migration.version.clone(),
                    checksum: migration.checksum.clone(),
                    status: status.to_string(),
                    applied_at: executed
                        .map(|record| record.applied_at.clone())
                        .unwrap_or_default(),
                    created_at: String::new(),
                    updated_at: String::new(),
                })?;
        }
        Ok(())
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
                let mut result = serde_json::json!({
                    "operation_id": running.operation_id,
                    "status": "SUCCEEDED",
                    "started_at": running.started_at,
                    "finished_at": "finished",
                    "changed_objects": changed_objects,
                    "topology_snapshot_id": serde_json::Value::Null,
                });
                if let Some(runtime_pipeline) =
                    runtime_pipeline_result_from_logs(self.store, &running)?
                {
                    result
                        .as_object_mut()
                        .expect("operation result is an object")
                        .insert("runtime_pipeline".to_string(), runtime_pipeline);
                }
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
                let host_ip = operation
                    .request
                    .get("host_ip")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("127.0.0.1");
                let endpoint_id = operation
                    .request
                    .get("endpoint")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_string)
                    .unwrap_or_else(|| {
                        format!(
                            "{}:{}:{}",
                            host_ip, service.endpoint.default_port, service.id
                        )
                    });
                let external_service_running =
                    operation_bool_field(operation, "external_service_running")
                        || operation_bool_field(operation, "existing_endpoint_running");
                let _previous_state =
                    self.capture_release_install_previous_state(operation, &service.id)?;
                self.store.put_service(service.clone())?;
                for endpoint in self
                    .store
                    .list_endpoints()?
                    .into_iter()
                    .filter(|endpoint| endpoint.service_id == service.id)
                {
                    self.store.delete_endpoint(&endpoint.endpoint)?;
                }
                self.store.upsert_host_service(host_service_from_install(
                    &service,
                    release.as_ref(),
                    host_ip,
                    "installing",
                    serde_json::json!({}),
                    install_labels(release.as_ref()),
                )?)?;
                let endpoint = endpoint_from_install(&service, release.as_ref(), &endpoint_id)?;
                self.store.put_endpoint(endpoint.clone())?;
                let mut auth_permission_registration = None;
                let mut migration_execution = None;
                let mut redis_provision = None;
                let mut storage_provision = None;
                let mut gateway_route_publish = None;
                let mut release_routes = Vec::new();
                let mut api_surface_count = 0_usize;
                let release_package_load = match release.as_ref() {
                    Some(release) => {
                        Some(self.load_release_package(&operation.operation_id, release)?)
                    }
                    None => None,
                };
                if let Some(release) = release.as_ref() {
                    self.store
                        .upsert_service_release(service_release_record(release)?)?;
                    self.clear_release_resource_registries_for_install(&release.service_name)?;
                    let api_surfaces = service_api_surfaces_from_release(release)?;
                    for api in &api_surfaces {
                        self.store.upsert_service_api_surface(api.clone())?;
                    }
                    let routes = service_routes_from_release(release)?;
                    for route in &routes {
                        self.store.upsert_service_route(route.clone())?;
                    }
                    api_surface_count = api_surfaces.len();
                    release_routes = routes;
                    migration_execution =
                        Some(self.execute_release_migrations(operation, release)?);
                    for record in service_permission_records_from_release(release) {
                        self.store.upsert_service_permission_record(record)?;
                    }
                    auth_permission_registration =
                        Some(self.register_release_permissions(&operation.operation_id, release)?);
                    self.store.upsert_service_frontend_entry(
                        service_frontend_entry_from_release(release)?,
                    )?;
                    let redis_resources = service_redis_resources_from_release(release);
                    for resource in &redis_resources {
                        self.store.upsert_service_redis_resource(resource.clone())?;
                    }
                    redis_provision = Some(self.provision_release_redis_resources(
                        &operation.operation_id,
                        release,
                        &redis_resources,
                    )?);
                    let storage_resources = service_storage_resources_from_release(release);
                    for resource in &storage_resources {
                        self.store
                            .upsert_service_storage_resource(resource.clone())?;
                    }
                    storage_provision = Some(self.provision_release_storage_resources(
                        &operation.operation_id,
                        release,
                        &storage_resources,
                    )?);
                    self.store
                        .upsert_rendered_service_config(rendered_config_from_release(release)?)?;
                }
                let dispatch_config =
                    rendered_runtime_config(&service, release.as_ref(), &endpoint_id, None, None);
                let dispatch_host_service = host_service_from_install(
                    &service,
                    release.as_ref(),
                    host_ip,
                    "dispatching",
                    dispatch_config.clone(),
                    install_labels(release.as_ref()),
                )?;
                let node_dispatch = Some(self.dispatch_release_to_node(
                    operation,
                    &service,
                    release.as_ref(),
                    &dispatch_host_service,
                    &endpoint,
                    dispatch_config,
                    release_package_load.as_ref(),
                )?);
                let driver_result = execute_service_driver_action(
                    &service,
                    operation,
                    self.service_driver_execution_enabled,
                )?;
                self.store.append_operation_log(driver_result_log_record(
                    &operation.operation_id,
                    &driver_result,
                ))?;
                if self.service_driver_execution_enabled {
                    ensure_driver_result_succeeded(&driver_result)?;
                }
                let health = if external_service_running {
                    external_running_install_health(&endpoint)
                } else if driver_result.status == "SUCCEEDED" {
                    self.wait_endpoint_health_and_persist(&operation.operation_id, &endpoint)?
                } else {
                    deferred_install_health(&endpoint, &driver_result)
                };
                if external_service_running {
                    self.store.update_endpoint_health(
                        &endpoint.endpoint,
                        health.health.clone(),
                        health.reachable,
                    )?;
                    self.store.append_operation_log(endpoint_health_log_record(
                        &operation.operation_id,
                        &health,
                    ))?;
                }
                if self.service_driver_execution_enabled
                    && !external_service_running
                    && driver_result.status == "SUCCEEDED"
                    && !health.reachable
                {
                    if let Some(release) = release.as_ref() {
                        let _ = self.stop_local_process_release(
                            &operation.operation_id,
                            &service,
                            release,
                            endpoint.endpoint.clone(),
                        );
                    }
                    return Err(OrchestratorError::Dependency(format!(
                        "service_start health failed for {}: {}",
                        endpoint.endpoint, health.message
                    )));
                }
                let final_status = if external_service_running {
                    "running"
                } else {
                    release_install_host_status(node_dispatch.as_ref(), &driver_result, &health)
                };
                self.store.upsert_host_service(host_service_from_install(
                    &service,
                    release.as_ref(),
                    host_ip,
                    final_status,
                    rendered_runtime_config(
                        &service,
                        release.as_ref(),
                        &endpoint_id,
                        node_dispatch.as_ref(),
                        Some(&driver_result),
                    ),
                    install_labels(release.as_ref()),
                )?)?;
                if let Some(release) = release.as_ref() {
                    self.ensure_node_for_host(host_ip)?;
                    for api in deployed_service_apis_from_release(
                        release,
                        host_ip,
                        &endpoint_id,
                        final_status,
                    )? {
                        self.store.upsert_deployed_service_api(api)?;
                    }
                    let gateway_node_id = gateway_reload_node_id(operation);
                    let effective_routes = if let Some(node_id) = gateway_node_id.as_deref() {
                        self.store.effective_api_routes(node_id)?
                    } else {
                        Vec::new()
                    };
                    gateway_route_publish = Some(self.publish_gateway_routes(
                        &operation.operation_id,
                        release,
                        &release_routes,
                        &effective_routes,
                        gateway_node_id.as_deref().unwrap_or(""),
                        api_surface_count,
                    )?);
                }
                changed.extend(release_changed_objects(&service, release.as_ref()));
                if let Some(release) = release.as_ref() {
                    self.store.append_operation_log(release_install_log_record(
                        &operation.operation_id,
                        release,
                    ))?;
                }
                self.store
                    .append_operation_log(release_install_pipeline_log_record(
                        &operation.operation_id,
                        &service,
                        release.as_ref(),
                        host_ip,
                        &endpoint_id,
                        &health,
                        migration_execution.as_ref(),
                        auth_permission_registration.as_ref(),
                        redis_provision.as_ref(),
                        storage_provision.as_ref(),
                        release_package_load.as_ref(),
                        gateway_route_publish.as_ref(),
                        node_dispatch.as_ref(),
                        &driver_result,
                    ))?;
                changed.push(changed_object(
                    "HostService",
                    &format!("{}:{}", host_ip, service.id),
                ));
                changed.push(changed_object("Endpoint", &endpoint_id));
                changed.push(changed_object("Service", &service.id));
            }
            "release.delete" => {
                let service_name = operation
                    .request
                    .get("service_id")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or(operation.target_id.as_str());
                let _ = self.stop_installed_local_process_release(operation, service_name)?;
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
                let _ =
                    self.stop_installed_local_process_release(operation, &operation.target_id)?;
                self.append_migration_rollback_unsupported_log(operation)?;
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
            host_services: self
                .store
                .list_host_services()?
                .into_iter()
                .filter(|host_service| host_service.service_name == service_name)
                .collect(),
            endpoints: self
                .store
                .list_endpoints()?
                .into_iter()
                .filter(|endpoint| endpoint.service_id == service_name)
                .collect(),
            links: self
                .store
                .list_links()?
                .into_iter()
                .filter(|link| {
                    parse_endpoint_id(&link.source_endpoint)
                        .is_ok_and(|identity| identity.service_name == service_name)
                        || parse_endpoint_id(&link.target_endpoint)
                            .is_ok_and(|identity| identity.service_name == service_name)
                })
                .collect(),
            log_views: self
                .store
                .list_log_sources()?
                .into_iter()
                .filter(|log_view| log_view.service_id == service_name)
                .collect(),
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
            api_surfaces: self
                .store
                .list_service_api_surfaces()?
                .into_iter()
                .filter(|api| api.service_name == service_name)
                .collect(),
            deployed_apis: self
                .store
                .list_deployed_service_apis()?
                .into_iter()
                .filter(|api| api.service_name == service_name)
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
            self.store.delete_host_services_for_service(service_name)?;
            for endpoint in self
                .store
                .list_endpoints()?
                .into_iter()
                .filter(|endpoint| endpoint.service_id == service_name)
            {
                self.store.delete_endpoint(&endpoint.endpoint)?;
                changed.push(changed_object("Endpoint", &endpoint.endpoint));
            }
            for link in self.store.list_links()?.into_iter().filter(|link| {
                parse_endpoint_id(&link.source_endpoint)
                    .is_ok_and(|identity| identity.service_name == service_name)
                    || parse_endpoint_id(&link.target_endpoint)
                        .is_ok_and(|identity| identity.service_name == service_name)
            }) {
                self.store
                    .delete_link(&link.source_endpoint, &link.target_endpoint)?;
                changed.push(changed_object("Link", &link_target_id(&link)));
            }
            for log_view in self
                .store
                .list_log_sources()?
                .into_iter()
                .filter(|log_view| log_view.service_id == service_name)
            {
                self.store.delete_log_source(&log_view.source_id)?;
                changed.push(changed_object("LogView", &log_view.source_id));
            }
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
            self.clear_release_resource_registries(service_name)?;
            self.store.delete_service(service_name)?;
            changed.push(changed_object("Service", service_name));
            return Ok(changed);
        };

        self.store.delete_host_services_for_service(service_name)?;
        for endpoint in self
            .store
            .list_endpoints()?
            .into_iter()
            .filter(|endpoint| endpoint.service_id == service_name)
        {
            self.store.delete_endpoint(&endpoint.endpoint)?;
        }
        self.store.put_service(service.clone())?;
        changed.push(changed_object("Service", &service.id));
        for host_service in &previous_state.host_services {
            self.store.upsert_host_service(host_service.clone())?;
            changed.push(changed_object(
                "HostService",
                &format!("{}:{}", host_service.host_ip, host_service.service_name),
            ));
        }
        for endpoint in &previous_state.endpoints {
            self.store.put_endpoint(endpoint.clone())?;
            changed.push(changed_object("Endpoint", &endpoint.endpoint));
        }
        for link in &previous_state.links {
            if self.store.get_endpoint(&link.source_endpoint)?.is_some()
                && self.store.get_endpoint(&link.target_endpoint)?.is_some()
            {
                self.store.put_link(link.clone())?;
                changed.push(changed_object("Link", &link_target_id(link)));
            }
        }
        for log_view in &previous_state.log_views {
            if self.store.get_endpoint(&log_view.endpoint)?.is_some() {
                self.store.put_log_view(log_view.clone())?;
                changed.push(changed_object("LogView", &log_view.source_id));
            }
        }
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
        for api in &previous_state.api_surfaces {
            self.store.upsert_service_api_surface(api.clone())?;
            changed.push(changed_object("ServiceApiSurface", &api_surface_id(api)));
        }
        for api in &previous_state.deployed_apis {
            self.store.upsert_deployed_service_api(api.clone())?;
            changed.push(changed_object("DeployedServiceApi", &deployed_api_id(api)));
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
            api_surfaces: self
                .store
                .list_service_api_surfaces()?
                .into_iter()
                .filter(|api| api.service_name == service_name)
                .collect(),
            deployed_apis: self
                .store
                .list_deployed_service_apis()?
                .into_iter()
                .filter(|api| api.service_name == service_name)
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
        for api in &previous_state.api_surfaces {
            self.store.upsert_service_api_surface(api.clone())?;
            changed.push(changed_object("ServiceApiSurface", &api_surface_id(api)));
        }
        for api in &previous_state.deployed_apis {
            self.store.upsert_deployed_service_api(api.clone())?;
            changed.push(changed_object("DeployedServiceApi", &deployed_api_id(api)));
        }
        changed.push(changed_object("ReleaseRegistry", service_name));
        Ok(changed)
    }

    fn append_migration_rollback_unsupported_log(&mut self, operation: &Operation) -> Result<()> {
        let logs = self.store.list_operation_logs(&operation.operation_id)?;
        let applied = logs.iter().any(|log| {
            log.step_id.starts_with("migrations:")
                && log.data.get("status").and_then(serde_json::Value::as_str) == Some("applied")
        });
        if applied {
            self.store.append_operation_log(operation_step_log_record(
                &operation.operation_id,
                "migration-rollback:unsupported",
                "warn",
                "database migration rollback is unsupported; registry state will be restored only",
                serde_json::json!({
                    "status": "unsupported",
                    "scope": "database-migrations",
                    "registry_rollback": "restore_previous_state"
                }),
            ))?;
        }
        Ok(())
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

    fn wait_endpoint_health_and_persist(
        &mut self,
        operation_id: &str,
        endpoint: &Endpoint,
    ) -> Result<EndpointHealthResult> {
        let mut last = None;
        let attempts = env_usize("ORCHESTRATOR_ENDPOINT_HEALTH_ATTEMPTS").unwrap_or(25);
        let interval_ms = env_u64("ORCHESTRATOR_ENDPOINT_HEALTH_INTERVAL_MS").unwrap_or(200);
        let attempts = attempts.max(1);
        for attempt in 0..attempts {
            let health = check_endpoint_health_with_probe(endpoint, &self.endpoint_probe)?;
            if health.reachable {
                self.store.update_endpoint_health(
                    &health.endpoint,
                    health.health.clone(),
                    health.reachable,
                )?;
                self.store
                    .append_operation_log(endpoint_health_log_record(operation_id, &health))?;
                return Ok(health);
            }
            last = Some(health);
            if attempt + 1 < attempts {
                std::thread::sleep(Duration::from_millis(interval_ms));
            }
        }
        let health = last.ok_or_else(|| {
            OrchestratorError::Dependency("endpoint health wait produced no result".to_string())
        })?;
        self.store.update_endpoint_health(
            &health.endpoint,
            health.health.clone(),
            health.reachable,
        )?;
        self.store
            .append_operation_log(endpoint_health_log_record(operation_id, &health))?;
        Ok(health)
    }

    fn ensure_node_for_host(&mut self, host_ip: &str) -> Result<()> {
        if self
            .store
            .list_nodes()?
            .into_iter()
            .any(|node| node.host_ip == host_ip)
        {
            return Ok(());
        }
        let node_id = format!(
            "host-{}",
            host_ip
                .chars()
                .map(|ch| if ch.is_ascii_alphanumeric() { ch } else { '-' })
                .collect::<String>()
                .trim_matches('-')
        );
        self.store.upsert_node(NodeRecord {
            node_id,
            host_ip: host_ip.to_string(),
            parent_node_id: String::new(),
            role: "standalone".to_string(),
            labels: serde_json::json!({
                "source": "release.install"
            }),
            status: "running".to_string(),
            created_at: String::new(),
            updated_at: String::new(),
        })
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
        self.store
            .delete_service_api_surfaces_for_service(service_name)?;
        self.store
            .delete_deployed_service_apis_for_service(service_name)?;
        Ok(())
    }

    fn clear_release_resource_registries_for_install(&mut self, service_name: &str) -> Result<()> {
        self.store.delete_service_routes_for_service(service_name)?;
        self.store
            .delete_service_permission_records_for_service(service_name)?;
        self.store.delete_service_frontend_entry(service_name)?;
        self.store
            .delete_service_redis_resources_for_service(service_name)?;
        self.store
            .delete_service_storage_resources_for_service(service_name)?;
        self.store
            .delete_rendered_service_configs_for_service(service_name)?;
        self.store
            .delete_service_api_surfaces_for_service(service_name)?;
        self.store
            .delete_deployed_service_apis_for_service(service_name)?;
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

fn runtime_pipeline_result_from_logs<S: OrchestratorStore>(
    store: &S,
    operation: &Operation,
) -> Result<Option<serde_json::Value>> {
    if operation.action != "release.install" {
        return Ok(None);
    }
    Ok(store
        .list_operation_logs(&operation.operation_id)?
        .into_iter()
        .rev()
        .find(|log| log.step_id.starts_with("install-pipeline:"))
        .map(|log| log.data))
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

fn api_surface_id(api: &ServiceApiSurface) -> String {
    format!("{}@{}:{}", api.service_name, api.version, api.api_id)
}

fn deployed_api_id(api: &DeployedServiceApi) -> String {
    format!(
        "{}@{}:{}:{}",
        api.service_name, api.version, api.api_id, api.endpoint
    )
}

fn validate_node_tree_upsert<'a>(
    existing_nodes: impl Iterator<Item = &'a NodeRecord>,
    node: &NodeRecord,
) -> Result<()> {
    let mut nodes = existing_nodes
        .map(|item| (item.node_id.clone(), item.clone()))
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
    ensure_node_tree_acyclic(&nodes)
}

fn ensure_node_tree_acyclic(nodes: &BTreeMap<String, NodeRecord>) -> Result<()> {
    for node_id in nodes.keys() {
        let mut seen = BTreeSet::new();
        let mut current = node_id.as_str();
        loop {
            if !seen.insert(current.to_string()) {
                return Err(OrchestratorError::InvalidManifest(format!(
                    "node tree contains cycle at {current}"
                )));
            }
            let Some(node) = nodes.get(current) else {
                return Err(OrchestratorError::Dependency(format!(
                    "node {current} is missing during tree validation"
                )));
            };
            let parent = node.parent_node_id.trim();
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

fn ancestors_of_from_nodes(nodes: Vec<NodeRecord>, node_id: &str) -> Result<Vec<NodeRecord>> {
    let map = nodes
        .into_iter()
        .map(|node| (node.node_id.clone(), node))
        .collect::<BTreeMap<_, _>>();
    ensure_node_tree_acyclic(&map)?;
    let node = map
        .get(node_id)
        .ok_or_else(|| OrchestratorError::Dependency(format!("node {node_id} not found")))?;
    if node.role == "standalone" {
        return Ok(Vec::new());
    }
    let mut ancestors = Vec::new();
    let mut parent_id = node.parent_node_id.trim().to_string();
    while !parent_id.is_empty() {
        let parent = map.get(&parent_id).ok_or_else(|| {
            OrchestratorError::Dependency(format!("parent node {parent_id} not found"))
        })?;
        ancestors.push(parent.clone());
        parent_id = parent.parent_node_id.trim().to_string();
    }
    Ok(ancestors)
}

fn descendants_of_from_nodes(nodes: Vec<NodeRecord>, node_id: &str) -> Result<Vec<NodeRecord>> {
    let map = nodes
        .into_iter()
        .map(|node| (node.node_id.clone(), node))
        .collect::<BTreeMap<_, _>>();
    ensure_node_tree_acyclic(&map)?;
    if !map.contains_key(node_id) {
        return Err(OrchestratorError::Dependency(format!(
            "node {node_id} not found"
        )));
    }
    let mut descendants = Vec::new();
    let mut frontier = vec![node_id.to_string()];
    while let Some(parent_id) = frontier.pop() {
        let mut children = map
            .values()
            .filter(|node| node.parent_node_id == parent_id)
            .cloned()
            .collect::<Vec<_>>();
        children.sort_by(|left, right| left.node_id.cmp(&right.node_id));
        for child in children {
            frontier.push(child.node_id.clone());
            descendants.push(child);
        }
    }
    Ok(descendants)
}

fn effective_api_routes_from_registry(
    node_id: &str,
    nodes: Vec<NodeRecord>,
    surfaces: Vec<ServiceApiSurface>,
    deployed_apis: Vec<DeployedServiceApi>,
) -> Result<Vec<EffectiveApiRoute>> {
    let node_by_id = nodes
        .iter()
        .cloned()
        .map(|node| (node.node_id.clone(), node))
        .collect::<BTreeMap<_, _>>();
    ensure_node_tree_acyclic(&node_by_id)?;
    let target = node_by_id
        .get(node_id)
        .ok_or_else(|| OrchestratorError::Dependency(format!("node {node_id} not found")))?;
    let node_by_host = nodes
        .iter()
        .cloned()
        .map(|node| (node.host_ip.clone(), node))
        .collect::<BTreeMap<_, _>>();
    let surface_by_key = surfaces
        .into_iter()
        .map(|api| {
            (
                (
                    api.service_name.clone(),
                    api.version.clone(),
                    api.api_id.clone(),
                ),
                api,
            )
        })
        .collect::<BTreeMap<_, _>>();
    let ancestor_distances = ancestors_of_from_nodes(nodes, node_id)?
        .into_iter()
        .enumerate()
        .map(|(index, ancestor)| (ancestor.node_id, (index + 1) as u32))
        .collect::<BTreeMap<_, _>>();
    let mut routes = Vec::new();
    for deployed in deployed_apis
        .into_iter()
        .filter(|deployed| deployed.status == "running")
    {
        let Some(provider_node) = node_by_host.get(&deployed.host_ip) else {
            continue;
        };
        let Some(surface) = surface_by_key.get(&(
            deployed.service_name.clone(),
            deployed.version.clone(),
            deployed.api_id.clone(),
        )) else {
            continue;
        };
        let (visible, distance, visibility_source) = if provider_node.node_id == target.node_id {
            if matches!(surface.visibility.as_str(), "same-node" | "global") {
                (true, 0, "same-node")
            } else {
                (false, 0, "")
            }
        } else if let Some(distance) = ancestor_distances.get(&provider_node.node_id) {
            if surface.visibility == "descendants" {
                (true, *distance, "ancestor-descendants")
            } else {
                (false, *distance, "")
            }
        } else {
            (false, 0, "")
        };
        if !visible {
            continue;
        }
        routes.push(EffectiveApiRoute {
            node_id: target.node_id.clone(),
            api_id: surface.api_id.clone(),
            provider_node_id: provider_node.node_id.clone(),
            provider_host_ip: provider_node.host_ip.clone(),
            provider_service_name: surface.service_name.clone(),
            provider_endpoint: deployed.endpoint.clone(),
            protocol: surface.protocol.clone(),
            path_prefix: surface.path_prefix.clone(),
            methods: surface.methods.clone(),
            permission: surface.permission.clone(),
            auth_mode: surface.auth_mode.clone(),
            visibility_source: visibility_source.to_string(),
            distance,
            status: deployed.status.clone(),
        });
    }
    routes.sort_by(|left, right| {
        left.distance
            .cmp(&right.distance)
            .then_with(|| left.api_id.cmp(&right.api_id))
            .then_with(|| left.provider_endpoint.cmp(&right.provider_endpoint))
    });
    Ok(routes)
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

fn service_api_surfaces_from_release(
    release: &ServiceReleaseManifest,
) -> Result<Vec<ServiceApiSurface>> {
    release
        .apis
        .iter()
        .map(|api| {
            let surface = ServiceApiSurface {
                service_name: release.service_name.clone(),
                version: release.version.clone(),
                api_id: api.api_id.clone(),
                protocol: api.protocol.clone(),
                port_name: api.port_name.clone(),
                path_prefix: api.path_prefix.clone(),
                methods: api
                    .methods
                    .iter()
                    .map(|method| method.to_ascii_uppercase())
                    .collect(),
                visibility: api.visibility.clone(),
                auth_mode: api.auth_mode.clone(),
                permission: api.permission.clone(),
                stability: api.stability.clone(),
                api_version: api.version.clone(),
                rate_limit: api.rate_limit.clone(),
                timeout: api.timeout.clone(),
                config: serde_json::json!({
                    "allowed_callers": api.allowed_callers,
                    "denied_callers": api.denied_callers,
                    "grpc_service": api.grpc_service,
                    "stream_name": api.stream_name,
                }),
                created_at: String::new(),
                updated_at: String::new(),
            };
            validate_service_api_surface(&surface)?;
            Ok(surface)
        })
        .collect()
}

fn deployed_service_apis_from_release(
    release: &ServiceReleaseManifest,
    host_ip: &str,
    endpoint: &str,
    status: &str,
) -> Result<Vec<DeployedServiceApi>> {
    release
        .apis
        .iter()
        .map(|api| {
            let deployed = DeployedServiceApi {
                host_ip: host_ip.to_string(),
                service_name: release.service_name.clone(),
                version: release.version.clone(),
                endpoint: endpoint.to_string(),
                api_id: api.api_id.clone(),
                status: status.to_string(),
                created_at: String::new(),
                updated_at: String::new(),
            };
            validate_deployed_service_api(&deployed)?;
            Ok(deployed)
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

fn service_identity_registration_from_release<S: OrchestratorStore>(
    store: &S,
    release: &ServiceReleaseManifest,
) -> Result<Option<AuthServiceIdentityRegistration>> {
    if release.service_identity.service_name.trim().is_empty()
        && release.service_identity.allowed_apis.is_empty()
    {
        return Ok(None);
    }
    let service_name = release.service_identity.service_name.trim();
    if service_name != release.service_name {
        return Err(OrchestratorError::InvalidManifest(
            "release service_identity service_name must match service_name".to_string(),
        ));
    }
    let allowed_apis = release
        .service_identity
        .allowed_apis
        .iter()
        .map(|api| api.trim().to_string())
        .filter(|api| !api.is_empty())
        .collect::<Vec<_>>();
    let surfaces = store.list_service_api_surfaces()?;
    let mut grants = Vec::new();
    for api_id in &allowed_apis {
        let surface = surfaces
            .iter()
            .find(|surface| surface.api_id == *api_id)
            .ok_or_else(|| {
                OrchestratorError::Dependency(format!(
                    "service_identity api {api_id} is not registered in API surface registry"
                ))
            })?;
        if surface.permission == "public" || surface.permission.trim().is_empty() {
            continue;
        }
        grants.push(AuthServiceIdentityGrant {
            api_id: api_id.clone(),
            permission: surface.permission.clone(),
        });
    }
    Ok(Some(AuthServiceIdentityRegistration {
        service_name: release.service_name.clone(),
        allowed_apis,
        grants,
    }))
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

fn redis_stream_name(resource: &ServiceRedisResource) -> String {
    if resource.service_name == "judge-api" && resource.name == "redis" {
        "ojos:judge:task".to_string()
    } else if resource.service_name == "judge-worker" && resource.name == "redis" {
        "ojos:judge:task".to_string()
    } else {
        format!(
            "ojos:{}:{}",
            resource.service_name.replace('-', ":"),
            resource.name.replace('-', ":")
        )
    }
}

fn redis_consumer_group_name(resource: &ServiceRedisResource) -> String {
    if resource.service_name == "judge-worker" && resource.name == "redis" {
        "judge-worker".to_string()
    } else {
        format!(
            "{}-{}",
            resource.service_name.replace(':', "-"),
            resource.name.replace(':', "-")
        )
    }
}

fn redis_socket_from_endpoint(endpoint: &str) -> String {
    let value = endpoint.trim();
    if let Some(rest) = value.strip_prefix("redis://") {
        let authority = rest.split('/').next().unwrap_or(rest);
        let without_auth = authority.rsplit('@').next().unwrap_or(authority);
        if without_auth.contains(':') {
            without_auth.to_string()
        } else {
            format!("{without_auth}:6379")
        }
    } else {
        value.to_string()
    }
}

struct SimpleRedisConnection {
    stream: TcpStream,
    timeout: Duration,
}

impl SimpleRedisConnection {
    fn connect(endpoint: &str, timeout: Duration) -> Result<Self> {
        let stream = TcpStream::connect(endpoint).map_err(|err| {
            OrchestratorError::Dependency(format!("connect redis {endpoint} failed: {err}"))
        })?;
        stream.set_read_timeout(Some(timeout)).map_err(|err| {
            OrchestratorError::Dependency(format!("configure redis read timeout failed: {err}"))
        })?;
        stream.set_write_timeout(Some(timeout)).map_err(|err| {
            OrchestratorError::Dependency(format!("configure redis write timeout failed: {err}"))
        })?;
        Ok(Self { stream, timeout })
    }

    fn send_command(&mut self, args: &[&str]) -> Result<()> {
        let mut payload = format!("*{}\r\n", args.len()).into_bytes();
        for arg in args {
            payload.extend_from_slice(format!("${}\r\n", arg.as_bytes().len()).as_bytes());
            payload.extend_from_slice(arg.as_bytes());
            payload.extend_from_slice(b"\r\n");
        }
        self.stream.write_all(&payload).map_err(|err| {
            OrchestratorError::Dependency(format!("write redis command failed: {err}"))
        })?;
        self.stream.flush().map_err(|err| {
            OrchestratorError::Dependency(format!("flush redis command failed: {err}"))
        })?;
        let response = read_redis_response(&mut self.stream, self.timeout)?;
        if response.starts_with("-BUSYGROUP") {
            return Ok(());
        }
        if response.starts_with('-') {
            return Err(OrchestratorError::Dependency(format!(
                "redis command failed: {}",
                response.trim()
            )));
        }
        Ok(())
    }
}

fn read_redis_response(stream: &mut TcpStream, timeout: Duration) -> Result<String> {
    stream.set_read_timeout(Some(timeout)).map_err(|err| {
        OrchestratorError::Dependency(format!("configure redis read timeout failed: {err}"))
    })?;
    let mut reader = BufReader::new(stream.try_clone().map_err(|err| {
        OrchestratorError::Dependency(format!("clone redis stream failed: {err}"))
    })?);
    let mut line = String::new();
    reader.read_line(&mut line).map_err(|err| {
        OrchestratorError::Dependency(format!("read redis response failed: {err}"))
    })?;
    if line.starts_with('$') {
        let len = line
            .trim()
            .trim_start_matches('$')
            .parse::<usize>()
            .map_err(|err| {
                OrchestratorError::Dependency(format!("parse redis bulk length failed: {err}"))
            })?;
        let mut body = vec![0; len + 2];
        reader.read_exact(&mut body).map_err(|err| {
            OrchestratorError::Dependency(format!("read redis bulk response failed: {err}"))
        })?;
    }
    Ok(line)
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

fn endpoint_from_install(
    service: &ServiceManifest,
    release: Option<&ServiceReleaseManifest>,
    endpoint_id: &str,
) -> Result<Endpoint> {
    validate_endpoint_id(endpoint_id)?;
    let identity = parse_endpoint_id(endpoint_id)?;
    if identity.service_name != service.id {
        return Err(OrchestratorError::InvalidManifest(
            "install endpoint service-name must match service id".to_string(),
        ));
    }
    Ok(Endpoint {
        endpoint: endpoint_id.to_string(),
        service_id: service.id.clone(),
        protocol: release
            .map(|release| release.backend.protocol.clone())
            .unwrap_or_else(|| service.endpoint.protocol.clone()),
        health_path: release
            .map(|release| release.backend.health_path.clone())
            .unwrap_or_else(|| service.endpoint.health_path.clone()),
        health: "unknown".to_string(),
        reachable: false,
        display_name: service.name.clone(),
        note: "allocated by release.install pipeline".to_string(),
        config: serde_json::json!({
            "port_name": "default",
            "visibility": if service.endpoint.expose { "public" } else { "cluster" },
            "source": "release.install",
        }),
        created_at: String::new(),
        updated_at: String::new(),
    })
}

fn host_service_from_install(
    service: &ServiceManifest,
    release: Option<&ServiceReleaseManifest>,
    host_ip: &str,
    status: &str,
    config: serde_json::Value,
    labels: serde_json::Value,
) -> Result<HostService> {
    let host_service = HostService {
        host_ip: host_ip.to_string(),
        service_name: service.id.clone(),
        version: release
            .map(|release| release.version.clone())
            .unwrap_or_else(|| service.version.clone()),
        status: status.to_string(),
        config,
        labels,
        created_at: String::new(),
        updated_at: String::new(),
    };
    validate_host_service(&host_service)?;
    Ok(host_service)
}

fn install_labels(release: Option<&ServiceReleaseManifest>) -> serde_json::Value {
    match release {
        Some(release) => serde_json::json!({
            "service_type": release.service_type,
            "runtime": release.runtime.kind,
            "source": "release.install",
        }),
        None => serde_json::json!({
            "source": "release.install",
        }),
    }
}

fn rendered_runtime_config(
    service: &ServiceManifest,
    release: Option<&ServiceReleaseManifest>,
    endpoint_id: &str,
    node_dispatch: Option<&NodeServiceDispatchResult>,
    driver_result: Option<&DriverResult>,
) -> serde_json::Value {
    let node_status = node_dispatch
        .map(|result| result.status.as_str())
        .unwrap_or("planned");
    let driver_status = driver_result
        .map(|result| result.status.as_str())
        .unwrap_or("deferred");
    serde_json::json!({
        "service_name": service.id,
        "endpoint": endpoint_id,
        "runtime": release
            .map(|release| serde_json::to_value(&release.runtime).unwrap_or_else(|_| serde_json::json!({})))
            .unwrap_or_else(|| serde_json::json!({
                "mode": format!("{:?}", service.runtime.mode),
                "driver": service.runtime.driver,
            })),
        "external_steps": {
            "package_fetch": release.is_some(),
            "node_dispatch": node_status,
            "service_start": driver_status,
            "node": node_dispatch.map(|result| serde_json::json!({
                "status": result.status,
                "message": result.message,
                "endpoint": result.endpoint,
                "accepted": result.accepted,
            })),
            "driver": driver_result.map(|result| serde_json::json!({
                "action": result.action,
                "status": result.status,
                "message": result.message,
                "command": result.command,
                "pid": result.pid,
                "pid_file": result.pid_file,
            }))
        }
    })
}

pub(crate) fn release_install_host_status(
    node_dispatch: Option<&NodeServiceDispatchResult>,
    driver_result: &DriverResult,
    health: &EndpointHealthResult,
) -> &'static str {
    if node_dispatch.is_some_and(|result| result.status == "failed") {
        return "failed";
    }
    if node_dispatch.is_some_and(|result| !result.accepted && result.status != "planned") {
        return "planned";
    }
    match driver_result.status.as_str() {
        "SUCCEEDED" if health.reachable => "running",
        "SUCCEEDED" => "starting",
        "FAILED" => "failed",
        "PLANNED" => "planned",
        _ => "deferred",
    }
}

fn deferred_install_health(
    endpoint: &Endpoint,
    driver_result: &DriverResult,
) -> EndpointHealthResult {
    EndpointHealthResult {
        endpoint: endpoint.endpoint.clone(),
        health: "deferred".to_string(),
        reachable: false,
        latency_ms: None,
        message: format!(
            "health probe deferred until service driver reaches SUCCEEDED; current driver status {}",
            driver_result.status
        ),
    }
}

fn external_running_install_health(endpoint: &Endpoint) -> EndpointHealthResult {
    EndpointHealthResult {
        endpoint: endpoint.endpoint.clone(),
        health: "ok".to_string(),
        reachable: true,
        latency_ms: None,
        message: "service process was started externally before release.install; service_start remains deferred"
            .to_string(),
    }
}

fn operation_bool_field(operation: &Operation, field: &str) -> bool {
    operation.request.get(field).is_some_and(json_truthy_value)
        || operation
            .request
            .get("install_options")
            .and_then(|value| value.get(field))
            .is_some_and(json_truthy_value)
}

fn gateway_reload_node_id(operation: &Operation) -> Option<String> {
    operation
        .request
        .get("gateway_node_id")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .or_else(|| {
            operation
                .request
                .get("install_options")
                .and_then(|value| value.get("gateway_node_id"))
                .and_then(serde_json::Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string)
        })
        .or_else(|| {
            std::env::var("GATEWAY_NODE_ID")
                .ok()
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty())
        })
}

fn json_truthy_value(value: &serde_json::Value) -> bool {
    value.as_bool().unwrap_or_else(|| {
        value.as_str().is_some_and(|text| {
            matches!(
                text.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
        })
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

pub(crate) fn execute_service_driver_action(
    service: &ServiceManifest,
    operation: &Operation,
    execute_fixed_commands: bool,
) -> Result<DriverResult> {
    let release_runtime = operation
        .request
        .get("release_manifest")
        .and_then(|value| value.get("runtime"))
        .cloned()
        .map(serde_json::from_value)
        .transpose()?;
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
        release_runtime,
    };
    if request
        .release_runtime
        .as_ref()
        .is_some_and(|runtime| runtime.kind.trim().eq_ignore_ascii_case("local-process"))
    {
        if execute_fixed_commands {
            return LocalProcessDriver::new().execute(&request);
        }
        return Ok(DriverResult {
            action: operation.action.clone(),
            status: "PLANNED".to_string(),
            message: "local-process runtime declared but execute_service_driver is false"
                .to_string(),
            command: Vec::new(),
            pid: None,
            pid_file: String::new(),
        });
    }
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

fn ensure_node_dispatch_accepted(result: &NodeServiceDispatchResult) -> Result<()> {
    if result.status.eq_ignore_ascii_case("failed") {
        return Err(OrchestratorError::Dependency(format!(
            "node dispatch failed: {}",
            result.message
        )));
    }
    if !result.accepted && result.status != "planned" {
        return Err(OrchestratorError::Dependency(format!(
            "node dispatch was not accepted: {}",
            result.message
        )));
    }
    Ok(())
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
            "pid": result.pid,
            "pid_file": result.pid_file,
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

fn release_package_log_record(
    operation_id: &str,
    service_name: &str,
    result: &ReleasePackageLoadResult,
) -> OperationLogRecord {
    let level = match result.status.as_str() {
        "loaded" => "info",
        "planned" => "warn",
        _ => "info",
    };
    operation_step_log_record(
        operation_id,
        format!("release-package:{service_name}"),
        level,
        format!(
            "release package load for {} {}: {}",
            service_name, result.status, result.message
        ),
        serde_json::json!({
            "service_name": service_name,
            "status": result.status,
            "source_url": result.source_url,
            "manifest_loaded": result.manifest_loaded,
            "checksum": result.checksum,
            "message": result.message,
        }),
    )
}

fn node_dispatch_log_record(
    operation_id: &str,
    service_name: &str,
    result: &NodeServiceDispatchResult,
) -> OperationLogRecord {
    let level = if result.status.eq_ignore_ascii_case("failed") {
        "error"
    } else if result.accepted {
        "info"
    } else {
        "warn"
    };
    operation_step_log_record(
        operation_id,
        format!("node-dispatch:{service_name}"),
        level,
        format!(
            "node dispatch for {} {}: {}",
            service_name, result.status, result.message
        ),
        serde_json::json!({
            "service_name": service_name,
            "status": result.status,
            "endpoint": result.endpoint,
            "accepted": result.accepted,
            "message": result.message,
        }),
    )
}

fn auth_permission_log_record(
    operation_id: &str,
    service_name: &str,
    result: &AuthPermissionRegistrationResult,
) -> OperationLogRecord {
    let level = if result.status == "registered" {
        "info"
    } else {
        "warn"
    };
    operation_step_log_record(
        operation_id,
        format!("auth-permissions:{service_name}"),
        level,
        format!(
            "auth permission registration for {} {}: {}",
            service_name, result.status, result.message
        ),
        serde_json::json!({
            "service_name": service_name,
            "status": result.status,
            "endpoint": result.endpoint,
            "registered": result.registered,
            "message": result.message,
        }),
    )
}

fn gateway_route_log_record(
    operation_id: &str,
    service_name: &str,
    api_count: usize,
    force_reload: bool,
    result: &GatewayRoutePublishResult,
) -> OperationLogRecord {
    let level = if result.reloaded { "info" } else { "warn" };
    let message = if result.reloaded {
        format!("[OK] gateway route reload completed for {service_name}")
    } else if result
        .message
        .to_ascii_lowercase()
        .contains("gateway_endpoint")
        || result
            .message
            .to_ascii_lowercase()
            .contains("gateway endpoint is not configured")
    {
        "[DEFERRED] gateway route reload skipped: gateway endpoint not configured".to_string()
    } else {
        format!(
            "gateway route reload for {} {}: {}",
            service_name, result.status, result.message
        )
    };
    operation_step_log_record(
        operation_id,
        format!("gateway_reload:{service_name}"),
        level,
        message,
        serde_json::json!({
            "service_name": service_name,
            "status": result.status,
            "endpoint": result.endpoint,
            "route_count": result.route_count,
            "api_count": api_count,
            "force_reload": force_reload,
            "reloaded": result.reloaded,
            "message": result.message,
        }),
    )
}

fn redis_provision_log_record(
    operation_id: &str,
    service_name: &str,
    result: &RedisProvisionResult,
) -> OperationLogRecord {
    let level = match result.status.as_str() {
        "created" | "updated" | "ok" => "info",
        "skipped" => "warn",
        _ => "info",
    };
    operation_step_log_record(
        operation_id,
        format!("redis-resources:{service_name}"),
        level,
        format!(
            "redis resource provisioning for {} {}: {}",
            service_name, result.status, result.message
        ),
        serde_json::json!({
            "service_name": service_name,
            "status": result.status,
            "endpoint": result.endpoint,
            "provisioned": result.provisioned,
            "message": result.message,
        }),
    )
}

fn storage_provision_log_record(
    operation_id: &str,
    service_name: &str,
    result: &StorageProvisionResult,
) -> OperationLogRecord {
    let level = match result.status.as_str() {
        "ensured" | "created" | "ok" => "info",
        "skipped" => "warn",
        _ => "info",
    };
    operation_step_log_record(
        operation_id,
        format!("storage-resources:{service_name}"),
        level,
        format!(
            "storage resource provisioning for {} {}: {}",
            service_name, result.status, result.message
        ),
        serde_json::json!({
            "service_name": service_name,
            "status": result.status,
            "endpoint": result.endpoint,
            "provisioned": result.provisioned,
            "message": result.message,
        }),
    )
}

fn migration_execution_log_record(
    operation_id: &str,
    service_name: &str,
    result: &MigrationExecutionResult,
) -> OperationLogRecord {
    let level = match result.status.as_str() {
        "applied" | "dry-run" | "skipped" => "info",
        "deferred" => "warn",
        "failed" => "error",
        _ => "info",
    };
    operation_step_log_record(
        operation_id,
        format!("migrations:{service_name}"),
        level,
        format!(
            "migration execution for {} {}: {}",
            service_name, result.status, result.message
        ),
        serde_json::json!({
            "service_name": service_name,
            "status": result.status,
            "runner": result.runner,
            "dry_run": result.dry_run,
            "executed": result.executed,
            "message": result.message,
        }),
    )
}

fn release_install_pipeline_log_record(
    operation_id: &str,
    service: &ServiceManifest,
    release: Option<&ServiceReleaseManifest>,
    host_ip: &str,
    endpoint: &str,
    health: &EndpointHealthResult,
    migration_execution: Option<&MigrationExecutionResult>,
    auth_permission_registration: Option<&AuthPermissionRegistrationResult>,
    redis_provision: Option<&RedisProvisionResult>,
    storage_provision: Option<&StorageProvisionResult>,
    release_package_load: Option<&ReleasePackageLoadResult>,
    gateway_route_publish: Option<&GatewayRoutePublishResult>,
    node_dispatch: Option<&NodeServiceDispatchResult>,
    driver_result: &DriverResult,
) -> OperationLogRecord {
    let migration_status = match (release, migration_execution) {
        (Some(release), Some(result)) if !release.migrations.is_empty() => result.status.as_str(),
        (Some(release), None) if !release.migrations.is_empty() => "pending",
        _ => "none",
    };
    let permission_registration_status = match (release, auth_permission_registration) {
        (Some(release), Some(result)) if !release.permissions.is_empty() => result.status.as_str(),
        (Some(release), None) if !release.permissions.is_empty() => "pending",
        _ => "none",
    };
    let redis_resources_status = match (release, redis_provision) {
        (Some(release), Some(result)) if !release.redis.is_empty() => result.status.as_str(),
        (Some(release), None) if !release.redis.is_empty() => "pending",
        _ => "none",
    };
    let storage_resources_status = match (release, storage_provision) {
        (Some(release), Some(result)) if !release.storage.is_empty() => result.status.as_str(),
        (Some(release), None) if !release.storage.is_empty() => "pending",
        _ => "none",
    };
    let gateway_route_status = match (release, gateway_route_publish) {
        (Some(release), Some(result)) if !release.routes.is_empty() || !release.apis.is_empty() => {
            result.status.as_str()
        }
        (Some(release), None) if !release.routes.is_empty() || !release.apis.is_empty() => {
            "pending"
        }
        _ => "none",
    };
    operation_step_log_record(
        operation_id,
        format!("install-pipeline:{}", service.id),
        "info",
        format!(
            "release.install pipeline recorded {} on {} as {}",
            service.id, host_ip, endpoint
        ),
        serde_json::json!({
            "service_name": service.id,
            "version": release.map(|release| release.version.as_str()).unwrap_or(service.version.as_str()),
            "host_ip": host_ip,
            "endpoint": endpoint,
            "release_package": release_package_load.map(|result| serde_json::json!({
                "status": result.status,
                "source_url": result.source_url,
                "manifest_loaded": result.manifest_loaded,
                "checksum": result.checksum,
                "message": result.message,
            })),
            "host_service": "created",
            "endpoint_allocation": "created",
            "migrations": {
                "count": release.map(|release| release.migrations.len()).unwrap_or(0),
                "status": migration_status,
                "runner": migration_execution.map(|result| result.runner.as_str()).unwrap_or("none"),
                "dry_run": migration_execution.is_some_and(|result| result.dry_run),
                "executed": migration_execution.map(|result| serde_json::json!(result.executed.clone())),
                "message": migration_execution.map(|result| result.message.as_str())
            },
            "permission_registration": permission_registration_status,
            "auth_permission_registration": auth_permission_registration.map(|result| serde_json::json!({
                "status": result.status,
                "endpoint": result.endpoint,
                "registered": result.registered,
                "message": result.message,
            })),
            "gateway_route_update": gateway_route_status,
            "gateway_route_publish": gateway_route_publish.map(|result| serde_json::json!({
                "status": result.status,
                "endpoint": result.endpoint,
                "route_count": result.route_count,
                "api_count": release.map(|release| release.apis.len()).unwrap_or(0),
                "reloaded": result.reloaded,
                "message": result.message,
            })),
            "frontend_registration": if release.is_some_and(|release| release.frontend.enabled) { "registry-only" } else { "none" },
            "redis_resources": redis_resources_status,
            "redis_provision": redis_provision.map(|result| serde_json::json!({
                "status": result.status,
                "endpoint": result.endpoint,
                "provisioned": result.provisioned,
                "message": result.message,
            })),
            "storage_resources": storage_resources_status,
            "storage_provision": storage_provision.map(|result| serde_json::json!({
                "status": result.status,
                "endpoint": result.endpoint,
                "provisioned": result.provisioned,
                "message": result.message,
            })),
            "node_dispatch": node_dispatch.map(|result| result.status.as_str()).unwrap_or("planned"),
            "node_dispatch_result": node_dispatch.map(|result| serde_json::json!({
                "status": result.status,
                "endpoint": result.endpoint,
                "accepted": result.accepted,
                "message": result.message,
            })),
            "service_start": driver_result.status,
            "service_driver": {
                "action": driver_result.action,
                "status": driver_result.status,
                "message": driver_result.message,
                "command": driver_result.command,
                "pid": driver_result.pid,
                "pid_file": driver_result.pid_file,
            },
            "health": {
                "status": health.health,
                "reachable": health.reachable,
                "message": health.message,
            }
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

fn env_flag(name: &str) -> bool {
    std::env::var(name).ok().is_some_and(|value| truthy(&value))
}

fn env_usize(name: &str) -> Option<usize> {
    std::env::var(name)
        .ok()
        .and_then(|value| value.trim().parse::<usize>().ok())
}

fn env_u64(name: &str) -> Option<u64> {
    std::env::var(name)
        .ok()
        .and_then(|value| value.trim().parse::<u64>().ok())
}

fn service_database_url_env_candidates(service_name: &str) -> Vec<String> {
    let key = service_env_key(service_name);
    let mut prefixes = vec![key.clone()];
    for suffix in ["_SERVICE", "_API", "_WORKER"] {
        if let Some(prefix) = key.strip_suffix(suffix) {
            if !prefix.is_empty() && !prefixes.iter().any(|item| item == prefix) {
                prefixes.push(prefix.to_string());
            }
        }
    }

    let mut candidates = Vec::new();
    for prefix in &prefixes {
        candidates.push(format!("ORCHESTRATOR_MIGRATION_DATABASE_URL_{prefix}"));
        candidates.push(format!("OJOS_MIGRATION_DATABASE_URL_{prefix}"));
        candidates.push(format!("{prefix}_DATABASE_URL"));
    }
    candidates
}

fn service_env_key(service_name: &str) -> String {
    service_name
        .trim()
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() {
                ch.to_ascii_uppercase()
            } else {
                '_'
            }
        })
        .collect()
}

fn empty_checksum_label(value: &str) -> &str {
    if value.trim().is_empty() {
        "<empty>"
    } else {
        value
    }
}

fn truthy(value: &str) -> bool {
    matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "1" | "true" | "yes" | "on"
    )
}

fn safe_child_path(root: &Path, child: &str) -> Result<PathBuf> {
    let child = child.trim();
    if child.is_empty() {
        return Err(OrchestratorError::UnsafePath(
            "migration path is required".to_string(),
        ));
    }
    let child_path = Path::new(child);
    let mut normalized = PathBuf::new();
    for component in child_path.components() {
        match component {
            Component::Normal(value) => normalized.push(value),
            Component::CurDir => {}
            Component::ParentDir => {
                return Err(OrchestratorError::UnsafePath(
                    "path traversal is not allowed".to_string(),
                ));
            }
            Component::RootDir | Component::Prefix(_) => {
                return Err(OrchestratorError::UnsafePath(
                    "absolute path is not allowed".to_string(),
                ));
            }
        }
    }
    Ok(root.join(normalized))
}

fn validate_migration_checksum(
    migration: &crate::ReleaseMigrationDecl,
    bytes: &[u8],
) -> Result<()> {
    let checksum = migration.checksum.trim();
    if checksum.is_empty() {
        return Ok(());
    }
    if let Some(expected) = checksum.strip_prefix("sha256:") {
        let expected = expected.trim().to_ascii_lowercase();
        let actual = format!("{:x}", Sha256::digest(bytes));
        if expected == actual {
            return Ok(());
        }
        return Err(OrchestratorError::Dependency(format!(
            "migration {} checksum mismatch: expected sha256:{expected}, got sha256:{actual}",
            migration.version
        )));
    }
    if let Some(expected) = checksum.strip_prefix("len:") {
        let expected = expected.trim().parse::<usize>().map_err(|err| {
            OrchestratorError::InvalidManifest(format!(
                "migration {} checksum len is invalid: {err}",
                migration.version
            ))
        })?;
        if expected == bytes.len() {
            return Ok(());
        }
        return Err(OrchestratorError::Dependency(format!(
            "migration {} checksum mismatch: expected len:{expected}, got len:{}",
            migration.version,
            bytes.len()
        )));
    }
    Err(OrchestratorError::InvalidManifest(format!(
        "migration {} checksum format is unsupported; supported formats are sha256:<hex> and len:<bytes>",
        migration.version
    )))
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
