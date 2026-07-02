use crate::{
    ActionDescriptor, ActionPlanMode, DeploymentTemplate, Endpoint, Link, Operation,
    OrchestratorError, RenderedServiceConfig, Result, ServiceFrontendEntry, ServiceManifest,
    ServiceMigrationRecord, ServicePermissionRecord, ServiceRedisResource, ServiceReleaseManifest,
    ServiceRoute, ServiceStorageResource, Topology, action_descriptor, diagnostic_export_operation,
    endpoint_create_operation, endpoint_delete_operation, endpoint_health_check_operation,
    endpoint_update_operation, link_create_operation, link_delete_operation,
    link_health_check_operation, link_update_operation, log_create_operation, log_query_operation,
    parse_endpoint_id, plan_operation, release_create_operation, release_delete_operation,
    release_install_operation, release_install_operation_with_release, release_rollback_operation,
    release_update_operation, service_health_check_operation, service_lifecycle_operation,
    topology_apply_operation, validate_endpoint_id, validate_endpoint_service_name,
};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ActionRequest {
    pub operation_id: String,
    pub action: String,
    #[serde(default)]
    pub fields: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ActionPlanPreview {
    pub operation_id: String,
    pub action: String,
    pub target_type: String,
    pub target_id: String,
    pub risk: String,
    pub mode: String,
    pub plan_required: String,
    pub requires_confirmation: bool,
    pub steps: Vec<String>,
    pub rollback_available: bool,
}

impl ActionRequest {
    pub fn new(
        operation_id: impl Into<String>,
        action: impl Into<String>,
        fields: BTreeMap<String, String>,
    ) -> Self {
        Self {
            operation_id: operation_id.into(),
            action: action.into(),
            fields,
        }
    }

    pub fn field(&self, name: &str) -> Option<&str> {
        self.fields
            .get(name)
            .map(String::as_str)
            .filter(|value| !value.trim().is_empty())
    }

    pub fn require_field(&self, name: &str) -> Result<&str> {
        self.field(name).ok_or_else(|| {
            OrchestratorError::InvalidManifest(format!(
                "{} requires form field {}",
                self.action, name
            ))
        })
    }
}

pub fn default_action_request(action: &str) -> Option<ActionRequest> {
    let fields = match action {
        "release.create" | "release.update" => map([
            ("service_id", "gateway"),
            ("release_url", "services/gateway"),
            ("service_yaml_path", "services/gateway/service.yaml"),
        ]),
        "release.list" => map([("service_id", "gateway")]),
        "release.get" | "release.validate" => map([("service_id", "gateway")]),
        "release.delete" => map([("service_id", "gateway"), ("confirm", "true")]),
        "release.install" => map([("service_id", "gateway"), ("confirm", "true")]),
        "release.rollback" => map([
            ("service_id", "gateway"),
            ("version", "0.1.0"),
            ("confirm", "true"),
        ]),
        "host.create" | "host.update" | "host.get" | "host.health.check" => {
            map([("host_ip", "127.0.0.1")])
        }
        "host.list" => BTreeMap::new(),
        "host.delete" => map([("host_ip", "127.0.0.1"), ("confirm", "true")]),
        "service.create" | "service.update" | "service.get" => map([("service_id", "gateway")]),
        "service.list" => BTreeMap::new(),
        "service.enable" | "service.disable" | "service.start" | "service.stop"
        | "service.restart" => map([("service_id", "gateway"), ("confirm", "true")]),
        "service.delete" => map([("service_id", "gateway"), ("confirm", "true")]),
        "service.health.check" => map([
            ("service_id", "gateway"),
            ("endpoint", "127.0.0.1:8080:gateway"),
        ]),
        "endpoint.create" => map([
            ("endpoint", "127.0.0.1:8080:gateway"),
            ("service_id", "gateway"),
            ("protocol", "http"),
            ("health_path", "/health"),
        ]),
        "endpoint.list" => BTreeMap::new(),
        "endpoint.get" | "endpoint.update" | "endpoint.health.check" => {
            map([("endpoint", "127.0.0.1:8080:gateway")])
        }
        "endpoint.delete" => map([("endpoint", "127.0.0.1:8080:gateway"), ("confirm", "true")]),
        "link.create" | "link.update" | "link.health.check" | "link.get" => map([
            ("source_endpoint", "127.0.0.1:8080:gateway"),
            ("target_endpoint", "127.0.0.1:8083:problem-service"),
            ("protocol", "http"),
            ("auth_mode", "internal"),
        ]),
        "link.list" => BTreeMap::new(),
        "link.delete" => map([
            ("source_endpoint", "127.0.0.1:8080:gateway"),
            ("target_endpoint", "127.0.0.1:8083:problem-service"),
            ("confirm", "true"),
        ]),
        "route.create" | "route.update" => map([
            ("route_id", "gateway-auth"),
            ("service_id", "gateway"),
            ("path", "/api/auth/**"),
            ("target", "auth-service[*]"),
        ]),
        "route.list" | "route.validate" => map([("service_id", "gateway")]),
        "route.get" => map([("route_id", "gateway-auth")]),
        "route.delete" => map([("route_id", "gateway-auth"), ("confirm", "true")]),
        "route.apply" => map([
            ("gateway_endpoint", "127.0.0.1:8080:gateway"),
            ("confirm", "true"),
        ]),
        "frontend.create" | "frontend.update" => map([
            ("frontend_id", "gateway-shell"),
            ("service_id", "gateway"),
            ("route_prefix", "/"),
        ]),
        "frontend.list" | "frontend.validate" => map([("service_id", "gateway")]),
        "frontend.get" => map([("frontend_id", "gateway-shell")]),
        "frontend.delete" => map([("frontend_id", "gateway-shell"), ("confirm", "true")]),
        "frontend.publish" => map([
            ("gateway_endpoint", "127.0.0.1:8080:gateway"),
            ("confirm", "true"),
        ]),
        "migration.create" | "migration.update" => map([
            ("migration_id", "gateway-0001"),
            ("service_id", "gateway"),
            ("version", "0001"),
        ]),
        "migration.list" | "migration.validate" => map([("service_id", "gateway")]),
        "migration.get" => map([("migration_id", "gateway-0001")]),
        "migration.delete" => map([("migration_id", "gateway-0001"), ("confirm", "true")]),
        "migration.apply" => map([("service_id", "gateway"), ("confirm", "true")]),
        "migration.rollback" => map([
            ("service_id", "gateway"),
            ("migration_id", "gateway-0001"),
            ("confirm", "true"),
        ]),
        "permission.create" | "permission.update" => {
            map([("permission_id", "gateway.read"), ("service_id", "gateway")])
        }
        "permission.list" | "permission.validate" => map([("service_id", "gateway")]),
        "permission.get" => map([("permission_id", "gateway.read")]),
        "permission.delete" => map([("permission_id", "gateway.read"), ("confirm", "true")]),
        "permission.sync" => map([("service_id", "gateway"), ("confirm", "true")]),
        "redis.create" | "redis.update" | "storage.create" | "storage.update" => map([
            ("resource_id", "gateway-resource"),
            ("service_id", "gateway"),
        ]),
        "redis.list" | "redis.validate" | "storage.list" | "storage.validate" => {
            map([("service_id", "gateway")])
        }
        "redis.get" | "storage.get" => map([("resource_id", "gateway-resource")]),
        "redis.delete" | "storage.delete" => {
            map([("resource_id", "gateway-resource"), ("confirm", "true")])
        }
        "redis.apply" | "storage.apply" => map([("service_id", "gateway"), ("confirm", "true")]),
        "config.create" | "config.update" => map([
            ("config_id", "gateway-default"),
            ("service_id", "gateway"),
            ("config", "{}"),
        ]),
        "config.list" | "config.render" | "config.validate" => map([("service_id", "gateway")]),
        "config.get" => map([("config_id", "gateway-default")]),
        "config.delete" => map([("config_id", "gateway-default"), ("confirm", "true")]),
        "secret.create" | "secret.update" => map([
            ("secret_id", "secret://gateway/default"),
            ("confirm", "true"),
        ]),
        "secret.list" => map([("service_id", "gateway")]),
        "secret.get" => map([("secret_id", "secret://gateway/default")]),
        "secret.delete" => map([
            ("secret_id", "secret://gateway/default"),
            ("confirm", "true"),
        ]),
        "secret.distribute" => map([
            ("secret_id", "secret://gateway/default"),
            ("endpoint", "127.0.0.1:8080:gateway"),
            ("confirm", "true"),
        ]),
        "topology.create" => map([
            ("root_endpoint", "127.0.0.1:8080:gateway"),
            ("confirm", "true"),
        ]),
        "topology.list" | "topology.get" | "topology.validate" => BTreeMap::new(),
        "topology.update" | "topology.delete" => {
            map([("topology_snapshot_id", "current"), ("confirm", "true")])
        }
        "topology.apply" => map([("topology_snapshot_id", "current"), ("confirm", "true")]),
        "topology.export" => map([("format", "json")]),
        "operation.create" => map([
            ("action", "release.install"),
            ("target_type", "ServiceRelease"),
            ("target_id", "gateway"),
        ]),
        "operation.list" => BTreeMap::new(),
        "operation.get" | "operation.confirm" | "operation.cancel" | "log.query" => {
            map([("operation_id", "op-sample")])
        }
        "operation.update" => map([("operation_id", "op-sample"), ("note", "updated")]),
        "operation.delete" => map([("operation_id", "op-sample"), ("confirm", "true")]),
        "operation.apply" | "operation.rollback" => {
            map([("operation_id", "op-sample"), ("confirm", "true")])
        }
        "log.create" | "log.update" | "log.get" => map([
            ("source_id", "log-sample"),
            ("service_id", "gateway"),
            ("endpoint", "127.0.0.1:8080:gateway"),
        ]),
        "log.list" => BTreeMap::new(),
        "log.delete" => map([("source_id", "log-sample"), ("confirm", "true")]),
        "diagnostic.create" => map([("target_type", "Topology"), ("target_id", "current")]),
        "diagnostic.list" => BTreeMap::new(),
        "diagnostic.get" | "diagnostic.update" => map([("report_id", "diag-sample")]),
        "diagnostic.delete" => map([("report_id", "diag-sample"), ("confirm", "true")]),
        "diagnostic.export" => map([("report_id", "diag-sample"), ("format", "json")]),
        _ => return None,
    };
    Some(ActionRequest::new(
        format!("preview-{}", action.replace('.', "-")),
        action,
        fields,
    ))
}

pub fn plan_action_request(
    request: &ActionRequest,
    services: &[ServiceManifest],
    _sets: &[DeploymentTemplate],
    endpoints: &[Endpoint],
    topology: Option<&Topology>,
) -> Result<Operation> {
    plan_action_request_with_releases(request, services, &[], _sets, endpoints, topology)
}

pub fn plan_action_request_with_releases(
    request: &ActionRequest,
    services: &[ServiceManifest],
    releases: &[ServiceReleaseManifest],
    _sets: &[DeploymentTemplate],
    endpoints: &[Endpoint],
    topology: Option<&Topology>,
) -> Result<Operation> {
    let descriptor = action_descriptor(&request.action).ok_or_else(|| {
        OrchestratorError::InvalidManifest(format!("unknown action {}", request.action))
    })?;
    validate_request_endpoint_fields(request)?;
    match request.action.as_str() {
        "release.create" => {
            let service_id = request.require_field("service_id")?;
            let release = find_release(releases, service_id)?;
            release_create_operation(&request.operation_id, release, request.field("release_url"))
        }
        "release.update" => {
            let service_id = request.require_field("service_id")?;
            let release = find_release(releases, service_id)?;
            release_update_operation(&request.operation_id, release, request.field("release_url"))
        }
        "release.install" => {
            let service_id = request.require_field("service_id")?;
            let manifest = find_service(services, service_id)?;
            let installed = services
                .iter()
                .map(|service| service.id.clone())
                .collect::<Vec<_>>();
            if releases.is_empty() {
                release_install_operation(&request.operation_id, manifest, &installed)
            } else {
                let release = release_with_request_source_overrides(
                    find_release(releases, service_id)?.clone(),
                    request,
                );
                release_install_operation_with_release(
                    &request.operation_id,
                    manifest,
                    Some(&release),
                    &installed,
                    request.field("host_ip").unwrap_or("127.0.0.1"),
                    request.field("endpoint"),
                    release_install_options(request),
                )
            }
        }
        "release.delete" => release_delete_operation(
            &request.operation_id,
            request.require_field("service_id")?,
            request.field("version"),
        ),
        "release.rollback" => release_rollback_operation(
            &request.operation_id,
            request.require_field("service_id")?,
            request.field("version"),
            request
                .field("target_operation_id")
                .or_else(|| request.field("operation_id")),
        ),
        "service.enable" | "service.disable" | "service.start" | "service.stop"
        | "service.restart" | "service.delete" => service_lifecycle_operation(
            &request.operation_id,
            &request.action,
            request.require_field("service_id")?,
        ),
        "service.health.check" => service_health_check_operation(
            &request.operation_id,
            request.field("service_id").unwrap_or("all-services"),
            request.field("endpoint"),
        ),
        "endpoint.create" => {
            let endpoint = endpoint_from_request(request, true, endpoints)?;
            endpoint_create_operation(&request.operation_id, &endpoint)
        }
        "endpoint.update" => {
            let endpoint = endpoint_from_request(request, false, endpoints)?;
            endpoint_update_operation(&request.operation_id, &endpoint)
        }
        "endpoint.delete" => {
            endpoint_delete_operation(&request.operation_id, request.require_field("endpoint")?)
        }
        "endpoint.health.check" => endpoint_health_check_operation(
            &request.operation_id,
            request.require_field("endpoint")?,
        ),
        "link.create" => {
            let link = link_from_request(request)?;
            link_create_operation(&request.operation_id, &link, endpoints)
        }
        "link.update" => {
            let link = link_from_request(request)?;
            link_update_operation(&request.operation_id, &link, endpoints)
        }
        "link.delete" => {
            let link = link_from_request(request)?;
            link_delete_operation(&request.operation_id, &link)
        }
        "link.health.check" => {
            let link = link_from_request(request)?;
            link_health_check_operation(&request.operation_id, &link)
        }
        "route.create" | "route.update" => {
            let route = route_from_request(request)?;
            registry_resource_operation(
                request,
                descriptor,
                route_id(&route),
                "upsert",
                "service_route",
                serde_json::to_value(route)?,
            )
        }
        "route.delete" => registry_resource_delete_operation(
            request,
            descriptor,
            request.require_field("route_id")?,
            "service_route",
        ),
        "frontend.create" | "frontend.update" => {
            let frontend = frontend_from_request(request)?;
            registry_resource_operation(
                request,
                descriptor,
                frontend_id(&frontend),
                "upsert",
                "service_frontend",
                serde_json::to_value(frontend)?,
            )
        }
        "frontend.delete" => registry_resource_delete_operation(
            request,
            descriptor,
            request.require_field("frontend_id")?,
            "service_frontend",
        ),
        "migration.create" | "migration.update" => {
            let migration = migration_from_request(request)?;
            registry_resource_operation(
                request,
                descriptor,
                migration_id(&migration),
                "upsert",
                "service_migration",
                serde_json::to_value(migration)?,
            )
        }
        "migration.delete" => registry_resource_delete_operation(
            request,
            descriptor,
            request.require_field("migration_id")?,
            "service_migration",
        ),
        "permission.create" | "permission.update" => {
            let permission = permission_from_request(request)?;
            registry_resource_operation(
                request,
                descriptor,
                permission.permission_key.clone(),
                "upsert",
                "service_permission",
                serde_json::to_value(permission)?,
            )
        }
        "permission.delete" => registry_resource_delete_operation(
            request,
            descriptor,
            request.require_field("permission_id")?,
            "service_permission",
        ),
        "redis.create" | "redis.update" => {
            let redis = redis_from_request(request)?;
            registry_resource_operation(
                request,
                descriptor,
                redis_id(&redis),
                "upsert",
                "service_redis",
                serde_json::to_value(redis)?,
            )
        }
        "redis.delete" => registry_resource_delete_operation(
            request,
            descriptor,
            request.require_field("resource_id")?,
            "service_redis",
        ),
        "storage.create" | "storage.update" => {
            let storage = storage_from_request(request)?;
            registry_resource_operation(
                request,
                descriptor,
                storage_id(&storage),
                "upsert",
                "service_storage",
                serde_json::to_value(storage)?,
            )
        }
        "storage.delete" => registry_resource_delete_operation(
            request,
            descriptor,
            request.require_field("resource_id")?,
            "service_storage",
        ),
        "config.create" | "config.update" => {
            let config = config_from_request(request)?;
            registry_resource_operation(
                request,
                descriptor,
                config_id(&config),
                "upsert",
                "rendered_config",
                serde_json::to_value(config)?,
            )
        }
        "config.delete" => registry_resource_delete_operation(
            request,
            descriptor,
            request.require_field("config_id")?,
            "rendered_config",
        ),
        "topology.apply" => {
            let topology = topology.ok_or_else(|| {
                OrchestratorError::InvalidManifest(
                    "topology.apply requires current topology".to_string(),
                )
            })?;
            topology_apply_operation(&request.operation_id, topology)
        }
        "log.query" => log_query_operation(
            &request.operation_id,
            request.require_field("operation_id")?,
        ),
        "log.create" => log_create_operation(
            &request.operation_id,
            request.require_field("service_id")?,
            request.field("endpoint"),
        ),
        "diagnostic.export" => diagnostic_export_operation(
            &request.operation_id,
            request.require_field("report_id")?,
            request.field("format").unwrap_or("json"),
        ),
        _ => generic_action_operation(request, descriptor),
    }
}

fn registry_resource_operation(
    request: &ActionRequest,
    descriptor: &ActionDescriptor,
    target_id: String,
    verb: &str,
    resource_kind: &str,
    resource: Value,
) -> Result<Operation> {
    let mut request_value = request_value(request);
    request_value
        .as_object_mut()
        .expect("request_value builds a JSON object")
        .insert("resource".to_string(), resource);
    plan_operation(
        &request.operation_id,
        descriptor.action,
        descriptor.target_type,
        &target_id,
        request_value,
        serde_json::json!({
            "steps": [
                {
                    "action": verb,
                    "target": target_id,
                    "resource_kind": resource_kind,
                    "detail": descriptor.summary
                }
            ],
            "requires_confirmation": descriptor.plan_mode.requires_confirmation()
        }),
        serde_json::json!({
            "steps": [
                {
                    "action": "restore_previous_registry_state",
                    "target": target_id,
                    "resource_kind": resource_kind
                }
            ]
        }),
    )
}

fn release_install_options(request: &ActionRequest) -> Value {
    serde_json::json!({
        "migration_dry_run": request.field("migration_dry_run").is_some_and(truthy_field),
        "allow_destructive_migrations": request
            .field("allow_destructive_migrations")
            .is_some_and(truthy_field),
        "execute_service_driver": request.field("execute_service_driver").is_some_and(truthy_field),
        "external_service_running": request
            .field("external_service_running")
            .or_else(|| request.field("existing_endpoint_running"))
            .is_some_and(truthy_field),
        "gateway_node_id": request.field("gateway_node_id").unwrap_or(""),
        "release_url": request
            .field("release_url")
            .or_else(|| request.field("source_url"))
            .unwrap_or(""),
        "release_checksum": request.field("release_checksum").unwrap_or(""),
    })
}

fn truthy_field(value: &str) -> bool {
    matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "1" | "true" | "yes" | "on"
    )
}

fn release_with_request_source_overrides(
    mut release: ServiceReleaseManifest,
    request: &ActionRequest,
) -> ServiceReleaseManifest {
    if let Some(source_url) = request
        .field("release_url")
        .or_else(|| request.field("source_url"))
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        release.source.url = source_url.to_string();
    }
    release
}

fn registry_resource_delete_operation(
    request: &ActionRequest,
    descriptor: &ActionDescriptor,
    target_id: &str,
    resource_kind: &str,
) -> Result<Operation> {
    plan_operation(
        &request.operation_id,
        descriptor.action,
        descriptor.target_type,
        target_id,
        request_value(request),
        serde_json::json!({
            "steps": [
                {
                    "action": "delete",
                    "target": target_id,
                    "resource_kind": resource_kind,
                    "detail": descriptor.summary
                }
            ],
            "requires_confirmation": descriptor.plan_mode.requires_confirmation()
        }),
        serde_json::json!({
            "steps": [
                {
                    "action": "restore_previous_registry_state",
                    "target": target_id,
                    "resource_kind": resource_kind
                }
            ]
        }),
    )
}

pub fn preview_operation(
    operation: &Operation,
    descriptor: &ActionDescriptor,
) -> ActionPlanPreview {
    ActionPlanPreview {
        operation_id: operation.operation_id.clone(),
        action: operation.action.clone(),
        target_type: operation.target_type.clone(),
        target_id: operation.target_id.clone(),
        risk: descriptor.risk_label().to_string(),
        mode: descriptor.mode_label().to_string(),
        plan_required: descriptor.plan_requirement().to_string(),
        requires_confirmation: operation_requires_confirmation(operation, descriptor.plan_mode),
        steps: operation_step_names(operation),
        rollback_available: operation
            .rollback_plan
            .get("steps")
            .and_then(Value::as_array)
            .is_some_and(|steps| !steps.is_empty()),
    }
}

pub fn plan_action_preview(
    request: &ActionRequest,
    services: &[ServiceManifest],
    sets: &[DeploymentTemplate],
    endpoints: &[Endpoint],
    topology: Option<&Topology>,
) -> Result<ActionPlanPreview> {
    plan_action_preview_with_releases(request, services, &[], sets, endpoints, topology)
}

pub fn plan_action_preview_with_releases(
    request: &ActionRequest,
    services: &[ServiceManifest],
    releases: &[ServiceReleaseManifest],
    sets: &[DeploymentTemplate],
    endpoints: &[Endpoint],
    topology: Option<&Topology>,
) -> Result<ActionPlanPreview> {
    let descriptor = action_descriptor(&request.action).ok_or_else(|| {
        OrchestratorError::InvalidManifest(format!("unknown action {}", request.action))
    })?;
    let operation =
        plan_action_request_with_releases(request, services, releases, sets, endpoints, topology)?;
    Ok(preview_operation(&operation, descriptor))
}

fn validate_request_endpoint_fields(request: &ActionRequest) -> Result<()> {
    for field in [
        "endpoint",
        "source_endpoint",
        "target_endpoint",
        "gateway_endpoint",
        "root_endpoint",
    ] {
        if let Some(endpoint) = request.field(field) {
            validate_endpoint_id(endpoint)?;
        }
    }
    if let (Some(endpoint), Some(service_id)) =
        (request.field("endpoint"), request.field("service_id"))
    {
        validate_endpoint_service_name(endpoint, service_id)?;
    }
    Ok(())
}

fn generic_action_operation(
    request: &ActionRequest,
    descriptor: &ActionDescriptor,
) -> Result<Operation> {
    let target_id = request
        .field("target_id")
        .or_else(|| request.field("service_id"))
        .or_else(|| request.field("endpoint"))
        .or_else(|| request.field("operation_id"))
        .or_else(|| request.field("report_id"))
        .or_else(|| request.field("topology_snapshot_id"))
        .unwrap_or("current");
    let request_value = request_value(request);
    let step = match descriptor.plan_mode {
        ActionPlanMode::ReadOnly => "read",
        ActionPlanMode::Direct => "execute",
        ActionPlanMode::Planned | ActionPlanMode::ConfirmedPlan => "plan",
    };
    plan_operation(
        &request.operation_id,
        descriptor.action,
        descriptor.target_type,
        target_id,
        request_value,
        serde_json::json!({
            "steps": [
                {
                    "action": step,
                    "target": target_id,
                    "detail": descriptor.summary
                }
            ],
            "requires_confirmation": descriptor.plan_mode.requires_confirmation()
        }),
        serde_json::json!({
            "steps": []
        }),
    )
}

fn route_from_request(request: &ActionRequest) -> Result<ServiceRoute> {
    let path = request
        .field("path")
        .or_else(|| request.field("route_id"))
        .unwrap_or("/api/**")
        .to_string();
    let method = request
        .field("method")
        .unwrap_or("ANY")
        .to_ascii_uppercase();
    let target = request.field("target").unwrap_or("gateway[*]");
    let target_type = request
        .field("target_type")
        .unwrap_or_else(|| route_target_type(target))
        .to_string();
    Ok(ServiceRoute {
        path,
        method,
        target_type: target_type.clone(),
        target_service_name: request
            .field("target_service_name")
            .map(str::to_string)
            .unwrap_or_else(|| route_target_service_name(target)),
        target_selector: route_target_selector(&target_type, target),
        permission: request.field("permission").unwrap_or("").to_string(),
        enabled: request
            .field("enabled")
            .is_none_or(|value| value.eq_ignore_ascii_case("true")),
        created_at: String::new(),
        updated_at: String::new(),
    })
}

fn frontend_from_request(request: &ActionRequest) -> Result<ServiceFrontendEntry> {
    Ok(ServiceFrontendEntry {
        service_name: request
            .field("service_id")
            .or_else(|| request.field("frontend_id"))
            .unwrap_or("gateway")
            .to_string(),
        enabled: request
            .field("enabled")
            .is_none_or(|value| value.eq_ignore_ascii_case("true")),
        route_prefix: request.field("route_prefix").unwrap_or("/").to_string(),
        remote_entry: request.field("remote_entry").unwrap_or("").to_string(),
        menu_items: json_array_string_field(request, "menu_items")?,
        created_at: String::new(),
        updated_at: String::new(),
    })
}

fn migration_from_request(request: &ActionRequest) -> Result<ServiceMigrationRecord> {
    Ok(ServiceMigrationRecord {
        service_name: request.field("service_id").unwrap_or("gateway").to_string(),
        migration_version: request
            .field("version")
            .or_else(|| request.field("migration_id"))
            .unwrap_or("0001")
            .to_string(),
        checksum: request.field("checksum").unwrap_or("").to_string(),
        status: request.field("status").unwrap_or("registered").to_string(),
        applied_at: request.field("applied_at").unwrap_or("").to_string(),
        created_at: String::new(),
        updated_at: String::new(),
    })
}

fn permission_from_request(request: &ActionRequest) -> Result<ServicePermissionRecord> {
    Ok(ServicePermissionRecord {
        service_name: request.field("service_id").unwrap_or("gateway").to_string(),
        permission_key: request.require_field("permission_id")?.to_string(),
        source: request.field("source").unwrap_or("manual").to_string(),
        created_at: String::new(),
        updated_at: String::new(),
    })
}

fn redis_from_request(request: &ActionRequest) -> Result<ServiceRedisResource> {
    Ok(ServiceRedisResource {
        service_name: request.field("service_id").unwrap_or("gateway").to_string(),
        name: request.require_field("resource_id")?.to_string(),
        kind: request.field("kind").unwrap_or("stream").to_string(),
        usage: request.field("usage").unwrap_or("").to_string(),
        created_at: String::new(),
        updated_at: String::new(),
    })
}

fn storage_from_request(request: &ActionRequest) -> Result<ServiceStorageResource> {
    Ok(ServiceStorageResource {
        service_name: request.field("service_id").unwrap_or("gateway").to_string(),
        object_type: request.require_field("resource_id")?.to_string(),
        bucket: request.field("bucket").unwrap_or("ojos").to_string(),
        path_prefix: request.field("path_prefix").unwrap_or("").to_string(),
        created_at: String::new(),
        updated_at: String::new(),
    })
}

fn config_from_request(request: &ActionRequest) -> Result<RenderedServiceConfig> {
    Ok(RenderedServiceConfig {
        service_name: request.field("service_id").unwrap_or("gateway").to_string(),
        version: request.field("version").unwrap_or("default").to_string(),
        config: json_field(request, "config")?,
        created_at: String::new(),
        updated_at: String::new(),
    })
}

fn route_target_type(target: &str) -> &str {
    if target.ends_with("[*]") {
        "endpoint-group"
    } else if target.starts_with('/') {
        "frontend"
    } else {
        "endpoint"
    }
}

fn route_target_service_name(target: &str) -> String {
    if target.ends_with("[*]") {
        target.trim_end_matches("[*]").to_string()
    } else if let Ok(identity) = parse_endpoint_id(target) {
        identity.service_name.to_string()
    } else {
        target.trim_start_matches('/').to_string()
    }
}

fn route_target_selector(target_type: &str, target: &str) -> Value {
    match target_type {
        "endpoint" => serde_json::json!({ "endpoint": target }),
        "frontend" => serde_json::json!({ "frontend": target }),
        _ => serde_json::json!({ "group": target }),
    }
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

fn request_value(request: &ActionRequest) -> Value {
    Value::Object(
        request
            .fields
            .iter()
            .map(|(key, value)| (key.clone(), Value::String(value.clone())))
            .collect::<Map<_, _>>(),
    )
}

fn find_service<'a>(
    services: &'a [ServiceManifest],
    service_id: &str,
) -> Result<&'a ServiceManifest> {
    services
        .iter()
        .find(|service| service.id == service_id)
        .ok_or_else(|| {
            OrchestratorError::Dependency(format!("missing Service manifest {}", service_id))
        })
}

fn find_release<'a>(
    releases: &'a [ServiceReleaseManifest],
    service_id: &str,
) -> Result<&'a ServiceReleaseManifest> {
    releases
        .iter()
        .find(|release| release.service_name == service_id)
        .ok_or_else(|| {
            OrchestratorError::Dependency(format!("missing ServiceRelease manifest {}", service_id))
        })
}

fn endpoint_from_request(
    request: &ActionRequest,
    require_service_id: bool,
    endpoints: &[Endpoint],
) -> Result<Endpoint> {
    let endpoint = request.require_field("endpoint")?;
    validate_endpoint_id(endpoint)?;
    let endpoint_identity = parse_endpoint_id(endpoint)?;
    let current = endpoints.iter().find(|item| item.endpoint == endpoint);
    let service_id = if require_service_id {
        request.require_field("service_id")?.to_string()
    } else {
        request
            .field("service_id")
            .or_else(|| current.map(|item| item.service_id.as_str()))
            .unwrap_or(endpoint_identity.service_name)
            .to_string()
    };
    Ok(Endpoint {
        endpoint: endpoint.to_string(),
        service_id,
        protocol: request
            .field("protocol")
            .or_else(|| current.map(|item| item.protocol.as_str()))
            .unwrap_or("http")
            .to_string(),
        health_path: request
            .field("health_path")
            .or_else(|| current.map(|item| item.health_path.as_str()))
            .unwrap_or("")
            .to_string(),
        health: "unknown".to_string(),
        reachable: false,
        display_name: request
            .field("display_name")
            .or_else(|| current.map(|item| item.display_name.as_str()))
            .unwrap_or("")
            .to_string(),
        note: request
            .field("note")
            .or_else(|| current.map(|item| item.note.as_str()))
            .unwrap_or("")
            .to_string(),
        config: json_field(request, "config")?,
        created_at: String::new(),
        updated_at: String::new(),
    })
}

fn link_from_request(request: &ActionRequest) -> Result<Link> {
    let source_endpoint = request.require_field("source_endpoint")?;
    let target_endpoint = request.require_field("target_endpoint")?;
    validate_endpoint_id(source_endpoint)?;
    validate_endpoint_id(target_endpoint)?;
    Ok(Link {
        source_endpoint: source_endpoint.to_string(),
        target_endpoint: target_endpoint.to_string(),
        protocol: request.field("protocol").unwrap_or("http").to_string(),
        auth_mode: request.field("auth_mode").unwrap_or("internal").to_string(),
        scope: request.field("scope").unwrap_or("").to_string(),
        health: "unknown".to_string(),
        latency_ms: None,
        config_ref: request.field("config_ref").unwrap_or("").to_string(),
        secret_ref: request.field("secret_ref").unwrap_or("").to_string(),
        policy: json_field(request, "policy")?,
        created_at: String::new(),
        updated_at: String::new(),
    })
}

fn operation_requires_confirmation(operation: &Operation, plan_mode: ActionPlanMode) -> bool {
    operation
        .plan
        .get("requires_confirmation")
        .and_then(Value::as_bool)
        .unwrap_or_else(|| plan_mode.requires_confirmation())
}

fn operation_step_names(operation: &Operation) -> Vec<String> {
    operation
        .plan
        .get("steps")
        .and_then(Value::as_array)
        .map(|steps| {
            steps
                .iter()
                .filter_map(|step| {
                    step.get("action")
                        .and_then(Value::as_str)
                        .map(str::to_string)
                })
                .collect()
        })
        .unwrap_or_default()
}

fn json_field(request: &ActionRequest, name: &str) -> Result<Value> {
    match request.field(name) {
        Some(value) => serde_json::from_str(value).map_err(|err| {
            OrchestratorError::InvalidManifest(format!(
                "{} field {} must be valid JSON: {err}",
                request.action, name
            ))
        }),
        None => Ok(Value::Null),
    }
}

fn json_array_string_field(request: &ActionRequest, name: &str) -> Result<Vec<String>> {
    match request.field(name) {
        Some(value) => serde_json::from_str(value).map_err(|err| {
            OrchestratorError::InvalidManifest(format!(
                "{} field {} must be a JSON string array: {err}",
                request.action, name
            ))
        }),
        None => Ok(Vec::new()),
    }
}

fn map<const N: usize>(items: [(&str, &str); N]) -> BTreeMap<String, String> {
    items
        .into_iter()
        .map(|(key, value)| (key.to_string(), value.to_string()))
        .collect()
}
