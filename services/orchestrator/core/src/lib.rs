mod action;
mod database;
mod dispatcher;
mod executor;
mod health;
mod model;
mod observability;
mod planner;
mod reconciler;
mod schema;
mod service;
mod store;
mod view;
mod workbench;

pub use action::{
    ACTION_CATALOG, ActionDescriptor, ActionPlanMode, ActionRisk, CORE_ACTION_TARGETS,
    FORMAL_ACTION_PREFIXES, action_catalog, action_descriptor, validate_action_catalog,
};
pub use database::{
    DatabaseAccessReport, DatabaseSchemaReport, DatabaseStatement, DatabaseWrite,
    DatabaseWritePlan, ORCHESTRATOR_DATABASE_STATEMENTS, ORCHESTRATOR_TABLES, PgOrchestratorStore,
    inspect_database_access, inspect_orchestrator_schema, plan_database_writes,
};
pub use dispatcher::{
    ActionCapabilityStatus, ActionDispatchResult, ActionMatrixEntry, OrchestratorActionConsole,
    OrchestratorActionDispatcher, SmokeControlPlaneSeed, action_matrix, capability_for_action,
    default_console_request,
};
pub use executor::{
    DockerComposeDriver, DriverRequest, DriverResult, ExecutionDriver, ExternalEndpointDriver,
    LocalProcessDriver, driver_request_for_endpoint,
};
pub use health::{
    EndpointHealthResult, EndpointProbe, LinkHealthResult, StaticEndpointProbe, TcpEndpointProbe,
    check_endpoint_health_with_probe, check_link_health,
};
pub use model::{
    DeployedServiceApi, DiagnosticFinding, DiagnosticReport, EffectiveApiRoute, Endpoint,
    HostService, Link, LogView, NodeRecord, Operation, OperationLock, OperationLogRecord,
    OperationStatus, RenderedServiceConfig, ServiceApiSurface, ServiceFrontendEntry,
    ServiceMigrationRecord, ServicePermissionRecord, ServiceRedisResource, ServiceRelease,
    ServiceRoute, ServiceStorageResource, Topology, TopologyAuthority, TopologySnapshot,
    build_topology, cancel_operation, confirm_operation, diagnostic_report_json, expire_operation,
    fail_operation, operation_log_record, operation_step_log_record, plan_operation,
    redact_secret_text, rollback_operation, start_operation, succeed_operation, topology_authority,
    validate_deployed_service_api, validate_endpoint, validate_host_service, validate_link,
    validate_node_record, validate_rendered_service_config, validate_service_api_surface,
    validate_service_frontend_entry, validate_service_migration_record,
    validate_service_permission_record, validate_service_redis_resource,
    validate_service_release_record, validate_service_route, validate_service_storage_resource,
    validate_topology,
};
pub use observability::{
    DiagnosticExport, LogQuery, LogQueryResult, build_diagnostic_report, export_diagnostic_report,
    query_logs, validate_log_view,
};
pub use planner::{
    ActionPlanPreview, ActionRequest, default_action_request, plan_action_preview,
    plan_action_preview_with_releases, plan_action_request, plan_action_request_with_releases,
    preview_operation,
};
pub use reconciler::{
    ReconcileLoopConfig, ReconcileLoopResult, ReconcileTickResult, run_reconcile_loop,
    run_reconcile_tick,
};
pub use schema::{
    ActionFormSchema, FormFieldSchema, SharedSchemas, ensure_shared_schemas_loaded,
    load_shared_schemas,
};
pub use service::{
    DeploymentTemplate, DeploymentTemplateEndpoint, DeploymentTemplateLink,
    DeploymentTemplateOperations, DeploymentTemplatePreview, DeploymentTemplateService,
    DeploymentTemplateServiceSpec, EndpointDecl, EndpointIdentity, ReleaseApiSurfaceDecl,
    ReleaseBackendDecl, ReleaseFrontendDecl, ReleaseMigrationDecl, ReleaseObservabilityDecl,
    ReleaseRedisDecl, ReleaseRouteDecl, ReleaseRuntimeDecl, ReleaseSourceDecl, ReleaseStorageDecl,
    RuntimeMode, ServiceHealthDecl, ServiceManifest, ServiceProvides, ServiceReleaseManifest,
    ServiceRequires, ServiceRuntimeDecl, ServiceSecurityDecl, ServiceUiDecl, SourceDecl,
    diagnostic_export_operation, endpoint_create_operation, endpoint_delete_operation,
    endpoint_health_check_operation, endpoint_socket_addr, endpoint_update_operation,
    link_create_operation, link_delete_operation, link_health_check_operation,
    link_update_operation, log_create_operation, log_query_operation, parse_endpoint_id,
    preview_deployment_template, release_create_operation, release_delete_operation,
    release_install_operation, release_install_operation_with_release, release_rollback_operation,
    release_update_operation, service_health_check_operation, service_lifecycle_operation,
    topology_apply_operation, validate_deployment_template, validate_deployment_template_file,
    validate_deployment_template_references, validate_endpoint_id, validate_endpoint_service_name,
    validate_service_manifest, validate_service_manifest_file, validate_service_release,
    validate_service_release_file,
};
pub use store::{
    AuthPermissionRegistrar, AuthPermissionRegistration, AuthPermissionRegistrationResult,
    ConfiguredAuthPermissionRegistrar, ConfiguredGatewayRoutePublisher, ConfiguredMigrationRunner,
    ConfiguredNodeServiceDispatcher, ConfiguredRedisResourceProvisioner,
    ConfiguredReleasePackageLoader, ConfiguredStorageResourceProvisioner,
    DeferredAuthPermissionRegistrar, DeferredGatewayRoutePublisher, DeferredMigrationRunner,
    DeferredNodeServiceDispatcher, DeferredRedisResourceProvisioner, DeferredReleasePackageLoader,
    DeferredStorageResourceProvisioner, GatewayRoutePublishRequest, GatewayRoutePublishResult,
    GatewayRoutePublisher, HttpAuthPermissionRegistrar, HttpGatewayRoutePublisher,
    HttpNodeServiceDispatcher, HttpStorageResourceProvisioner, LocalReleasePackageLoader,
    LocalSqlMigrationRunner, MemoryOrchestratorStore, MigrationExecutionRecord,
    MigrationExecutionRequest, MigrationExecutionResult, MigrationRunner,
    NodeServiceDispatchRequest, NodeServiceDispatchResult, NodeServiceDispatcher,
    OperationExecutor, OrchestratorStore, RedisProvisionRequest, RedisProvisionResult,
    RedisProvisionedResource, RedisResourceProvisioner, ReleasePackageLoadRequest,
    ReleasePackageLoadResult, ReleasePackageLoader, StorageProvisionRequest,
    StorageProvisionResult, StorageProvisionedResource, StorageResourceProvisioner,
    TcpRedisResourceProvisioner,
};
pub use view::{
    DiagnosticViewRow, EndpointViewRow, LinkViewRow, LogViewRow, OperationViewRow,
    OperationWorkbenchView, OrchestratorView, OrchestratorViewPage, ReleaseRegistryViewRow,
    ServiceViewRow, TemplateViewRow, endpoint_hosts, ensure_view_is_loaded, load_orchestrator_view,
    load_orchestrator_view_from_store, merge_operation_workbench_session_into_view,
};
pub use workbench::{
    OperationWorkbench, OperationWorkbenchContext, OperationWorkbenchRun,
    OperationWorkbenchSession, apply_operation_workbench_session, build_operation_workbench,
    build_operation_workbench_from_request, build_operation_workbench_from_request_with_releases,
    build_operation_workbench_with_releases, confirm_operation_workbench_session,
    load_operation_workbench_context, load_operation_workbench_context_from_store,
    new_operation_workbench_session, rollback_operation_workbench_session,
    run_operation_workbench_flow, update_operation_workbench_field,
    update_operation_workbench_field_with_releases,
};

use thiserror::Error;

#[derive(Debug, Error)]
pub enum OrchestratorError {
    #[error("invalid manifest: {0}")]
    InvalidManifest(String),
    #[error("unsafe path: {0}")]
    UnsafePath(String),
    #[error("dependency error: {0}")]
    Dependency(String),
    #[error("operation blocked: {0}")]
    Blocked(String),
    #[error("io error")]
    Io(#[from] std::io::Error),
    #[error("yaml parse error: {0}")]
    Yaml(#[from] serde_yaml::Error),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
}

pub type Result<T> = std::result::Result<T, OrchestratorError>;

pub fn sanitize_path_for_error(path: &std::path::Path) -> String {
    path.file_name()
        .and_then(|v| v.to_str())
        .unwrap_or("path")
        .to_string()
}

#[cfg(test)]
mod tests;
