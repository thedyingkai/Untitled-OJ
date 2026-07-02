use anyhow::{Context, Result, anyhow};
use clap::Parser;
use orchestrator_core::{
    ActionRequest, EffectiveApiRoute, Endpoint, NodeRecord, NodeServiceDispatchRequest,
    OrchestratorActionConsole, ServiceRoute, SmokeControlPlaneSeed, SmokeNodeTreeSeed,
    default_console_request, parse_endpoint_id, validate_endpoint_id,
};
use serde_json::{Map, Value, json};
use std::collections::BTreeMap;
use std::fs;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::time::Duration;

#[derive(Parser)]
#[command(name = "ojos-orchestrator-daemon")]
#[command(about = "OJOS Orchestrator HTTP API 入口")]
#[command(version)]
struct Cli {
    #[arg(long, default_value = ".")]
    repo_root: PathBuf,

    #[arg(long, default_value = "127.0.0.1:8090")]
    bind: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ApiRequest {
    method: String,
    path: String,
    headers: BTreeMap<String, String>,
    body: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ApiResponse {
    status: u16,
    body: Value,
}

impl ApiResponse {
    fn ok(body: Value) -> Self {
        Self { status: 200, body }
    }

    fn created(body: Value) -> Self {
        Self { status: 201, body }
    }

    fn no_content(body: Value) -> Self {
        Self { status: 200, body }
    }

    fn error(status: u16, message: impl Into<String>) -> Self {
        Self {
            status,
            body: json!({
                "status": "error",
                "message": message.into(),
            }),
        }
    }
}

fn main() -> Result<()> {
    configure_utf8_console()?;
    let cli = Cli::parse();
    let repo_root = fs::canonicalize(&cli.repo_root).unwrap_or(cli.repo_root);
    let console = OrchestratorActionConsole::load(repo_root)?;
    serve(cli.bind, console)
}

fn serve(bind: String, mut console: OrchestratorActionConsole) -> Result<()> {
    let listener = TcpListener::bind(&bind).with_context(|| format!("bind {bind}"))?;
    eprintln!("OJOS Orchestrator daemon listening on {bind}");
    for stream in listener.incoming() {
        let mut stream = match stream {
            Ok(stream) => stream,
            Err(err) => {
                eprintln!("orchestrator daemon accept error: {err}");
                continue;
            }
        };
        if let Err(err) = handle_connection(&mut console, &mut stream) {
            eprintln!("orchestrator daemon connection error: {err}");
        }
    }
    Ok(())
}

fn handle_connection(
    console: &mut OrchestratorActionConsole,
    stream: &mut TcpStream,
) -> Result<()> {
    let response = match read_http_request(stream) {
        Ok(request) => handle_api_request(console, request),
        Err(err) => ApiResponse::error(400, err.to_string()),
    };
    write_http_response(stream, response)
}

fn handle_api_request(console: &mut OrchestratorActionConsole, request: ApiRequest) -> ApiResponse {
    route_api_request(console, request)
        .unwrap_or_else(|err| ApiResponse::error(500, err.to_string()))
}

fn route_api_request(
    console: &mut OrchestratorActionConsole,
    request: ApiRequest,
) -> Result<ApiResponse> {
    let path = request.path.split('?').next().unwrap_or("/");
    let query = request
        .path
        .split_once('?')
        .map(|(_, query)| query)
        .unwrap_or("");
    let segments = path_segments(path)?;
    let segment_refs = segments.iter().map(String::as_str).collect::<Vec<_>>();
    match (request.method.as_str(), segment_refs.as_slice()) {
        ("POST", ["api", "node", "services", "install"]) => {
            require_node_token(&request)?;
            let request = serde_json::from_str::<NodeServiceDispatchRequest>(&request.body)?;
            Ok(ApiResponse::ok(json!({
                "node_dispatch_result": console.accept_node_service_install(request)?,
            })))
        }
        ("POST", ["internal", "smoke", "seed-control-plane"]) => {
            if !smoke_mode_enabled() {
                return Ok(ApiResponse::error(
                    404,
                    "smoke seed endpoint is disabled; set OJOS_SMOKE_MODE=1 for smoke/dev only",
                ));
            }
            let seed = serde_json::from_str::<SmokeControlPlaneSeed>(&request.body)?;
            let child_node_id = seed.child_node_id.clone();
            let routes = console.seed_smoke_control_plane(seed)?;
            Ok(ApiResponse::ok(json!({
                "status": "ok",
                "mode": "smoke/dev-only",
                "node_id": child_node_id,
                "effective_apis": routes,
            })))
        }
        ("POST", ["internal", "smoke", "seed-node-tree"]) => {
            if !smoke_mode_enabled() {
                return Ok(ApiResponse::error(
                    404,
                    "smoke node seed endpoint is disabled; set OJOS_SMOKE_MODE=1 for smoke/dev only",
                ));
            }
            let seed = serde_json::from_str::<SmokeNodeTreeSeed>(&request.body)?;
            let child_node_id = seed.child_node_id.clone();
            let nodes = console.seed_smoke_node_tree(seed)?;
            Ok(ApiResponse::ok(json!({
                "status": "ok",
                "mode": "smoke/dev-only-node-tree",
                "node_id": child_node_id,
                "nodes": nodes,
            })))
        }
        ("GET", ["health"]) => Ok(ApiResponse::ok(json!({
            "status": "ok",
            "service": "ojos-orchestrator-daemon",
            "store": if console.uses_persistent_store() { "persistent" } else { "memory" },
            "orchestrator_database_url": std::env::var("ORCHESTRATOR_DATABASE_URL").is_ok(),
            "warnings": console.warnings(),
        }))),
        ("GET", ["services"]) => Ok(ApiResponse::ok(json!({
            "services": console.view()?.services,
        }))),
        ("GET", ["nodes"]) => Ok(ApiResponse::ok(json!({
            "nodes": console.nodes()?,
        }))),
        ("POST", ["nodes"]) => {
            let node = node_record_from_body(&request.body, None, None)?;
            let node = console.upsert_node(node)?;
            Ok(ApiResponse::created(json!({
                "node": node,
            })))
        }
        ("GET", ["nodes", node_id, "routes"]) => {
            Ok(ApiResponse::ok(json!(internal_effective_route_table(
                console,
                node_id,
                query_bool(query, "include_upstream")
            )?)))
        }
        ("GET", ["nodes", node_id]) => {
            let Some(node) = console.node(node_id)? else {
                return Ok(ApiResponse::error(404, format!("node {node_id} not found")));
            };
            Ok(ApiResponse::ok(json!({
                "node": node,
            })))
        }
        ("PATCH", ["nodes", node_id]) => {
            let Some(existing) = console.node(node_id)? else {
                return Ok(ApiResponse::error(404, format!("node {node_id} not found")));
            };
            let node = node_record_from_body(&request.body, Some(node_id), Some(existing))?;
            let node = console.upsert_node(node)?;
            Ok(ApiResponse::ok(json!({
                "node": node,
            })))
        }
        ("DELETE", ["nodes", node_id]) => {
            console.delete_node(node_id)?;
            Ok(ApiResponse::no_content(json!({
                "deleted": true,
                "node_id": node_id,
            })))
        }
        ("GET", ["internal", "orchestrator", "snapshot"]) => Ok(ApiResponse::ok(json!({
            "version": "1",
            "generated_at": "",
            "service_definitions": internal_service_definitions(console, query_bool(query, "include_disabled")),
            "endpoints": console.endpoints()?,
            "permissions": internal_permissions(console)?,
            "menus": [],
            "frontend_routes": internal_frontend_routes(console)?,
            "gateway_routes": internal_gateway_routes(console, true)?,
            "components": [],
            "health_checks": [],
            "topology": {
                "dependency_edges": []
            }
        }))),
        ("GET", ["internal", "orchestrator", "routes"]) => {
            if let Some(node_id) = query_value(query, "node_id") {
                Ok(ApiResponse::ok(json!(internal_effective_route_table(
                    console,
                    node_id,
                    query_bool(query, "include_upstream")
                )?)))
            } else {
                Ok(ApiResponse::ok(json!(internal_route_table(
                    console,
                    query_bool(query, "include_disabled"),
                    query_bool(query, "include_upstream")
                )?)))
            }
        }
        (
            "GET",
            [
                "internal",
                "orchestrator",
                "nodes",
                node_id,
                "effective-apis",
            ],
        ) => Ok(ApiResponse::ok(json!({
            "node_id": node_id,
            "effective_apis": console.effective_api_routes(node_id)?,
        }))),
        ("GET", ["internal", "orchestrator", "nodes", node_id, "routes"]) => {
            Ok(ApiResponse::ok(json!(internal_effective_route_table(
                console,
                node_id,
                query_bool(query, "include_upstream")
            )?)))
        }
        ("GET", ["release-registry"]) => Ok(ApiResponse::ok(json!({
            "release_registry": console.release_registry()?,
        }))),
        ("GET", ["releases"]) => Ok(ApiResponse::ok(json!({
            "releases": console.service_releases()?,
        }))),
        ("GET", ["releases", service_name]) => Ok(ApiResponse::ok(json!({
            "releases": console
                .service_releases()?
                .into_iter()
                .filter(|release| release.service_name == *service_name)
                .collect::<Vec<_>>(),
        }))),
        ("GET", ["releases", service_name, version]) => Ok(ApiResponse::ok(json!({
            "release": console
                .service_releases()?
                .into_iter()
                .find(|release| release.service_name == *service_name && release.version == *version),
        }))),
        ("POST", ["releases"]) => {
            let action = action_from_body("release.create", &request.body, [])?;
            Ok(ApiResponse::created(json!({
                "action_result": console.dispatch(action)?,
            })))
        }
        ("PATCH", ["releases", service_name]) => {
            let action = action_from_body(
                "release.update",
                &request.body,
                [("service_id", *service_name)],
            )?;
            Ok(ApiResponse::ok(json!({
                "action_result": console.dispatch(action)?,
            })))
        }
        ("POST", ["releases", service_name, "install"]) => {
            let action = action_from_body(
                "release.install",
                &request.body,
                [("service_id", *service_name), ("confirm", "true")],
            )?;
            Ok(ApiResponse::ok(json!({
                "action_result": console.dispatch(action)?,
            })))
        }
        ("DELETE", ["releases", service_name]) => {
            let action = action_from_body(
                "release.delete",
                &request.body,
                [("service_id", *service_name), ("confirm", "true")],
            )?;
            Ok(ApiResponse::no_content(json!({
                "action_result": console.dispatch(action)?,
            })))
        }
        ("DELETE", ["releases", service_name, version]) => {
            let action = action_from_body(
                "release.delete",
                &request.body,
                [
                    ("service_id", *service_name),
                    ("version", *version),
                    ("confirm", "true"),
                ],
            )?;
            Ok(ApiResponse::no_content(json!({
                "action_result": console.dispatch(action)?,
            })))
        }
        ("POST", ["releases", service_name, "rollback"]) => {
            let action = action_from_body(
                "release.rollback",
                &request.body,
                [("service_id", *service_name), ("confirm", "true")],
            )?;
            Ok(ApiResponse::ok(json!({
                "action_result": console.dispatch(action)?,
            })))
        }
        ("POST", ["releases", service_name, version, "rollback"]) => {
            let action = action_from_body(
                "release.rollback",
                &request.body,
                [
                    ("service_id", *service_name),
                    ("version", *version),
                    ("confirm", "true"),
                ],
            )?;
            Ok(ApiResponse::ok(json!({
                "action_result": console.dispatch(action)?,
            })))
        }
        ("GET", ["templates"]) => Ok(ApiResponse::ok(json!({
            "templates": console.view()?.templates,
        }))),
        ("GET", ["sets"]) => Ok(ApiResponse::error(
            410,
            "service-name endpoint groups are derived queries; use /templates for readonly local deployment templates",
        )),
        ("GET", ["endpoints"]) => Ok(ApiResponse::ok(json!({
            "endpoints": console.view()?.endpoints,
        }))),
        ("POST", ["endpoints"]) => {
            let action = action_from_body("endpoint.create", &request.body, [])?;
            Ok(ApiResponse::created(json!({
                "action_result": console.dispatch(action)?,
            })))
        }
        ("POST", ["actions"]) => {
            let action = action_request_from_body(&request.body)?;
            Ok(ApiResponse::ok(json!({
                "action_result": console.dispatch(action)?,
            })))
        }
        ("PATCH", ["endpoints", endpoint]) => {
            validate_endpoint_id(endpoint)?;
            let action = action_from_body(
                "endpoint.update",
                &request.body,
                [("endpoint", *endpoint), ("confirm", "true")],
            )?;
            Ok(ApiResponse::ok(json!({
                "action_result": console.dispatch(action)?,
            })))
        }
        ("DELETE", ["endpoints", endpoint]) => {
            validate_endpoint_id(endpoint)?;
            let action = action_from_body(
                "endpoint.delete",
                &request.body,
                [("endpoint", *endpoint), ("confirm", "true")],
            )?;
            Ok(ApiResponse::no_content(json!({
                "action_result": console.dispatch(action)?,
            })))
        }
        ("POST", ["endpoints", endpoint, "health"]) => {
            validate_endpoint_id(endpoint)?;
            let action = action_from_body(
                "endpoint.health.check",
                &request.body,
                [("endpoint", *endpoint)],
            )?;
            Ok(ApiResponse::ok(json!({
                "action_result": console.dispatch(action)?,
            })))
        }
        ("POST", ["endpoints", "health"]) => {
            let action = action_from_body("endpoint.health.check", &request.body, [])?;
            Ok(ApiResponse::ok(json!({
                "action_result": console.dispatch(action)?,
            })))
        }
        ("GET", ["links"]) => Ok(ApiResponse::ok(json!({
            "links": console.view()?.links,
        }))),
        ("POST", ["links"]) => {
            let action = action_from_body("link.create", &request.body, [("confirm", "true")])?;
            Ok(ApiResponse::created(json!({
                "action_result": console.dispatch(action)?,
            })))
        }
        ("PATCH", ["links", source, target]) => {
            validate_endpoint_id(source)?;
            validate_endpoint_id(target)?;
            let action = action_from_body(
                "link.update",
                &request.body,
                [
                    ("source_endpoint", *source),
                    ("target_endpoint", *target),
                    ("confirm", "true"),
                ],
            )?;
            Ok(ApiResponse::ok(json!({
                "action_result": console.dispatch(action)?,
            })))
        }
        ("DELETE", ["links", source, target]) => {
            validate_endpoint_id(source)?;
            validate_endpoint_id(target)?;
            let action = action_from_body(
                "link.delete",
                &request.body,
                [
                    ("source_endpoint", *source),
                    ("target_endpoint", *target),
                    ("confirm", "true"),
                ],
            )?;
            Ok(ApiResponse::no_content(json!({
                "action_result": console.dispatch(action)?,
            })))
        }
        ("POST", ["links", source, target, "health"]) => {
            validate_endpoint_id(source)?;
            validate_endpoint_id(target)?;
            let action = action_from_body(
                "link.health.check",
                &request.body,
                [("source_endpoint", *source), ("target_endpoint", *target)],
            )?;
            Ok(ApiResponse::ok(json!({
                "action_result": console.dispatch(action)?,
            })))
        }
        ("POST", ["links", "health"]) => {
            let action = action_from_body("link.health.check", &request.body, [])?;
            Ok(ApiResponse::ok(json!({
                "action_result": console.dispatch(action)?,
            })))
        }
        ("POST", ["sets", _set_id, "expand"] | ["sets", _set_id, "apply"]) => {
            Ok(ApiResponse::error(
                410,
                "service-name endpoint groups are derived endpoint queries, not formal orchestrator actions",
            ))
        }
        ("GET", ["operations"]) => Ok(ApiResponse::ok(json!({
            "operations": console.view()?.operations,
        }))),
        ("POST", ["operations", "plan"]) => {
            let action = action_from_body("operation.create", &request.body, [])?;
            Ok(ApiResponse::created(json!({
                "action_result": console.dispatch(action)?,
            })))
        }
        ("GET", ["operations", operation_id]) => {
            let operation = console
                .operation(operation_id)?
                .ok_or_else(|| anyhow!("operation {operation_id} not found"))?;
            Ok(ApiResponse::ok(json!({ "operation": operation })))
        }
        ("POST", ["operations", operation_id, "confirm"]) => {
            let action = operation_action("operation.confirm", operation_id)?;
            Ok(ApiResponse::ok(json!({
                "action_result": console.dispatch(action)?,
            })))
        }
        ("POST", ["operations", operation_id, "apply"]) => {
            let action = operation_action("operation.apply", operation_id)?;
            Ok(ApiResponse::ok(json!({
                "action_result": console.dispatch(action)?,
            })))
        }
        ("POST", ["operations", operation_id, "rollback"]) => {
            let action = operation_action("operation.rollback", operation_id)?;
            Ok(ApiResponse::ok(json!({
                "action_result": console.dispatch(action)?,
            })))
        }
        ("GET", ["operations", operation_id, "logs"]) => {
            let logs = console.operation_logs(operation_id)?;
            Ok(ApiResponse::ok(json!({ "logs": logs })))
        }
        ("GET", ["topology"]) => Ok(ApiResponse::ok(json!({
            "topology": console.topology()?,
        }))),
        ("POST", ["diagnostics"]) => {
            let action = action_from_body("diagnostic.create", &request.body, [])?;
            Ok(ApiResponse::created(json!({
                "action_result": console.dispatch(action)?,
            })))
        }
        ("GET", [report_file]) if report_file.ends_with(".json") => {
            let report_id = report_file.trim_end_matches(".json");
            let export = console.diagnostic_export(report_id, "json")?;
            Ok(ApiResponse::ok(json!({
                "report_id": export.report_id,
                "format": export.format,
                "content": export.content,
            })))
        }
        ("GET", [report_file]) if report_file.ends_with(".md") => {
            let report_id = report_file.trim_end_matches(".md");
            let export = console.diagnostic_export(report_id, "markdown")?;
            Ok(ApiResponse::ok(json!({
                "report_id": export.report_id,
                "format": export.format,
                "content": export.content,
            })))
        }
        ("GET", ["diagnostics", report_file]) if report_file.ends_with(".json") => {
            let report_id = report_file.trim_end_matches(".json");
            let export = console.diagnostic_export(report_id, "json")?;
            Ok(ApiResponse::ok(json!({
                "report_id": export.report_id,
                "format": export.format,
                "content": export.content,
            })))
        }
        ("GET", ["diagnostics", report_file]) if report_file.ends_with(".md") => {
            let report_id = report_file.trim_end_matches(".md");
            let export = console.diagnostic_export(report_id, "markdown")?;
            Ok(ApiResponse::ok(json!({
                "report_id": export.report_id,
                "format": export.format,
                "content": export.content,
            })))
        }
        ("GET", ["diagnostics", report_id]) => {
            let report = console
                .diagnostic_report(report_id)?
                .ok_or_else(|| anyhow!("diagnostic report {report_id} not found"))?;
            Ok(ApiResponse::ok(json!({ "diagnostic_report": report })))
        }
        _ => Ok(ApiResponse::error(
            404,
            format!(
                "unsupported Orchestrator API route {} {}",
                request.method, path
            ),
        )),
    }
}

fn action_request_from_body(body: &str) -> Result<ActionRequest> {
    let trimmed = body.trim();
    if trimmed.is_empty() {
        return Err(anyhow!("POST /actions requires a JSON body"));
    }
    let value = serde_json::from_str::<Value>(trimmed)?;
    let object = value
        .as_object()
        .ok_or_else(|| anyhow!("request body must be a JSON object"))?;
    let action = object
        .get("action")
        .or_else(|| object.get("action_id"))
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("POST /actions requires action"))?;
    let mut request = default_console_request(action)?;
    for (key, value) in object {
        match key.as_str() {
            "action" | "action_id" => {}
            "operation_id" => request.operation_id = field_value(value)?,
            "fields" => {
                merge_json_fields(&mut request.fields, value)?;
            }
            _ => {
                request.fields.insert(key.clone(), field_value(value)?);
            }
        }
    }
    Ok(request)
}

fn action_from_body<const N: usize>(
    action: &str,
    body: &str,
    overrides: [(&str, &str); N],
) -> Result<ActionRequest> {
    let mut request = default_console_request(action)?;
    for (key, value) in fields_from_body(body)? {
        if key == "operation_id" {
            request.operation_id = value;
        } else {
            request.fields.insert(key, value);
        }
    }
    for (key, value) in overrides {
        request.fields.insert(key.to_string(), value.to_string());
    }
    Ok(request)
}

fn operation_action(action: &str, operation_id: &str) -> Result<ActionRequest> {
    let mut request = default_console_request(action)?;
    request.operation_id = format!("{}-{}", action.replace('.', "-"), operation_id);
    request
        .fields
        .insert("operation_id".to_string(), operation_id.to_string());
    request
        .fields
        .insert("confirm".to_string(), "true".to_string());
    Ok(request)
}

fn node_record_from_body(
    body: &str,
    path_node_id: Option<&str>,
    existing: Option<NodeRecord>,
) -> Result<NodeRecord> {
    let value = serde_json::from_str::<Value>(body.trim())?;
    let object = value
        .as_object()
        .ok_or_else(|| anyhow!("node request body must be a JSON object"))?;
    let mut node = existing.unwrap_or(NodeRecord {
        node_id: String::new(),
        host_ip: String::new(),
        parent_node_id: String::new(),
        role: String::new(),
        labels: json!({}),
        status: String::new(),
        created_at: String::new(),
        updated_at: String::new(),
    });
    if let Some(node_id) = path_node_id {
        node.node_id = node_id.to_string();
    }
    if let Some(value) = object.get("node_id").and_then(Value::as_str) {
        let value = value.trim();
        if let Some(path_node_id) = path_node_id {
            if value != path_node_id {
                return Err(anyhow!("node_id body/path mismatch"));
            }
        } else {
            node.node_id = value.to_string();
        }
    }
    if let Some(value) = object.get("host_ip").and_then(Value::as_str) {
        node.host_ip = value.trim().to_string();
    }
    if let Some(value) = object.get("parent_node_id").and_then(Value::as_str) {
        node.parent_node_id = value.trim().to_string();
    }
    if let Some(value) = object.get("role").and_then(Value::as_str) {
        node.role = value.trim().to_string();
    }
    if let Some(value) = object.get("labels") {
        node.labels = value.clone();
    }
    if let Some(value) = object.get("status").and_then(Value::as_str) {
        node.status = value.trim().to_string();
    }
    Ok(node)
}

fn internal_service_definitions(
    console: &OrchestratorActionConsole,
    _include_disabled: bool,
) -> Vec<Value> {
    console
        .services()
        .unwrap_or_default()
        .into_iter()
        .map(|service| {
            let manifest = serde_json::to_value(&service).unwrap_or(Value::Null);
            json!({
                "service_id": service.id.clone(),
                "name": service.name.clone(),
                "version": service.version.clone(),
                "status": "ENABLED",
                "kind": service.kind.clone(),
                "description": service.description.clone(),
                "manifest": manifest,
            })
        })
        .collect()
}

fn internal_permissions(console: &OrchestratorActionConsole) -> Result<Vec<Value>> {
    Ok(console
        .service_permission_records()?
        .into_iter()
        .map(|permission| {
            json!({
                "service_id": permission.service_name,
                "permission_key": permission.permission_key,
                "description": permission.source,
            })
        })
        .collect())
}

fn internal_frontend_routes(console: &OrchestratorActionConsole) -> Result<Vec<Value>> {
    Ok(console
        .service_frontend_entries()?
        .into_iter()
        .filter(|entry| entry.enabled)
        .map(|entry| {
            json!({
                "service_id": entry.service_name,
                "route_path": entry.route_prefix,
                "route_name": entry.remote_entry,
                "component_key": entry.remote_entry,
                "required_permission": "",
                "enabled": entry.enabled,
            })
        })
        .collect())
}

fn internal_gateway_routes(
    console: &OrchestratorActionConsole,
    include_upstream: bool,
) -> Result<Vec<Value>> {
    let endpoints = console.endpoints()?;
    Ok(console
        .service_routes()?
        .into_iter()
        .map(|route| {
            let upstream = if include_upstream {
                upstream_base_for_route(&route, &endpoints)
            } else {
                String::new()
            };
            json!({
                "service_id": route.target_service_name,
                "prefix": route_prefix_for_gateway(&route.path),
                "target_service": route.target_service_name,
                "upstream_base": upstream,
                "auth_mode": auth_mode_for_route(&route),
                "required_permission": required_permission(&route.permission),
                "strip_prefix": "/api",
                "rewrite_prefix": "",
                "health_check_id": format!("{}-health", route.target_service_name),
                "enabled": route.enabled,
            })
        })
        .collect())
}

fn internal_route_table(
    console: &OrchestratorActionConsole,
    _include_disabled: bool,
    include_upstream: bool,
) -> Result<Value> {
    let endpoints = console.endpoints()?;
    let routes = console
        .service_routes()?
        .into_iter()
        .map(|route| {
            let prefix = route_prefix_for_gateway(&route.path);
            let upstream = upstream_base_for_route(&route, &endpoints);
            let blocked_by = if upstream.is_empty() {
                vec!["missing endpoint".to_string()]
            } else {
                Vec::new()
            };
            let proxy_enabled = route.enabled && blocked_by.is_empty();
            json!({
                "route_id": format!("{}:{}", route.target_service_name, prefix),
                "owner_service_id": route.target_service_name,
                "prefix": prefix,
                "service_id": route.target_service_name,
                "target_service": route.target_service_name,
                "upstream_base": if include_upstream { upstream } else { String::new() },
                "auth_mode": auth_mode_for_route(&route),
                "required_permission": required_permission(&route.permission),
                "methods": route_methods(&route.method),
                "enabled": route.enabled,
                "proxy_enabled": proxy_enabled,
                "priority": route_prefix_for_gateway(&route.path).len(),
                "strip_prefix": "/api",
                "rewrite_prefix": "",
                "health_check_id": format!("{}-health", route.target_service_name),
                "created_from": "orchestrator_registry",
                "status": if proxy_enabled { "active" } else if route.enabled { "blocked" } else { "disabled" },
                "service_status": "",
                "service_health": "",
                "conflicts": [],
                "warnings": [],
                "blocked_by": blocked_by,
            })
        })
        .collect::<Vec<_>>();
    Ok(json!({
        "version": "1",
        "generated_at": "",
        "routes": routes,
        "warnings": [],
        "can_proxy": routes_have_proxy(console)?,
    }))
}

fn internal_effective_route_table(
    console: &OrchestratorActionConsole,
    node_id: &str,
    include_upstream: bool,
) -> Result<Value> {
    let routes = console.effective_api_routes(node_id)?;
    let route_items = routes
        .iter()
        .map(|route| effective_route_table_item(route, include_upstream))
        .collect::<Result<Vec<_>>>()?;
    Ok(json!({
        "version": "1",
        "generated_at": "",
        "node_id": node_id,
        "routes": route_items,
        "warnings": [],
        "can_proxy": routes.iter().any(|route| route.status == "running"),
    }))
}

fn effective_route_table_item(route: &EffectiveApiRoute, include_upstream: bool) -> Result<Value> {
    let upstream = endpoint_upstream_base_from_id(&route.provider_endpoint, &route.protocol)?;
    let enabled = route.status == "running";
    let blocked_by = if upstream.is_empty() {
        vec!["missing endpoint".to_string()]
    } else {
        Vec::new()
    };
    let proxy_enabled = enabled && blocked_by.is_empty();
    Ok(json!({
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
        "upstream_base": if include_upstream { upstream } else { String::new() },
        "auth_mode": route.auth_mode,
        "required_permission": required_permission(&route.permission),
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
        "service_health": "",
        "conflicts": [],
        "warnings": [],
        "blocked_by": blocked_by,
    }))
}

fn routes_have_proxy(console: &OrchestratorActionConsole) -> Result<bool> {
    let endpoints = console.endpoints()?;
    Ok(console
        .service_routes()?
        .into_iter()
        .any(|route| route.enabled && !upstream_base_for_route(&route, &endpoints).is_empty()))
}

fn upstream_base_for_route(route: &ServiceRoute, endpoints: &[Endpoint]) -> String {
    let service_name = route.target_service_name.trim();
    endpoints
        .iter()
        .filter(|endpoint| endpoint.service_id == service_name)
        .filter_map(endpoint_upstream_base)
        .next()
        .unwrap_or_default()
}

fn endpoint_upstream_base(endpoint: &Endpoint) -> Option<String> {
    let identity = parse_endpoint_id(&endpoint.endpoint).ok()?;
    let scheme = if endpoint.protocol.trim().is_empty() {
        "http"
    } else {
        endpoint.protocol.trim()
    };
    Some(format!("{scheme}://{}:{}", identity.host, identity.port))
}

fn endpoint_upstream_base_from_id(endpoint: &str, protocol: &str) -> Result<String> {
    let identity = parse_endpoint_id(endpoint)?;
    let scheme = if protocol.trim().is_empty() {
        "http"
    } else {
        protocol.trim()
    };
    Ok(format!("{scheme}://{}:{}", identity.host, identity.port))
}

fn route_prefix_for_gateway(path: &str) -> String {
    let mut prefix = path
        .trim()
        .trim_end_matches('*')
        .trim_end_matches('/')
        .to_string();
    if prefix.is_empty() {
        prefix = "/".to_string();
    }
    if !prefix.starts_with('/') {
        prefix.insert(0, '/');
    }
    prefix
}

fn auth_mode_for_route(route: &ServiceRoute) -> String {
    if required_permission(&route.permission).is_empty() {
        "public".to_string()
    } else {
        "user".to_string()
    }
}

fn required_permission(permission: &str) -> String {
    let permission = permission.trim();
    if permission.eq_ignore_ascii_case("public") {
        String::new()
    } else {
        permission.to_string()
    }
}

fn route_methods(method: &str) -> Vec<String> {
    if method.eq_ignore_ascii_case("ANY") || method.trim().is_empty() {
        vec![
            "GET".to_string(),
            "POST".to_string(),
            "PUT".to_string(),
            "PATCH".to_string(),
            "DELETE".to_string(),
            "OPTIONS".to_string(),
            "HEAD".to_string(),
        ]
    } else {
        vec![method.trim().to_ascii_uppercase()]
    }
}

fn smoke_mode_enabled() -> bool {
    std::env::var("OJOS_SMOKE_MODE")
        .map(|value| matches!(value.trim(), "1" | "true" | "TRUE" | "True"))
        .unwrap_or(false)
}

fn fields_from_body(body: &str) -> Result<BTreeMap<String, String>> {
    let trimmed = body.trim();
    if trimmed.is_empty() {
        return Ok(BTreeMap::new());
    }
    let value = serde_json::from_str::<Value>(trimmed)?;
    let object = value
        .as_object()
        .ok_or_else(|| anyhow!("request body must be a JSON object"))?;
    let mut fields = BTreeMap::new();
    for (key, value) in object {
        if key == "fields" {
            merge_json_fields(&mut fields, value)?;
        } else {
            fields.insert(key.clone(), field_value(value)?);
        }
    }
    Ok(fields)
}

fn query_bool(query: &str, name: &str) -> bool {
    query.split('&').any(|pair| {
        let Some((key, value)) = pair.split_once('=') else {
            return false;
        };
        key == name && matches!(value, "1" | "true" | "TRUE" | "True")
    })
}

fn query_value<'a>(query: &'a str, name: &str) -> Option<&'a str> {
    query.split('&').find_map(|pair| {
        let (key, value) = pair.split_once('=')?;
        (key == name && !value.trim().is_empty()).then_some(value)
    })
}

fn merge_json_fields(fields: &mut BTreeMap<String, String>, value: &Value) -> Result<()> {
    let object = value
        .as_object()
        .ok_or_else(|| anyhow!("fields must be a JSON object"))?;
    for (key, value) in object {
        fields.insert(key.clone(), field_value(value)?);
    }
    Ok(())
}

fn field_value(value: &Value) -> Result<String> {
    match value {
        Value::Null => Ok(String::new()),
        Value::String(text) => Ok(text.clone()),
        Value::Bool(flag) => Ok(flag.to_string()),
        Value::Number(number) => Ok(number.to_string()),
        Value::Array(_) | Value::Object(_) => Ok(serde_json::to_string(value)?),
    }
}

fn path_segments(path: &str) -> Result<Vec<String>> {
    path.trim_matches('/')
        .split('/')
        .filter(|segment| !segment.is_empty())
        .map(percent_decode_segment)
        .collect()
}

fn percent_decode_segment(segment: &str) -> Result<String> {
    let bytes = segment.as_bytes();
    let mut output = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            let hex = bytes
                .get(index + 1..index + 3)
                .ok_or_else(|| anyhow!("invalid percent-encoded path segment"))?;
            let text = std::str::from_utf8(hex)?;
            let value = u8::from_str_radix(text, 16)
                .map_err(|_| anyhow!("invalid percent-encoded path segment"))?;
            output.push(value);
            index += 3;
        } else {
            output.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8(output).map_err(Into::into)
}

fn read_http_request(stream: &mut TcpStream) -> Result<ApiRequest> {
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    let mut bytes = Vec::new();
    let mut buffer = [0_u8; 4096];
    loop {
        let read = stream.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        bytes.extend_from_slice(&buffer[..read]);
        if complete_http_request(&bytes)? {
            break;
        }
        if bytes.len() > 1024 * 1024 {
            return Err(anyhow!("request body is too large"));
        }
    }
    parse_http_request_bytes(bytes)
}

fn complete_http_request(bytes: &[u8]) -> Result<bool> {
    let Some(header_end) = header_end(bytes) else {
        return Ok(false);
    };
    let headers = std::str::from_utf8(&bytes[..header_end])?;
    let content_length = content_length(headers)?;
    Ok(bytes.len() >= header_end + 4 + content_length)
}

fn parse_http_request_bytes(bytes: Vec<u8>) -> Result<ApiRequest> {
    let header_end = header_end(&bytes).ok_or_else(|| anyhow!("HTTP headers are incomplete"))?;
    let headers = std::str::from_utf8(&bytes[..header_end])?;
    let mut lines = headers.lines();
    let request_line = lines
        .next()
        .ok_or_else(|| anyhow!("missing request line"))?;
    let mut parts = request_line.split_whitespace();
    let method = parts
        .next()
        .ok_or_else(|| anyhow!("missing HTTP method"))?
        .to_string();
    let path = parts
        .next()
        .ok_or_else(|| anyhow!("missing HTTP path"))?
        .to_string();
    let headers = parse_headers(lines)?;
    let content_length = content_length_from_headers(&headers)?;
    let body_bytes = bytes
        .get(header_end + 4..header_end + 4 + content_length)
        .ok_or_else(|| anyhow!("HTTP body is incomplete"))?;
    let body = String::from_utf8(body_bytes.to_vec())?;
    Ok(ApiRequest {
        method,
        path,
        headers,
        body,
    })
}

fn parse_headers<'a>(lines: impl Iterator<Item = &'a str>) -> Result<BTreeMap<String, String>> {
    let mut headers = BTreeMap::new();
    for line in lines {
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        headers.insert(name.trim().to_ascii_lowercase(), value.trim().to_string());
    }
    Ok(headers)
}

fn header_end(bytes: &[u8]) -> Option<usize> {
    bytes.windows(4).position(|window| window == b"\r\n\r\n")
}

fn content_length(headers: &str) -> Result<usize> {
    for line in headers.lines().skip(1) {
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        if name.eq_ignore_ascii_case("content-length") {
            return value
                .trim()
                .parse::<usize>()
                .map_err(|_| anyhow!("invalid content-length"));
        }
    }
    Ok(0)
}

fn content_length_from_headers(headers: &BTreeMap<String, String>) -> Result<usize> {
    headers
        .get("content-length")
        .map(|value| {
            value
                .trim()
                .parse::<usize>()
                .map_err(|_| anyhow!("invalid content-length"))
        })
        .transpose()
        .map(|value| value.unwrap_or(0))
}

fn require_node_token(request: &ApiRequest) -> Result<()> {
    let token = std::env::var("ORCHESTRATOR_NODE_TOKEN")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());
    let Some(token) = token else {
        return Ok(());
    };
    let expected = format!("Bearer {token}");
    let actual = request
        .headers
        .get("authorization")
        .map(String::as_str)
        .unwrap_or("");
    if actual == expected {
        Ok(())
    } else {
        Err(anyhow!("node install request is unauthorized"))
    }
}

fn write_http_response(stream: &mut TcpStream, response: ApiResponse) -> Result<()> {
    let body = response_json(response.body)?;
    let status_text = match response.status {
        200 => "OK",
        201 => "Created",
        400 => "Bad Request",
        404 => "Not Found",
        _ => "Internal Server Error",
    };
    write!(
        stream,
        "HTTP/1.1 {} {}\r\nContent-Type: application/json; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        response.status,
        status_text,
        body.len(),
        body
    )?;
    Ok(())
}

fn response_json(mut body: Value) -> Result<String> {
    if let Some(object) = body.as_object_mut() {
        ensure_status_field(object);
    }
    Ok(serde_json::to_string_pretty(&body)?)
}

fn ensure_status_field(object: &mut Map<String, Value>) {
    object
        .entry("status".to_string())
        .or_insert_with(|| Value::String("ok".to_string()));
}

fn configure_utf8_console() -> Result<()> {
    #[cfg(windows)]
    {
        const CP_UTF8: u32 = 65001;
        let output_ok = unsafe { SetConsoleOutputCP(CP_UTF8) } != 0;
        let input_ok = unsafe { SetConsoleCP(CP_UTF8) } != 0;
        if !output_ok || !input_ok {
            anyhow::bail!("无法�?Windows 控制台输�?输出编码设置�?UTF-8");
        }
    }
    Ok(())
}

#[cfg(windows)]
#[link(name = "kernel32")]
unsafe extern "system" {
    fn SetConsoleOutputCP(code_page_id: u32) -> i32;
    fn SetConsoleCP(code_page_id: u32) -> i32;
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    static NODE_INSTALL_ENV_LOCK: Mutex<()> = Mutex::new(());

    fn repo_root() -> PathBuf {
        let mut current = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        loop {
            if current.join("Cargo.toml").is_file()
                && current
                    .join("platform/schemas/orchestrator/actions.yaml")
                    .is_file()
                && current
                    .join("services/orchestrator/core/Cargo.toml")
                    .is_file()
            {
                return current;
            }
            if !current.pop() {
                panic!("repo root");
            }
        }
    }

    fn console() -> OrchestratorActionConsole {
        OrchestratorActionConsole::load(repo_root()).expect("daemon console")
    }

    fn request(method: &str, path: &str, body: &str) -> ApiRequest {
        ApiRequest {
            method: method.to_string(),
            path: path.to_string(),
            headers: BTreeMap::new(),
            body: body.to_string(),
        }
    }

    fn post_json(console: &mut OrchestratorActionConsole, path: &str, body: &str) -> ApiResponse {
        route_api_request(console, request("POST", path, body)).expect("POST response")
    }

    fn node_install_body_for(
        service_yaml: &str,
        operation_id: &str,
        host_ip: &str,
        port: u16,
    ) -> String {
        let root = repo_root();
        let service = orchestrator_core::validate_service_manifest_file(
            &root,
            std::path::Path::new(service_yaml),
        )
        .expect("service manifest");
        let endpoint_id = format!("{host_ip}:{port}:{}", service.id);
        serde_json::to_string(&NodeServiceDispatchRequest {
            operation_id: operation_id.to_string(),
            service: service.clone(),
            release: None,
            host_service: orchestrator_core::HostService {
                host_ip: host_ip.to_string(),
                service_name: service.id.clone(),
                version: service.version.clone(),
                status: "starting".to_string(),
                config: json!({"node": true}),
                labels: json!({"source": "root-orchestrator"}),
                created_at: String::new(),
                updated_at: String::new(),
            },
            endpoint: Endpoint {
                endpoint: endpoint_id,
                service_id: service.id.clone(),
                protocol: service.endpoint.protocol.clone(),
                health_path: service.endpoint.health_path.clone(),
                health: "unknown".to_string(),
                reachable: false,
                display_name: format!("{} on node", service.id),
                note: String::new(),
                config: json!({}),
                created_at: String::new(),
                updated_at: String::new(),
            },
            rendered_config: json!({"node": true}),
            package_load: None,
        })
        .expect("node install body")
    }

    fn node_install_body() -> String {
        node_install_body_for(
            "services/gateway/service.yaml",
            "op-node-install-gateway",
            "10.10.0.7",
            8080,
        )
    }

    fn restore_node_token_env(previous: Option<String>) {
        unsafe {
            match previous {
                Some(value) => std::env::set_var("ORCHESTRATOR_NODE_TOKEN", value),
                None => std::env::remove_var("ORCHESTRATOR_NODE_TOKEN"),
            }
        }
    }

    fn restore_env(name: &str, previous: Option<String>) {
        unsafe {
            match previous {
                Some(value) => std::env::set_var(name, value),
                None => std::env::remove_var(name),
            }
        }
    }

    #[test]
    fn daemon_node_install_route_accepts_dispatched_service() {
        let _guard = NODE_INSTALL_ENV_LOCK.lock().expect("node install env lock");
        let previous = std::env::var("ORCHESTRATOR_NODE_TOKEN").ok();
        unsafe {
            std::env::remove_var("ORCHESTRATOR_NODE_TOKEN");
        }
        let mut console = console();
        let response = route_api_request(
            &mut console,
            request("POST", "/api/node/services/install", &node_install_body()),
        )
        .expect("node install response");
        restore_node_token_env(previous);

        assert_eq!(response.status, 200);
        assert_eq!(response.body["node_dispatch_result"]["accepted"], true);
        assert_eq!(
            response.body["node_dispatch_result"]["endpoint"],
            "10.10.0.7:8080:gateway"
        );
        let endpoints = console.endpoints().expect("console endpoints");
        assert!(
            endpoints
                .iter()
                .any(|endpoint| endpoint.endpoint == "10.10.0.7:8080:gateway")
        );
        let operation = get(&mut console, "/operations/op-node-install-gateway");
        assert_eq!(operation.status, 200);
        assert_eq!(operation.body["operation"]["status"], json!("SUCCEEDED"));
        assert_eq!(
            operation.body["operation"]["target_type"],
            json!("NodeServiceInstall")
        );
        let logs = get(&mut console, "/operations/op-node-install-gateway/logs");
        assert_eq!(logs.status, 200);
        let logs = logs.body["logs"].as_array().expect("operation logs");
        assert!(logs.iter().any(|log| log["step_id"] == "node-accept"));
        assert!(logs.iter().any(|log| log["step_id"] == "node-store"));
        assert!(logs.iter().any(|log| {
            log["step_id"] == "node-driver"
                && log["message"].as_str().unwrap_or("").contains("deferred")
        }));
    }

    #[test]
    fn daemon_node_install_route_requires_token_when_configured() {
        let _guard = NODE_INSTALL_ENV_LOCK.lock().expect("node install env lock");
        let previous = std::env::var("ORCHESTRATOR_NODE_TOKEN").ok();
        unsafe {
            std::env::set_var("ORCHESTRATOR_NODE_TOKEN", "node-secret");
        }
        let mut console = console();
        let response = handle_api_request(
            &mut console,
            request("POST", "/api/node/services/install", &node_install_body()),
        );
        restore_node_token_env(previous);

        assert_eq!(response.status, 500);
        assert!(
            response.body["message"]
                .as_str()
                .unwrap_or("")
                .contains("unauthorized")
        );
    }

    #[test]
    fn daemon_node_install_route_runs_driver_when_enabled() {
        let _guard = NODE_INSTALL_ENV_LOCK.lock().expect("node install env lock");
        let previous_token = std::env::var("ORCHESTRATOR_NODE_TOKEN").ok();
        let previous_execute = std::env::var("ORCHESTRATOR_NODE_EXECUTE_SERVICE_DRIVER").ok();
        let previous_docker = std::env::var("OJOS_ORCHESTRATOR_DOCKER_BINARY").ok();
        unsafe {
            std::env::remove_var("ORCHESTRATOR_NODE_TOKEN");
            std::env::set_var("ORCHESTRATOR_NODE_EXECUTE_SERVICE_DRIVER", "true");
            std::env::set_var(
                "OJOS_ORCHESTRATOR_DOCKER_BINARY",
                "ojos-docker-compose-missing",
            );
        }
        let mut console = console();
        let response = route_api_request(
            &mut console,
            request("POST", "/api/node/services/install", &node_install_body()),
        )
        .expect("node install response");
        restore_env("ORCHESTRATOR_NODE_TOKEN", previous_token);
        restore_env("ORCHESTRATOR_NODE_EXECUTE_SERVICE_DRIVER", previous_execute);
        restore_env("OJOS_ORCHESTRATOR_DOCKER_BINARY", previous_docker);

        assert_eq!(response.status, 200);
        assert_eq!(response.body["node_dispatch_result"]["accepted"], true);
        assert!(
            response.body["node_dispatch_result"]["message"]
                .as_str()
                .unwrap_or("")
                .contains("driver FAILED")
        );
        let endpoints = console.endpoints().expect("console endpoints");
        let endpoint = endpoints
            .iter()
            .find(|endpoint| endpoint.endpoint == "10.10.0.7:8080:gateway")
            .expect("node endpoint");
        assert_eq!(endpoint.health, "deferred");
        assert!(!endpoint.reachable);
        let operation = get(&mut console, "/operations/op-node-install-gateway");
        assert_eq!(operation.status, 200);
        assert_eq!(operation.body["operation"]["status"], json!("FAILED"));
        assert!(
            operation.body["operation"]["error_message"]
                .as_str()
                .unwrap_or("")
                .contains("node service driver failed")
        );
        let logs = get(&mut console, "/operations/op-node-install-gateway/logs");
        assert_eq!(logs.status, 200);
        let logs = logs.body["logs"].as_array().expect("operation logs");
        assert!(logs.iter().any(|log| {
            log["step_id"] == "node-driver"
                && log["level"] == "error"
                && log["message"].as_str().unwrap_or("").contains("FAILED")
        }));
        assert!(logs.iter().any(|log| log["step_id"] == "node-health"));
    }

    #[test]
    fn daemon_node_install_route_records_driver_setup_failure() {
        let _guard = NODE_INSTALL_ENV_LOCK.lock().expect("node install env lock");
        let previous_token = std::env::var("ORCHESTRATOR_NODE_TOKEN").ok();
        let previous_execute = std::env::var("ORCHESTRATOR_NODE_EXECUTE_SERVICE_DRIVER").ok();
        unsafe {
            std::env::remove_var("ORCHESTRATOR_NODE_TOKEN");
            std::env::set_var("ORCHESTRATOR_NODE_EXECUTE_SERVICE_DRIVER", "true");
        }
        let mut console = console();
        let response = handle_api_request(
            &mut console,
            request(
                "POST",
                "/api/node/services/install",
                &node_install_body_for(
                    "services/orchestrator/service.yaml",
                    "op-node-install-orchestrator",
                    "10.10.0.8",
                    8090,
                ),
            ),
        );
        restore_env("ORCHESTRATOR_NODE_TOKEN", previous_token);
        restore_env("ORCHESTRATOR_NODE_EXECUTE_SERVICE_DRIVER", previous_execute);

        assert_eq!(response.status, 500);
        assert!(
            response.body["message"]
                .as_str()
                .unwrap_or("")
                .contains("requires release runtime configuration")
        );
        let operation = get(&mut console, "/operations/op-node-install-orchestrator");
        assert_eq!(operation.status, 200);
        assert_eq!(operation.body["operation"]["status"], json!("FAILED"));
        assert!(
            operation.body["operation"]["error_message"]
                .as_str()
                .unwrap_or("")
                .contains("requires release runtime configuration")
        );
        let logs = get(
            &mut console,
            "/operations/op-node-install-orchestrator/logs",
        );
        assert_eq!(logs.status, 200);
        let logs = logs.body["logs"].as_array().expect("operation logs");
        assert!(logs.iter().any(|log| log["step_id"] == "node-accept"));
        assert!(logs.iter().any(|log| {
            log["step_id"] == "node-driver"
                && log["level"] == "error"
                && log["message"]
                    .as_str()
                    .unwrap_or("")
                    .contains("failed before execution")
        }));
    }

    fn get(console: &mut OrchestratorActionConsole, path: &str) -> ApiResponse {
        route_api_request(console, request("GET", path, "")).expect("GET response")
    }

    fn seed_gateway_auth_service_link(console: &mut OrchestratorActionConsole) {
        let gateway = post_json(
            console,
            "/endpoints",
            r#"{
                "operation_id": "op-daemon-gateway-endpoint",
                "endpoint": "127.0.0.1:19180:gateway",
                "service_id": "gateway",
                "protocol": "http",
                "health_path": "/health"
            }"#,
        );
        assert_eq!(gateway.status, 201);

        let auth_service = post_json(
            console,
            "/endpoints",
            r#"{
                "operation_id": "op-daemon-auth-service-endpoint",
                "endpoint": "127.0.0.1:19181:auth-service",
                "service_id": "auth-service",
                "protocol": "http",
                "health_path": "/health"
            }"#,
        );
        assert_eq!(auth_service.status, 201);

        let link = post_json(
            console,
            "/links",
            r#"{
                "operation_id": "op-daemon-gateway-auth-service-link",
                "source_endpoint": "127.0.0.1:19180:gateway",
                "target_endpoint": "127.0.0.1:19181:auth-service",
                "protocol": "http",
                "auth_mode": "internal",
                "scope": "gateway-to-auth-service"
            }"#,
        );
        assert_eq!(link.status, 201);
    }

    #[test]
    fn daemon_health_reports_orchestrator_api_status() {
        let mut console = console();
        let response = route_api_request(&mut console, request("GET", "/health", ""))
            .expect("health response");
        assert_eq!(response.status, 200);
        assert_eq!(response.body["service"], "ojos-orchestrator-daemon");
        assert!(matches!(
            response.body["store"].as_str(),
            Some("memory" | "persistent")
        ));
    }

    #[test]
    fn daemon_endpoint_routes_use_core_dispatcher() {
        let mut console = console();
        let response = route_api_request(
            &mut console,
            request(
                "POST",
                "/endpoints",
                r#"{
                    "endpoint": "127.0.0.1:19011:gateway",
                    "service_id": "gateway",
                    "protocol": "http",
                    "health_path": "/health"
                }"#,
            ),
        )
        .expect("endpoint create response");
        assert_eq!(response.status, 201);
        assert_eq!(
            response.body["action_result"]["action_id"],
            "endpoint.create"
        );
        assert_ne!(response.body["action_result"]["status"], "UNSUPPORTED");

        let response = route_api_request(&mut console, request("GET", "/endpoints", ""))
            .expect("endpoints response");
        assert!(
            response.body["endpoints"]
                .as_array()
                .expect("endpoint rows")
                .iter()
                .any(|endpoint| endpoint["endpoint"] == "127.0.0.1:19011:gateway")
        );
    }

    #[test]
    fn daemon_operation_routes_expose_operation_state_and_logs() {
        let mut console = console();
        let response = route_api_request(
            &mut console,
            request(
                "POST",
                "/endpoints",
                r#"{
                    "operation_id": "op-daemon-endpoint",
                    "endpoint": "127.0.0.1:19012:gateway",
                    "service_id": "gateway",
                    "protocol": "http"
                }"#,
            ),
        )
        .expect("endpoint create response");
        assert_eq!(response.status, 201);

        let response = route_api_request(
            &mut console,
            request("GET", "/operations/op-daemon-endpoint", ""),
        )
        .expect("operation response");
        assert_eq!(response.status, 200);
        assert_eq!(
            response.body["operation"]["operation_id"],
            "op-daemon-endpoint"
        );

        let response = route_api_request(
            &mut console,
            request("GET", "/operations/op-daemon-endpoint/logs", ""),
        )
        .expect("operation logs response");
        assert_eq!(response.status, 200);
        assert!(
            response.body["logs"]
                .as_array()
                .expect("logs")
                .iter()
                .any(|log| log["operation_id"] == "op-daemon-endpoint")
        );
    }

    #[test]
    fn daemon_diagnostic_route_uses_core_diagnostic_report() {
        let mut console = console();
        let response = route_api_request(&mut console, request("POST", "/diagnostics", "{}"))
            .expect("diagnostics response");
        assert_eq!(response.status, 201);
        assert_eq!(
            response.body["action_result"]["action_id"],
            "diagnostic.create"
        );
    }

    #[test]
    fn daemon_topology_reflects_endpoint_link_mutations() {
        let mut console = console();
        seed_gateway_auth_service_link(&mut console);

        let response = get(&mut console, "/topology");
        assert_eq!(response.status, 200);
        let endpoints = response.body["topology"]["endpoints"]
            .as_array()
            .expect("topology endpoints");
        assert!(
            endpoints
                .iter()
                .any(|endpoint| endpoint["endpoint"] == "127.0.0.1:19180:gateway")
        );
        assert!(
            endpoints
                .iter()
                .any(|endpoint| endpoint["endpoint"] == "127.0.0.1:19181:auth-service")
        );
        let links = response.body["topology"]["links"]
            .as_array()
            .expect("topology links");
        assert!(links.iter().any(|link| {
            link["source_endpoint"] == "127.0.0.1:19180:gateway"
                && link["target_endpoint"] == "127.0.0.1:19181:auth-service"
        }));
    }

    #[test]
    fn topology_is_rebuilt_from_store_after_actions() {
        let mut console = console();
        let before = get(&mut console, "/topology");
        let before_count = before.body["topology"]["endpoints"]
            .as_array()
            .map(Vec::len)
            .unwrap_or_default();

        let response = post_json(
            &mut console,
            "/actions",
            r#"{
                "action": "endpoint.create",
                "operation_id": "op-daemon-actions-endpoint",
                "fields": {
                    "endpoint": "127.0.0.1:19182:gateway",
                    "service_id": "gateway",
                    "protocol": "http"
                }
            }"#,
        );
        assert_eq!(
            response.body["action_result"]["action_id"],
            "endpoint.create"
        );

        let after = get(&mut console, "/topology");
        let endpoints = after.body["topology"]["endpoints"]
            .as_array()
            .expect("topology endpoints");
        assert!(endpoints.len() >= before_count);
        assert!(
            endpoints
                .iter()
                .any(|endpoint| endpoint["endpoint"] == "127.0.0.1:19182:gateway")
        );
    }

    #[test]
    fn daemon_internal_routes_expose_registry_upstream_and_permission() {
        let mut console = console();
        let endpoint = post_json(
            &mut console,
            "/endpoints",
            r#"{
                "operation_id": "op-daemon-internal-demo-endpoint",
                "endpoint": "127.0.0.1:19200:gateway",
                "service_id": "gateway",
                "protocol": "http",
                "health_path": "/health"
            }"#,
        );
        assert_eq!(endpoint.status, 201);
        assert_eq!(endpoint.body["action_result"]["status"], "SUCCEEDED");

        let route = post_json(
            &mut console,
            "/actions",
            r#"{
                "action": "route.create",
                "operation_id": "op-daemon-internal-demo-route",
                "fields": {
                    "route_id": "/api/demo/**",
                    "path": "/api/demo/**",
                    "method": "ANY",
                    "target": "gateway[*]",
                    "permission": "demo.read",
                    "confirm": "true"
                }
            }"#,
        );
        assert_eq!(route.status, 200);
        assert_eq!(route.body["action_result"]["status"], "SUCCEEDED");

        let table = get(
            &mut console,
            "/internal/orchestrator/routes?include_upstream=true",
        );
        assert_eq!(table.status, 200);
        let routes = table.body["routes"].as_array().expect("routes");
        let demo = routes
            .iter()
            .find(|route| route["prefix"] == "/api/demo")
            .expect("demo route");
        assert_eq!(demo["target_service"], "gateway");
        assert_eq!(demo["upstream_base"], "http://127.0.0.1:19200");
        assert_eq!(demo["required_permission"], "demo.read");
        assert_eq!(demo["auth_mode"], "user");
        assert_eq!(demo["proxy_enabled"], true);

        let snapshot = get(&mut console, "/internal/orchestrator/snapshot");
        assert_eq!(snapshot.status, 200);
        assert!(
            snapshot.body["gateway_routes"]
                .as_array()
                .expect("gateway routes")
                .iter()
                .any(|route| {
                    route["prefix"] == "/api/demo"
                        && route["upstream_base"] == "http://127.0.0.1:19200"
                        && route["required_permission"] == "demo.read"
                })
        );
    }

    #[test]
    fn daemon_smoke_seed_route_is_disabled_by_default() {
        let _lock = NODE_INSTALL_ENV_LOCK.lock().expect("env lock");
        unsafe {
            std::env::remove_var("OJOS_SMOKE_MODE");
        }
        let mut console = console();
        let response = post_json(
            &mut console,
            "/internal/smoke/seed-control-plane",
            r#"{
                "root_node_id": "root-node",
                "root_host_ip": "127.0.0.1",
                "child_node_id": "child-node",
                "child_host_ip": "127.0.0.2",
                "storage_service_name": "storage-service",
                "storage_version": "0.1.0",
                "storage_endpoint": "127.0.0.1:19280:storage-service",
                "storage_protocol": "http"
            }"#,
        );
        assert_eq!(response.status, 404);
    }

    #[test]
    fn daemon_smoke_seed_route_uses_real_effective_api_view_when_enabled() {
        let _lock = NODE_INSTALL_ENV_LOCK.lock().expect("env lock");
        unsafe {
            std::env::set_var("OJOS_SMOKE_MODE", "1");
        }
        let mut console = console();
        let response = post_json(
            &mut console,
            "/internal/smoke/seed-control-plane",
            r#"{
                "root_node_id": "root-node",
                "root_host_ip": "127.0.0.1",
                "child_node_id": "child-node",
                "child_host_ip": "127.0.0.2",
                "storage_service_name": "storage-service",
                "storage_version": "0.1.0",
                "storage_endpoint": "127.0.0.1:19280:storage-service",
                "storage_protocol": "http"
            }"#,
        );
        unsafe {
            std::env::remove_var("OJOS_SMOKE_MODE");
        }
        assert_eq!(response.status, 200);
        assert_eq!(response.body["node_id"], "child-node");

        let table = get(
            &mut console,
            "/internal/orchestrator/nodes/child-node/routes?include_upstream=true",
        );
        assert_eq!(table.status, 200);
        let routes = table.body["routes"].as_array().expect("routes");
        assert!(routes.iter().any(|route| {
            route["api_id"] == "storage.object.get"
                && route["provider_node_id"] == "root-node"
                && route["provider_endpoint"] == "127.0.0.1:19280:storage-service"
                && route["upstream_base"] == "http://127.0.0.1:19280"
        }));
    }

    #[test]
    fn daemon_smoke_node_tree_seed_does_not_register_storage_api_surface() {
        let _lock = NODE_INSTALL_ENV_LOCK.lock().expect("env lock");
        unsafe {
            std::env::set_var("OJOS_SMOKE_MODE", "1");
        }
        let mut console = console();
        let response = post_json(
            &mut console,
            "/internal/smoke/seed-node-tree",
            r#"{
                "root_node_id": "root-node",
                "root_host_ip": "127.0.0.1",
                "child_node_id": "child-node",
                "child_host_ip": "127.0.0.2"
            }"#,
        );
        unsafe {
            std::env::remove_var("OJOS_SMOKE_MODE");
        }
        assert_eq!(response.status, 200);
        assert_eq!(response.body["node_id"], "child-node");

        let table = get(
            &mut console,
            "/internal/orchestrator/nodes/child-node/routes?include_upstream=true",
        );
        assert_eq!(table.status, 200);
        assert!(
            table.body["routes"].as_array().expect("routes").is_empty(),
            "node-tree seed must not pre-seed storage API surface or routes"
        );
    }

    #[test]
    fn daemon_node_lifecycle_routes_validate_tree_and_routes() {
        let mut console = console();
        let root = post_json(
            &mut console,
            "/nodes",
            r#"{
                "node_id": "root-node",
                "host_ip": "127.0.0.1",
                "parent_node_id": "",
                "role": "root",
                "labels": {"source": "api-test"},
                "status": "running"
            }"#,
        );
        assert_eq!(root.status, 201);
        assert_eq!(root.body["node"]["node_id"], "root-node");

        let child = post_json(
            &mut console,
            "/nodes",
            r#"{
                "node_id": "child-node",
                "host_ip": "127.0.0.2",
                "parent_node_id": "root-node",
                "role": "node",
                "labels": {"source": "api-test"},
                "status": "running"
            }"#,
        );
        assert_eq!(child.status, 201);
        assert_eq!(child.body["node"]["parent_node_id"], "root-node");

        let orphan = handle_api_request(
            &mut console,
            request(
                "POST",
                "/nodes",
                r#"{
                    "node_id": "orphan-node",
                    "host_ip": "127.0.0.3",
                    "parent_node_id": "missing-node",
                    "role": "node",
                    "status": "running"
                }"#,
            ),
        );
        assert_eq!(orphan.status, 500);
        assert!(
            orphan.body["message"]
                .as_str()
                .unwrap_or("")
                .contains("parent node")
        );

        let cycle = handle_api_request(
            &mut console,
            request(
                "PATCH",
                "/nodes/root-node",
                r#"{
                    "parent_node_id": "child-node",
                    "role": "node"
                }"#,
            ),
        );
        assert_eq!(cycle.status, 500);
        assert!(
            cycle.body["message"]
                .as_str()
                .unwrap_or("")
                .contains("cycle")
        );

        let listed = get(&mut console, "/nodes");
        assert_eq!(listed.status, 200);
        assert_eq!(listed.body["nodes"].as_array().expect("nodes").len(), 2);

        let detail = get(&mut console, "/nodes/child-node");
        assert_eq!(detail.status, 200);
        assert_eq!(detail.body["node"]["host_ip"], "127.0.0.2");

        let install = post_json(
            &mut console,
            "/releases/storage-service/install",
            r#"{
                "operation_id": "op-node-api-storage-install",
                "host_ip": "127.0.0.1",
                "endpoint": "127.0.0.1:19280:storage-service",
                "gateway_node_id": "child-node",
                "execute_service_driver": false,
                "external_service_running": true
            }"#,
        );
        assert_eq!(install.status, 200);
        assert_eq!(install.body["action_result"]["status"], "SUCCEEDED");

        let routes = get(
            &mut console,
            "/nodes/child-node/routes?include_upstream=true",
        );
        assert_eq!(routes.status, 200);
        assert!(
            routes.body["routes"]
                .as_array()
                .expect("routes")
                .iter()
                .any(|route| {
                    route["api_id"] == "storage.object.get"
                        && route["provider_node_id"] == "root-node"
                        && route["upstream_base"] == "http://127.0.0.1:19280"
                })
        );
    }

    #[test]
    fn daemon_endpoint_health_route_dispatches_action() {
        let mut console = console();
        seed_gateway_auth_service_link(&mut console);

        let response = post_json(
            &mut console,
            "/endpoints/127.0.0.1%3A19180%3Agateway/health",
            "{}",
        );
        assert_eq!(response.status, 200);
        assert_eq!(
            response.body["action_result"]["action_id"],
            "endpoint.health.check"
        );
        assert_eq!(response.body["action_result"]["capability_status"], "REAL");
        assert!(
            !response.body["action_result"]["logs"]
                .as_array()
                .expect("logs")
                .is_empty()
        );
    }

    #[test]
    fn daemon_link_health_route_dispatches_action() {
        let mut console = console();
        seed_gateway_auth_service_link(&mut console);

        let response = post_json(
            &mut console,
            "/links/127.0.0.1%3A19180%3Agateway/127.0.0.1%3A19181%3Aauth-service/health",
            "{}",
        );
        assert_eq!(response.status, 200);
        assert_eq!(
            response.body["action_result"]["action_id"],
            "link.health.check"
        );
        assert_eq!(response.body["action_result"]["capability_status"], "REAL");
    }

    #[test]
    fn daemon_templates_route_is_readonly() {
        let mut console = console();
        let response = get(&mut console, "/templates");
        assert_eq!(response.status, 200);
        assert!(
            response.body["templates"]
                .as_array()
                .is_some_and(|items| items.is_empty()),
            "persistent console view must not expose templates as formal objects"
        );
    }

    #[test]
    fn daemon_release_registry_route_exposes_service_release_resources() {
        let mut console = console();
        let install = post_json(
            &mut console,
            "/actions",
            r#"{
                "action": "release.install",
                "operation_id": "op-daemon-release-install",
                "fields": {
                    "service_id": "gateway",
                    "confirm": "true"
                }
            }"#,
        );
        assert_eq!(install.status, 200);
        assert_eq!(install.body["action_result"]["status"], "SUCCEEDED");

        let response = get(&mut console, "/release-registry");
        assert_eq!(response.status, 200);
        let rows = response.body["release_registry"]
            .as_array()
            .expect("release registry rows");
        assert!(rows.iter().any(|row| {
            row["service_name"] == "gateway"
                && row["record_type"] == "route"
                && row["source"] == "store"
        }));
        assert!(
            rows.iter().any(|row| {
                row["service_name"] == "gateway" && row["record_type"] == "frontend"
            })
        );
        assert!(
            rows.iter()
                .any(|row| { row["service_name"] == "gateway" && row["record_type"] == "config" })
        );
    }

    #[test]
    fn daemon_release_create_and_update_actions_register_release_records() {
        let mut console = console();
        let create = post_json(
            &mut console,
            "/actions",
            r#"{
                "action": "release.create",
                "operation_id": "op-daemon-release-create",
                "fields": {
                    "service_id": "gateway",
                    "release_url": "local://daemon-create"
                }
            }"#,
        );
        assert_eq!(create.status, 200);
        assert_eq!(create.body["action_result"]["status"], "SUCCEEDED");
        assert_eq!(
            create.body["action_result"]["capability_status"],
            "STORE_BACKED"
        );

        let update = post_json(
            &mut console,
            "/actions",
            r#"{
                "action": "release.update",
                "operation_id": "op-daemon-release-update",
                "fields": {
                    "service_id": "gateway",
                    "release_url": "local://daemon-update"
                }
            }"#,
        );
        assert_eq!(update.status, 200);
        assert_eq!(update.body["action_result"]["status"], "SUCCEEDED");
        assert_eq!(
            update.body["action_result"]["capability_status"],
            "STORE_BACKED"
        );

        let get_release = post_json(
            &mut console,
            "/actions",
            r#"{
                "action": "release.get",
                "operation_id": "op-daemon-release-get-after-update",
                "fields": {
                    "service_id": "gateway",
                    "version": "0.1.0"
                }
            }"#,
        );
        assert_eq!(get_release.status, 200);
        assert_eq!(
            get_release.body["action_result"]["result"]["release"]["release_url"],
            "local://daemon-update"
        );
    }

    #[test]
    fn daemon_release_routes_dispatch_crud_actions() {
        let mut console = console();
        let create = post_json(
            &mut console,
            "/releases",
            r#"{
                "operation_id": "op-daemon-release-route-create",
                "service_id": "gateway",
                "release_url": "local://daemon-route-create"
            }"#,
        );
        assert_eq!(create.status, 201);
        assert_eq!(create.body["action_result"]["action_id"], "release.create");
        assert_eq!(create.body["action_result"]["status"], "SUCCEEDED");

        let update = route_api_request(
            &mut console,
            request(
                "PATCH",
                "/releases/gateway",
                r#"{
                    "operation_id": "op-daemon-release-route-update",
                    "release_url": "local://daemon-route-update"
                }"#,
            ),
        )
        .expect("release update route");
        assert_eq!(update.status, 200);
        assert_eq!(update.body["action_result"]["action_id"], "release.update");
        assert_eq!(update.body["action_result"]["status"], "SUCCEEDED");

        let get_release = get(&mut console, "/releases/gateway/0.1.0");
        assert_eq!(get_release.status, 200);
        assert_eq!(
            get_release.body["release"]["release_url"],
            "local://daemon-route-update"
        );

        let install = post_json(
            &mut console,
            "/releases/gateway/install",
            r#"{
                "operation_id": "op-daemon-release-route-install"
            }"#,
        );
        assert_eq!(install.status, 200);
        assert_eq!(
            install.body["action_result"]["action_id"],
            "release.install"
        );
        assert_eq!(install.body["action_result"]["status"], "SUCCEEDED");

        let delete = route_api_request(
            &mut console,
            request(
                "DELETE",
                "/releases/gateway/0.1.0",
                r#"{
                    "operation_id": "op-daemon-release-route-delete"
                }"#,
            ),
        )
        .expect("release delete route");
        assert_eq!(delete.status, 200);
        assert_eq!(delete.body["action_result"]["action_id"], "release.delete");
        assert_eq!(delete.body["action_result"]["status"], "SUCCEEDED");
    }

    #[test]
    fn daemon_release_delete_action_updates_release_registry() {
        let mut console = console();
        let install = post_json(
            &mut console,
            "/actions",
            r#"{
                "action": "release.install",
                "operation_id": "op-daemon-release-delete-install",
                "fields": {
                    "service_id": "gateway",
                    "confirm": "true"
                }
            }"#,
        );
        assert_eq!(install.status, 200);
        assert_eq!(install.body["action_result"]["status"], "SUCCEEDED");

        let delete = post_json(
            &mut console,
            "/actions",
            r#"{
                "action": "release.delete",
                "operation_id": "op-daemon-release-delete",
                "fields": {
                    "service_id": "gateway",
                    "version": "0.1.0",
                    "confirm": "true"
                }
            }"#,
        );
        assert_eq!(delete.status, 200);
        assert_eq!(delete.body["action_result"]["status"], "SUCCEEDED");
        assert_eq!(
            delete.body["action_result"]["capability_status"],
            "STORE_BACKED"
        );

        let registry = get(&mut console, "/release-registry");
        let rows = registry.body["release_registry"]
            .as_array()
            .expect("release registry rows");
        assert!(
            !rows
                .iter()
                .any(|row| row["service_name"] == "gateway" && row["source"] == "store"),
            "release.delete should remove gateway store registry rows"
        );
    }

    #[test]
    fn daemon_release_rollback_action_dispatches_install_rollback() {
        let mut console = console();
        let install = post_json(
            &mut console,
            "/actions",
            r#"{
                "action": "release.install",
                "operation_id": "op-daemon-release-rollback-install",
                "fields": {
                    "service_id": "gateway",
                    "confirm": "true"
                }
            }"#,
        );
        assert_eq!(install.status, 200);
        assert_eq!(install.body["action_result"]["status"], "SUCCEEDED");

        let rollback = post_json(
            &mut console,
            "/actions",
            r#"{
                "action": "release.rollback",
                "operation_id": "op-daemon-release-rollback",
                "fields": {
                    "service_id": "gateway",
                    "version": "0.1.0",
                    "target_operation_id": "op-daemon-release-rollback-install",
                    "confirm": "true"
                }
            }"#,
        );
        assert_eq!(rollback.status, 200);
        assert_eq!(rollback.body["action_result"]["status"], "SUCCEEDED");
        assert_eq!(
            rollback.body["action_result"]["capability_status"],
            "STORE_BACKED"
        );

        let target = get(
            &mut console,
            "/operations/op-daemon-release-rollback-install",
        );
        assert_eq!(target.status, 200);
        assert_eq!(target.body["operation"]["status"], "ROLLED_BACK");
    }

    #[test]
    fn daemon_sets_route_is_gone() {
        let mut console = console();
        let response = get(&mut console, "/sets");
        assert_eq!(response.status, 410);
        assert!(
            response.body["message"]
                .as_str()
                .is_some_and(|message| message.contains("use /templates"))
        );
    }

    #[test]
    fn daemon_set_expand_route_is_gone() {
        let mut console = console();
        let response = post_json(&mut console, "/sets/single-node-oj/expand", "{}");
        assert_eq!(response.status, 410);
        assert!(
            response.body["message"]
                .as_str()
                .is_some_and(|message| message.contains("derived endpoint queries"))
        );
    }

    #[test]
    fn daemon_set_apply_route_is_gone() {
        let mut console = console();
        let response = post_json(
            &mut console,
            "/sets/single-node-oj/apply",
            r#"{"operation_id": "op-daemon-set-apply"}"#,
        );
        assert_eq!(response.status, 410);
        assert!(
            response.body["message"]
                .as_str()
                .is_some_and(|message| message.contains("derived endpoint queries"))
        );
    }

    #[test]
    fn daemon_operation_rollback_route_dispatches_action() {
        let mut console = console();
        let response = post_json(
            &mut console,
            "/endpoints",
            r#"{
                "operation_id": "op-daemon-rollback-endpoint",
                "endpoint": "127.0.0.1:19183:gateway",
                "service_id": "gateway",
                "protocol": "http"
            }"#,
        );
        assert_eq!(response.status, 201);

        let rollback = post_json(
            &mut console,
            "/operations/op-daemon-rollback-endpoint/rollback",
            "{}",
        );
        assert_eq!(rollback.status, 200);
        assert_eq!(
            rollback.body["action_result"]["action_id"],
            "endpoint.create"
        );
        assert_eq!(rollback.body["action_result"]["status"], "ROLLED_BACK");
    }

    #[test]
    fn daemon_diagnostics_export_routes_work() {
        let mut console = console();
        let run = post_json(
            &mut console,
            "/diagnostics",
            r#"{"operation_id": "op-daemon-diagnostics"}"#,
        );
        assert_eq!(run.status, 201);
        let report_id = run.body["action_result"]["changed_objects"]
            .as_array()
            .expect("changed objects")
            .iter()
            .find_map(|value| value.as_str())
            .and_then(|value| value.strip_prefix("DiagnosticReport:"))
            .expect("diagnostic report id");

        let report = get(&mut console, &format!("/diagnostics/{report_id}"));
        assert_eq!(report.status, 200);
        assert_eq!(
            report.body["diagnostic_report"]["report_id"],
            json!(report_id)
        );

        let json_export = get(&mut console, &format!("/diagnostics/{report_id}.json"));
        assert_eq!(json_export.status, 200);
        assert_eq!(json_export.body["format"], "json");
        assert!(
            json_export.body["content"]
                .as_str()
                .expect("json content")
                .contains(report_id)
        );

        let markdown_export = get(&mut console, &format!("/diagnostics/{report_id}.md"));
        assert_eq!(markdown_export.status, 200);
        assert_eq!(markdown_export.body["format"], "markdown");
        assert!(
            markdown_export.body["content"]
                .as_str()
                .expect("markdown content")
                .contains("# DiagnosticReport")
        );
    }

    #[test]
    fn daemon_rejects_unknown_routes() {
        let mut console = console();
        let response = route_api_request(&mut console, request("GET", "/unknown", ""))
            .expect("unknown route response");
        assert_eq!(response.status, 404);
        assert_eq!(response.body["status"], "error");
    }

    #[test]
    fn daemon_decodes_http_requests_as_strict_utf8() {
        let request = parse_http_request_bytes(
            b"POST /endpoints HTTP/1.1\r\nContent-Length: 24\r\n\r\n{\"display_name\":\"\xe6\x9c\x8d\xe5\x8a\xa1\"}"
                .to_vec(),
        )
        .expect("utf8 request");
        assert!(request.body.contains("服务"));

        let err = parse_http_request_bytes(
            b"POST /endpoints HTTP/1.1\r\nContent-Length: 1\r\n\r\n\xff".to_vec(),
        )
        .expect_err("non-UTF-8 body should fail");
        assert!(err.to_string().contains("invalid utf-8"));
    }

    #[test]
    fn daemon_source_avoids_forbidden_boundary_terms() {
        let source =
            std::fs::read_to_string(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/main.rs"))
                .expect("daemon source");
        for forbidden in forbidden_boundary_terms() {
            assert!(
                !source.contains(&forbidden),
                "daemon source must not contain forbidden term {forbidden}"
            );
        }
    }

    fn forbidden_boundary_terms() -> Vec<String> {
        [
            ["Ma", "chine"].concat(),
            ["De", "vice"].concat(),
            ["Service", "Installation"].concat(),
            ["Service", "Package"].concat(),
            ["Root", "Runtime", "Manager"].concat(),
            ["Root ", "Runtime ", "Manager"].concat(),
            ["oj", "os", "ctl"].concat(),
            ["shared", "-", "ui"].concat(),
            ["kernel", "/", "installer"].concat(),
            ["Runtime ", "Manager"].concat(),
            ["Module", "-first"].concat(),
            ["module", "-first"].concat(),
            ["Installer", "-first"].concat(),
            ["installer", "-first"].concat(),
        ]
        .to_vec()
    }
}
