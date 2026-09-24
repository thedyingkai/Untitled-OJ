//! Cloneable per-call serialization for an injected repository.
use super::OrchestratorStore;
use orchestrator_core::{
    DeployedServiceApi, DiagnosticReport, Endpoint, HostService, Link, LogView, NodeRecord,
    Operation, OperationLock, OperationLogRecord, OperationStatus, OrchestratorError,
    RenderedServiceConfig, Result, ServiceApiSurface, ServiceFrontendEntry, ServiceManifest,
    ServiceMigrationRecord, ServicePermissionRecord, ServiceRedisResource, ServiceRelease,
    ServiceRoute, ServiceStorageResource, Topology, TopologySnapshot,
};
use std::sync::{Arc, Mutex};

/// Cloneable adapter around an injected durable store.
///
/// The lock is acquired for one repository method at a time. Network access,
/// health checks, downloads, and runtime driver calls therefore do not retain
/// a database-wide mutex merely because the dispatcher owns this adapter.
#[derive(Clone)]
pub struct SharedOrchestratorStore {
    inner: Arc<Mutex<Box<dyn OrchestratorStore + Send>>>,
    kind: Arc<str>,
}

impl std::fmt::Debug for SharedOrchestratorStore {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SharedOrchestratorStore")
            .field("kind", &self.kind)
            .finish_non_exhaustive()
    }
}

impl SharedOrchestratorStore {
    pub fn new(kind: impl Into<String>, store: impl OrchestratorStore + Send + 'static) -> Self {
        Self {
            inner: Arc::new(Mutex::new(Box::new(store))),
            kind: Arc::from(kind.into()),
        }
    }

    pub fn kind(&self) -> &str {
        &self.kind
    }

    fn read<T>(
        &self,
        read: impl FnOnce(&(dyn OrchestratorStore + Send)) -> Result<T>,
    ) -> Result<T> {
        let store = self.inner.lock().map_err(|_| {
            OrchestratorError::Dependency("durable store lock poisoned".to_string())
        })?;
        read(store.as_ref())
    }

    fn write<T>(
        &mut self,
        write: impl FnOnce(&mut (dyn OrchestratorStore + Send)) -> Result<T>,
    ) -> Result<T> {
        let mut store = self.inner.lock().map_err(|_| {
            OrchestratorError::Dependency("durable store lock poisoned".to_string())
        })?;
        write(store.as_mut())
    }
}

macro_rules! shared_read {
    ($name:ident ( $( $arg:ident : $ty:ty ),* $(,)? ) -> $result:ty) => {
        fn $name(&self, $( $arg: $ty ),*) -> Result<$result> {
            self.read(|store| store.$name($( $arg ),*))
        }
    };
}

macro_rules! shared_write {
    ($name:ident ( $( $arg:ident : $ty:ty ),* $(,)? ) -> $result:ty) => {
        fn $name(&mut self, $( $arg: $ty ),*) -> Result<$result> {
            self.write(|store| store.$name($( $arg ),*))
        }
    };
}

impl OrchestratorStore for SharedOrchestratorStore {
    shared_read!(list_services() -> Vec<ServiceManifest>);
    shared_read!(get_service(service_id: &str) -> Option<ServiceManifest>);
    shared_write!(upsert_service(service: ServiceManifest) -> ());
    shared_write!(delete_service(service_id: &str) -> ());

    shared_read!(list_host_services() -> Vec<HostService>);
    shared_read!(get_host_service(host_ip: &str, service_name: &str) -> Option<HostService>);
    shared_write!(upsert_host_service(host_service: HostService) -> ());
    shared_write!(delete_host_service(host_ip: &str, service_name: &str) -> ());
    shared_write!(delete_host_services_for_service(service_name: &str) -> ());

    shared_read!(list_service_releases() -> Vec<ServiceRelease>);
    shared_read!(get_service_release(service_name: &str, version: &str) -> Option<ServiceRelease>);
    shared_write!(upsert_service_release(release: ServiceRelease) -> ());
    shared_write!(delete_service_release(service_name: &str, version: &str) -> ());
    shared_write!(register_service_release_atomic(service: ServiceManifest, release: ServiceRelease) -> ());

    shared_read!(list_service_routes() -> Vec<ServiceRoute>);
    shared_write!(upsert_service_route(route: ServiceRoute) -> ());
    shared_write!(delete_service_routes_for_service(service_name: &str) -> ());

    shared_read!(list_service_migration_records() -> Vec<ServiceMigrationRecord>);
    shared_write!(upsert_service_migration_record(record: ServiceMigrationRecord) -> ());
    shared_write!(delete_service_migration_records_for_service(service_name: &str) -> ());

    shared_read!(list_service_permission_records() -> Vec<ServicePermissionRecord>);
    shared_write!(upsert_service_permission_record(record: ServicePermissionRecord) -> ());
    shared_write!(delete_service_permission_records_for_service(service_name: &str) -> ());

    shared_read!(list_service_frontend_entries() -> Vec<ServiceFrontendEntry>);
    shared_write!(upsert_service_frontend_entry(entry: ServiceFrontendEntry) -> ());
    shared_write!(delete_service_frontend_entry(service_name: &str) -> ());

    shared_read!(list_service_redis_resources() -> Vec<ServiceRedisResource>);
    shared_write!(upsert_service_redis_resource(resource: ServiceRedisResource) -> ());
    shared_write!(delete_service_redis_resources_for_service(service_name: &str) -> ());

    shared_read!(list_service_storage_resources() -> Vec<ServiceStorageResource>);
    shared_write!(upsert_service_storage_resource(resource: ServiceStorageResource) -> ());
    shared_write!(delete_service_storage_resources_for_service(service_name: &str) -> ());

    shared_read!(list_rendered_service_configs() -> Vec<RenderedServiceConfig>);
    shared_write!(upsert_rendered_service_config(config: RenderedServiceConfig) -> ());
    shared_write!(delete_rendered_service_configs_for_service(service_name: &str) -> ());

    shared_read!(list_nodes() -> Vec<NodeRecord>);
    shared_read!(get_node(node_id: &str) -> Option<NodeRecord>);
    shared_write!(upsert_node(node: NodeRecord) -> ());
    shared_write!(delete_node(node_id: &str) -> ());

    shared_read!(list_service_api_surfaces() -> Vec<ServiceApiSurface>);
    shared_write!(upsert_service_api_surface(api: ServiceApiSurface) -> ());
    shared_write!(delete_service_api_surfaces_for_service(service_name: &str) -> ());

    shared_read!(list_deployed_service_apis() -> Vec<DeployedServiceApi>);
    shared_write!(upsert_deployed_service_api(api: DeployedServiceApi) -> ());
    shared_write!(delete_deployed_service_apis_for_service(service_name: &str) -> ());

    shared_read!(list_endpoints() -> Vec<Endpoint>);
    shared_read!(get_endpoint(endpoint: &str) -> Option<Endpoint>);
    shared_write!(upsert_endpoint(endpoint: Endpoint) -> ());
    shared_write!(delete_endpoint(endpoint: &str) -> ());
    shared_write!(update_endpoint_health(endpoint: &str, health: String, reachable: bool) -> ());

    shared_read!(list_links() -> Vec<Link>);
    shared_read!(get_link(source_endpoint: &str, target_endpoint: &str) -> Option<Link>);
    shared_write!(upsert_link(link: Link) -> ());
    shared_write!(delete_link(source_endpoint: &str, target_endpoint: &str) -> ());
    shared_write!(update_link_health(source_endpoint: &str, target_endpoint: &str, health: String, latency_ms: Option<u32>) -> ());

    shared_write!(create_operation(operation: Operation) -> ());
    shared_read!(get_operation(operation_id: &str) -> Option<Operation>);
    shared_read!(list_operations() -> Vec<Operation>);
    shared_write!(update_operation(operation: Operation) -> ());
    shared_write!(update_operation_status(operation_id: &str, status: OperationStatus, error_message: String) -> ());
    shared_write!(update_operation_result(operation_id: &str, result: serde_json::Value) -> ());
    shared_write!(append_operation_log(record: OperationLogRecord) -> ());
    shared_read!(list_operation_logs(operation_id: &str) -> Vec<OperationLogRecord>);
    shared_write!(acquire_operation_lock(lock: OperationLock) -> bool);
    shared_write!(release_operation_lock(lock_key: &str, operation_id: &str) -> ());

    shared_write!(save_topology_snapshot(snapshot: TopologySnapshot) -> ());
    shared_read!(get_latest_topology_snapshot() -> Option<TopologySnapshot>);
    shared_read!(build_topology_view() -> Topology);
    shared_write!(delete_topology(root_endpoint: &str) -> ());

    shared_read!(list_log_sources() -> Vec<LogView>);
    shared_write!(upsert_log_source(log_view: LogView) -> ());
    shared_write!(delete_log_source(source_id: &str) -> ());

    shared_write!(create_diagnostic_report(report: DiagnosticReport) -> ());
    shared_read!(get_diagnostic_report(report_id: &str) -> Option<DiagnosticReport>);
    shared_read!(list_diagnostic_reports() -> Vec<DiagnosticReport>);
}
