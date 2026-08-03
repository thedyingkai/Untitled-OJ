//! Explicit compatibility and infrastructure boundary for the pre-v1
//! orchestrator console.
//!
//! New domain code belongs in `orchestrator-core`. Database clients, network
//! probes, archive/file loading, process execution, repository-backed views and
//! the 0.2 action console live here so they cannot silently enter the core
//! dependency graph.

pub use orchestrator_domain::*;

mod database;
mod dispatcher;
mod executor;
mod health;
mod market;
mod observability;
mod reconciler;
mod schema_io;
mod service_io;
mod store;
mod view;
mod workbench;

pub use database::{
    DatabaseAccessReport, DatabaseSchemaReport, DatabaseStatement, DatabaseWrite,
    DatabaseWritePlan, ORCHESTRATOR_DATABASE_STATEMENTS, PgOrchestratorStore,
    inspect_database_access, inspect_orchestrator_schema, plan_database_writes,
};
pub use dispatcher::{
    ActionCapabilityStatus, ActionDispatchResult, ActionMatrixEntry, OrchestratorActionConsole,
    OrchestratorActionDispatcher, SmokeControlPlaneSeed, SmokeNodeTreeSeed, action_matrix,
    capability_for_action, default_console_request,
};
pub use executor::{
    DockerComposeDriver, DriverRequest, DriverResult, ExecutionDriver, ExternalEndpointDriver,
    LocalProcessDriver, driver_request_for_endpoint,
};
pub use health::{
    EndpointHealthResult, EndpointProbe, LinkHealthResult, StaticEndpointProbe, TcpEndpointProbe,
    check_endpoint_health_with_probe, check_link_health,
};
pub use market::{
    ExternalReleaseImport, external_release_import_from_yaml, register_external_release_into_store,
    release_source_kind_for_url, service_manifest_from_release,
};
pub use observability::{
    DiagnosticExport, LogQuery, LogQueryResult, build_diagnostic_report, export_diagnostic_report,
    query_logs, validate_log_view,
};
pub use reconciler::{
    ReconcileLoopConfig, ReconcileLoopResult, ReconcileTickResult, run_reconcile_loop,
    run_reconcile_tick,
};
pub use schema_io::load_shared_schemas;
pub use service_io::{
    validate_deployment_template_file, validate_deployment_template_references,
    validate_service_manifest_file, validate_service_release_file,
};
pub use store::{
    AuthPermissionRegistrar, AuthPermissionRegistration, AuthPermissionRegistrationResult,
    AuthServiceIdentityGrant, AuthServiceIdentityRegistration, ConfiguredAuthPermissionRegistrar,
    ConfiguredGatewayRoutePublisher, ConfiguredMigrationRunner, ConfiguredNodeServiceDispatcher,
    ConfiguredRedisResourceProvisioner, ConfiguredReleasePackageLoader,
    ConfiguredStorageResourceProvisioner, DeferredAuthPermissionRegistrar,
    DeferredGatewayRoutePublisher, DeferredMigrationRunner, DeferredNodeServiceDispatcher,
    DeferredRedisResourceProvisioner, DeferredReleasePackageLoader,
    DeferredStorageResourceProvisioner, FetchedReleaseSource, GatewayRoutePublishRequest,
    GatewayRoutePublishResult, GatewayRoutePublisher, HttpAuthPermissionRegistrar,
    HttpGatewayRoutePublisher, HttpNodeServiceDispatcher, HttpStorageResourceProvisioner,
    LocalReleasePackageLoader, LocalSqlMigrationRunner, MemoryOrchestratorStore,
    MigrationExecutionRecord, MigrationExecutionRequest, MigrationExecutionResult, MigrationRunner,
    NodeServiceDispatchRequest, NodeServiceDispatchResult, NodeServiceDispatcher,
    OperationExecutor, OrchestratorStore, RedisProvisionRequest, RedisProvisionResult,
    RedisProvisionedResource, RedisResourceProvisioner, ReleasePackageLoadRequest,
    ReleasePackageLoadResult, ReleasePackageLoader, SharedOrchestratorStore,
    StorageProvisionRequest, StorageProvisionResult, StorageProvisionedResource,
    StorageResourceProvisioner, TcpRedisResourceProvisioner, resolve_outbound_redirect,
    validate_outbound_url,
};
pub use view::{
    DeploymentViewRow, DiagnosticViewRow, EndpointViewRow, LinkViewRow, LogViewRow,
    OperationViewRow, OperationWorkbenchView, OrchestratorView, OrchestratorViewPage,
    ReleaseRegistryViewRow, ServiceViewRow, TemplateViewRow, endpoint_hosts, ensure_view_is_loaded,
    load_orchestrator_view, load_orchestrator_view_from_store,
    load_orchestrator_view_with_database_url, merge_operation_workbench_session_into_view,
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

#[cfg(test)]
mod tests;
