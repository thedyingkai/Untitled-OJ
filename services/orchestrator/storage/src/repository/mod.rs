//! Repository contract and explicit memory/shared adapters.
//! Durable adapters override multi-record writes with one transaction.
mod memory;
mod shared;
pub use memory::MemoryOrchestratorStore;
use orchestrator_core::{
    DeployedServiceApi, DiagnosticReport, EffectiveApiRoute, Endpoint, HostService, Link, LogView,
    NodeRecord, Operation, OperationLock, OperationLogRecord, OperationStatus, OrchestratorError,
    RenderedServiceConfig, Result, ServiceApiSurface, ServiceFrontendEntry, ServiceManifest,
    ServiceMigrationRecord, ServicePermissionRecord, ServiceRedisResource, ServiceRelease,
    ServiceRoute, ServiceStorageResource, Topology, TopologySnapshot, ancestors_of_from_nodes,
    descendants_of_from_nodes, effective_api_routes_from_registry, validate_service_manifest,
    validate_service_release_record,
};
pub use shared::SharedOrchestratorStore;

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

    /// Atomically publishes a validated Service and its exact Release record.
    /// Durable implementations override this with one database transaction.
    fn register_service_release_atomic(
        &mut self,
        service: ServiceManifest,
        release: ServiceRelease,
    ) -> Result<()> {
        validate_service_manifest(&service)?;
        validate_service_release_record(&release)?;
        if service.id != release.service_name || service.version != release.version {
            return Err(OrchestratorError::InvalidManifest(
                "service and release identities must match".to_string(),
            ));
        }
        let previous_service = self.get_service(&service.id)?;
        let previous_release = self.get_service_release(&release.service_name, &release.version)?;
        self.upsert_service(service.clone())?;
        if let Err(error) = self.upsert_service_release(release.clone()) {
            let restore_service = match previous_service {
                Some(previous) => self.upsert_service(previous),
                None => self.delete_service(&service.id),
            };
            let restore_release = match previous_release {
                Some(previous) => self.upsert_service_release(previous),
                None => self.delete_service_release(&release.service_name, &release.version),
            };
            if let Err(rollback) = restore_service.and(restore_release) {
                return Err(OrchestratorError::Dependency(format!(
                    "release registration failed ({error}) and rollback failed ({rollback})"
                )));
            }
            return Err(error);
        }
        Ok(())
    }

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
