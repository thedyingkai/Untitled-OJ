//! Pure domain kernel for Orchestrator v1.
//!
//! This crate owns serializable domain types, validation, deterministic plans,
//! state transitions and the published action contract. Runtime, persistence,
//! network, filesystem and compatibility adapters are intentionally kept in
//! `orchestrator-legacy` or the dedicated v1 infrastructure crates.

mod action;
mod api_binding;
mod contract_v1;
mod model;
mod planner;
mod schema;
mod service;
mod service_contract_v2;
mod service_openapi;
pub mod topology_v1;

pub use action::{
    ACTION_CATALOG, ActionDescriptor, ActionPlanMode, ActionRisk, CORE_ACTION_TARGETS,
    FORMAL_ACTION_PREFIXES, action_catalog, action_descriptor, validate_action_catalog,
};
pub use api_binding::{
    ApiBinding, ApiBindingResolutionError, ApiBindingResolutionRequest, ApiBindingState,
    ApiBindingValidationError, ApiProviderCandidate, api_version_matches,
    resolve_api_binding_candidate,
};
pub use contract_v1::{V1_ACTIONS, V1ActionDescriptor, V1Role, v1_action};
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
pub use planner::{
    ActionPlanPreview, ActionRequest, default_action_request, plan_action_preview,
    plan_action_preview_with_releases, plan_action_request, plan_action_request_with_releases,
    preview_operation,
};
pub use schema::{ActionFormSchema, FormFieldSchema, SharedSchemas, ensure_shared_schemas_loaded};
pub use service::{
    DeploymentTemplate, DeploymentTemplateEndpoint, DeploymentTemplateLink,
    DeploymentTemplateOperations, DeploymentTemplatePreview, DeploymentTemplateService,
    DeploymentTemplateServiceSpec, EndpointDecl, EndpointIdentity, LINK_PROBE_V1_CAPABILITY,
    ReleaseApiSurfaceDecl, ReleaseBackendDecl, ReleaseFrontendDecl, ReleaseMigrationDecl,
    ReleaseObservabilityDecl, ReleaseRedisDecl, ReleaseRouteDecl, ReleaseRuntimeDecl,
    ReleaseServiceIdentityDecl, ReleaseSourceDecl, ReleaseStorageDecl, RuntimeMode,
    ServiceHealthDecl, ServiceManifest, ServiceProvides, ServiceReleaseManifest, ServiceRequires,
    ServiceRuntimeDecl, ServiceSecurityDecl, ServiceUiDecl, SourceDecl,
    diagnostic_export_operation, endpoint_create_operation, endpoint_delete_operation,
    endpoint_health_check_operation, endpoint_socket_addr, endpoint_update_operation,
    host_lifecycle_operation, link_create_operation, link_delete_operation,
    link_health_check_operation, link_toggle_operation, link_update_operation,
    log_create_operation, log_query_operation, parse_endpoint_id, preview_deployment_template,
    release_create_operation, release_delete_operation, release_install_operation,
    release_install_operation_with_release, release_rollback_operation,
    release_supports_link_probe_v1, release_update_operation, service_health_check_operation,
    service_lifecycle_operation, service_lifecycle_operation_with_release,
    topology_apply_operation, validate_deployment_template, validate_endpoint_id,
    validate_endpoint_service_name, validate_service_manifest, validate_service_release,
};
pub use service_contract_v2::{
    ReleaseApiAuthDecl, ReleaseEventContractDecl, ReleaseEventDecl, ReleaseEventsContract,
    ReleaseProvidedApiContractDecl, ReleaseProvidedApiDecl, ReleaseProvidesContract,
    ReleaseRequiredApiBindingDecl, ReleaseRequiredApiDecl, ReleaseRequiredEventContractDecl,
    ReleaseRequiredEventDecl, ReleaseRequiresContract, ReleaseRuntimeContractDecl,
    SERVICE_CONTRACT_VERSION, STANDARD_CONTAINER_RUNTIME_ID, STANDARD_CONTAINER_RUNTIME_SHA256,
    ServiceReleaseContract,
};
pub use service_openapi::{
    ServiceOpenApiLintError, ServiceOpenApiLintReport, ServiceOpenApiOperation,
    lint_service_openapi_value, lint_service_openapi_yaml,
};
pub use topology_v1::{
    TOPOLOGY_SPEC_VERSION, TopologyApiBindingSpec, TopologyAuthoritySpec, TopologyChange,
    TopologyDeploymentStatus, TopologyDesiredDeploymentState, TopologyDiff, TopologyDrift,
    TopologyDriftKind, TopologyEndpointSpec, TopologyEndpointStatus, TopologyHealth,
    TopologyLinkSpec, TopologyLinkStatus, TopologyObservedDeploymentState,
    TopologyReconciliationState, TopologyResourceKind, TopologyRevision, TopologySpec,
    TopologyStatus, diff_topology_revisions, diff_topology_specs, rollback_topology_revision,
};

/// Stable record names used by domain diagnostics and legacy migration tools.
/// SQL statements and database clients are not part of this crate.
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
    #[error("yaml parse error: {0}")]
    Yaml(#[from] serde_yaml::Error),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
}

pub type Result<T> = std::result::Result<T, OrchestratorError>;

pub fn sanitize_path_for_error(path: &std::path::Path) -> String {
    path.file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("path")
        .to_string()
}
