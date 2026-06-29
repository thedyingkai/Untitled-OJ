use crate::{
    ActionDescriptor, ActionPlanMode, Endpoint, Link, Operation, OrchestratorError, Result,
    ServiceManifest, ServiceSet, Topology, action_descriptor, diagnostics_export_operation,
    endpoint_delete_operation, endpoint_health_check_operation, endpoint_register_operation,
    endpoint_update_operation, link_create_operation, link_delete_operation,
    link_health_check_operation, link_update_operation, operation_logs_view_operation,
    plan_operation, service_health_check_operation, service_install_operation,
    service_lifecycle_operation, service_logs_view_operation, set_apply_operation,
    topology_apply_operation, validate_endpoint_id,
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
        "deployment.create" => map([
            ("name", "default-topology"),
            ("root_endpoint", "127.0.0.1:8080"),
        ]),
        "deployment.open" => map([("deployment_id", "current")]),
        "deployment.diagnose" => map([("deployment_id", "current")]),
        "service.import" => map([
            ("import_kind", "local"),
            ("import_ref", "services/gateway"),
            ("service_yaml_path", "services/gateway/service.yaml"),
        ]),
        "service.validate" => map([("service_id", "gateway")]),
        "service.install" | "service.enable" | "service.disable" | "service.start"
        | "service.stop" | "service.restart" => map([("service_id", "gateway")]),
        "service.delete" => map([("service_id", "gateway"), ("confirm", "true")]),
        "service.logs.view" | "service.health.check" => {
            map([("service_id", "gateway"), ("endpoint", "127.0.0.1:8080")])
        }
        "set.import" => map([("set_yaml_path", "sets/single-node-oj.yaml")]),
        "set.validate" | "set.expand" => map([("set_id", "single-node-oj")]),
        "set.apply" => map([("set_id", "single-node-oj"), ("confirm", "true")]),
        "set.compare" => map([
            ("left_set_id", "single-node-oj"),
            ("right_set_id", "distributed-oj"),
        ]),
        "endpoint.register" => map([
            ("endpoint", "127.0.0.1:8080"),
            ("service_id", "gateway"),
            ("protocol", "http"),
            ("health_path", "/health"),
        ]),
        "endpoint.update" | "endpoint.health.check" => map([("endpoint", "127.0.0.1:8080")]),
        "endpoint.delete" => map([("endpoint", "127.0.0.1:8080"), ("confirm", "true")]),
        "link.create" | "link.update" | "link.health.check" => map([
            ("source_endpoint", "127.0.0.1:8080"),
            ("target_endpoint", "127.0.0.1:8081"),
            ("protocol", "http"),
            ("auth_mode", "internal"),
        ]),
        "link.delete" => map([
            ("source_endpoint", "127.0.0.1:8080"),
            ("target_endpoint", "127.0.0.1:8081"),
            ("confirm", "true"),
        ]),
        "topology.load" | "topology.validate" => BTreeMap::new(),
        "topology.apply" => map([("topology_snapshot_id", "current"), ("confirm", "true")]),
        "topology.export" => map([("format", "json")]),
        "operation.plan" => map([
            ("action", "service.install"),
            ("target_type", "Service"),
            ("target_id", "gateway"),
        ]),
        "operation.confirm" | "operation.cancel" | "operation.logs.view" => {
            map([("operation_id", "op-sample")])
        }
        "operation.apply" | "operation.rollback" => {
            map([("operation_id", "op-sample"), ("confirm", "true")])
        }
        "diagnostics.run" => map([("target_type", "Topology"), ("target_id", "current")]),
        "diagnostics.export" => map([("report_id", "diag-sample"), ("format", "json")]),
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
    sets: &[ServiceSet],
    endpoints: &[Endpoint],
    topology: Option<&Topology>,
) -> Result<Operation> {
    let descriptor = action_descriptor(&request.action).ok_or_else(|| {
        OrchestratorError::InvalidManifest(format!("unknown action {}", request.action))
    })?;
    match request.action.as_str() {
        "service.install" => {
            let service_id = request.require_field("service_id")?;
            let manifest = find_service(services, service_id)?;
            let installed = services
                .iter()
                .map(|service| service.id.clone())
                .collect::<Vec<_>>();
            service_install_operation(&request.operation_id, manifest, &installed)
        }
        "service.enable" | "service.disable" | "service.start" | "service.stop"
        | "service.restart" | "service.delete" => service_lifecycle_operation(
            &request.operation_id,
            &request.action,
            request.require_field("service_id")?,
        ),
        "service.logs.view" => service_logs_view_operation(
            &request.operation_id,
            request.field("service_id").unwrap_or("all-services"),
            request.field("endpoint"),
        ),
        "service.health.check" => service_health_check_operation(
            &request.operation_id,
            request.field("service_id").unwrap_or("all-services"),
            request.field("endpoint"),
        ),
        "set.apply" => {
            let set = find_set(sets, request.require_field("set_id")?)?;
            set_apply_operation(&request.operation_id, set)
        }
        "endpoint.register" => {
            let endpoint = endpoint_from_request(request, true, endpoints)?;
            endpoint_register_operation(&request.operation_id, &endpoint)
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
        "topology.apply" => {
            let topology = topology.ok_or_else(|| {
                OrchestratorError::InvalidManifest(
                    "topology.apply requires current topology".to_string(),
                )
            })?;
            topology_apply_operation(&request.operation_id, topology)
        }
        "operation.logs.view" => operation_logs_view_operation(
            &request.operation_id,
            request.require_field("operation_id")?,
        ),
        "diagnostics.export" => diagnostics_export_operation(
            &request.operation_id,
            request.require_field("report_id")?,
            request.field("format").unwrap_or("json"),
        ),
        _ => generic_action_operation(request, descriptor),
    }
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
    sets: &[ServiceSet],
    endpoints: &[Endpoint],
    topology: Option<&Topology>,
) -> Result<ActionPlanPreview> {
    let descriptor = action_descriptor(&request.action).ok_or_else(|| {
        OrchestratorError::InvalidManifest(format!("unknown action {}", request.action))
    })?;
    let operation = plan_action_request(request, services, sets, endpoints, topology)?;
    Ok(preview_operation(&operation, descriptor))
}

fn generic_action_operation(
    request: &ActionRequest,
    descriptor: &ActionDescriptor,
) -> Result<Operation> {
    let target_id = request
        .field("target_id")
        .or_else(|| request.field("service_id"))
        .or_else(|| request.field("set_id"))
        .or_else(|| request.field("endpoint"))
        .or_else(|| request.field("operation_id"))
        .or_else(|| request.field("report_id"))
        .or_else(|| request.field("topology_snapshot_id"))
        .unwrap_or("current");
    let request_value = Value::Object(
        request
            .fields
            .iter()
            .map(|(key, value)| (key.clone(), Value::String(value.clone())))
            .collect::<Map<_, _>>(),
    );
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

fn find_set<'a>(sets: &'a [ServiceSet], set_id: &str) -> Result<&'a ServiceSet> {
    sets.iter()
        .find(|set| set.id == set_id)
        .ok_or_else(|| OrchestratorError::Dependency(format!("missing Set {}", set_id)))
}

fn endpoint_from_request(
    request: &ActionRequest,
    require_service_id: bool,
    endpoints: &[Endpoint],
) -> Result<Endpoint> {
    let endpoint = request.require_field("endpoint")?;
    validate_endpoint_id(endpoint)?;
    let current = endpoints.iter().find(|item| item.endpoint == endpoint);
    let service_id = if require_service_id {
        request.require_field("service_id")?.to_string()
    } else {
        request
            .field("service_id")
            .or_else(|| current.map(|item| item.service_id.as_str()))
            .unwrap_or("unknown-service")
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

fn map<const N: usize>(items: [(&str, &str); N]) -> BTreeMap<String, String> {
    items
        .into_iter()
        .map(|(key, value)| (key.to_string(), value.to_string()))
        .collect()
}
