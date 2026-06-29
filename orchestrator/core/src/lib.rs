mod action;
mod database;
mod executor;
mod health;
mod model;
mod observability;
mod planner;
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
pub use executor::{
    DockerComposeDriver, DriverRequest, DriverResult, ExecutionDriver, ExternalEndpointDriver,
    LocalProcessDriver, driver_request_for_endpoint,
};
pub use health::{
    EndpointHealthResult, EndpointProbe, LinkHealthResult, StaticEndpointProbe, TcpEndpointProbe,
    check_endpoint_health_with_probe, check_link_health,
};
pub use model::{
    DiagnosticFinding, DiagnosticReport, Endpoint, Link, LogView, Operation, OperationLock,
    OperationLogRecord, OperationStatus, Topology, TopologyAuthority, TopologySnapshot,
    build_topology, cancel_operation, confirm_operation, diagnostic_report_json, expire_operation,
    fail_operation, operation_log_record, operation_step_log_record, plan_operation,
    redact_secret_text, rollback_operation, start_operation, succeed_operation, topology_authority,
    validate_endpoint, validate_link, validate_topology,
};
pub use observability::{
    DiagnosticExport, LogQuery, LogQueryResult, build_diagnostic_report, export_diagnostic_report,
    query_logs, validate_log_view,
};
pub use planner::{
    ActionPlanPreview, ActionRequest, default_action_request, plan_action_preview,
    plan_action_request, preview_operation,
};
pub use schema::{
    ActionFormSchema, FormFieldSchema, SharedSchemas, ensure_shared_schemas_loaded,
    load_shared_schemas,
};
pub use service::{
    EndpointDecl, RuntimeMode, ServiceHealthDecl, ServiceManifest, ServiceProvides,
    ServiceRequires, ServiceRuntimeDecl, ServiceSecurityDecl, ServiceSet, ServiceSetEndpoint,
    ServiceSetLink, ServiceSetService, ServiceSetServiceSpec, ServiceUiDecl, SetExpandResult,
    SourceDecl, endpoint_delete_operation, endpoint_health_check_operation,
    endpoint_register_operation, endpoint_update_operation, expand_set, link_create_operation,
    link_delete_operation, link_health_check_operation, link_update_operation,
    service_health_check_operation, service_install_operation, service_lifecycle_operation,
    service_logs_view_operation, set_apply_operation, topology_apply_operation,
    validate_endpoint_id, validate_service_manifest, validate_service_manifest_file,
    validate_service_set, validate_service_set_file, validate_service_set_references,
};
pub use store::{MemoryOrchestratorStore, OperationExecutor, OrchestratorStore};
pub use view::{
    DiagnosticViewRow, EndpointViewRow, LinkViewRow, LogViewRow, OperationViewRow,
    OperationWorkbenchView, OrchestratorView, OrchestratorViewPage, ServiceViewRow, SetViewRow,
    endpoint_hosts, ensure_view_is_loaded, load_orchestrator_view,
    load_orchestrator_view_from_store,
};
pub use workbench::{
    OperationWorkbench, OperationWorkbenchContext, OperationWorkbenchRun,
    OperationWorkbenchSession, apply_operation_workbench_session, build_operation_workbench,
    build_operation_workbench_from_request, confirm_operation_workbench_session,
    load_operation_workbench_context, new_operation_workbench_session,
    rollback_operation_workbench_session, run_operation_workbench_flow,
    update_operation_workbench_field,
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
