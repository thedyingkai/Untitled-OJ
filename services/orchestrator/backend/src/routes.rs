//! 编排器控制面路由：URL 到 core 动作的映射，以及路由用到的请求体解析与视图辅助函数。
//!
//! 本模块只做“HTTP 语义 → core 动作”的翻译，业务规则一律留在 core 里。

#[cfg(feature = "legacy-0_2")]
use crate::auth::require_node_install_credentials;
use crate::auth::{ORCHESTRATOR_INTERNAL_TOKEN_HEADER, internal_token_check};
use crate::http::{ApiRequest, ApiResponse, StatusError, path_segments, query_bool, query_value};
use anyhow::Result;
#[cfg(feature = "legacy-0_2")]
use orchestrator_legacy::NodeServiceDispatchRequest;
use orchestrator_legacy::{
    ActionRequest, EffectiveApiRoute, Endpoint, NodeRecord, OrchestratorActionConsole,
    OrchestratorError, ServiceRoute, parse_endpoint_id, validate_endpoint_id,
};
use serde_json::{Value, json};
use std::collections::BTreeMap;

/// 错误分级：显式标注的 [`StatusError`] 用它自带的状态码；请求体或
/// manifest 校验失败按 400；不允许当前状态执行的动作按 409。依赖、IO 等
/// 服务端故障仍按 500 上报。
pub(crate) fn status_for_error(err: &anyhow::Error) -> u16 {
    if let Some(status) = err.downcast_ref::<orchestrator_manager::StoreRequestError>() {
        return status.status();
    }
    if let Some(status) = err.downcast_ref::<StatusError>() {
        return status.0;
    }
    if let Some(core_error) = err.downcast_ref::<OrchestratorError>() {
        return match core_error {
            OrchestratorError::InvalidManifest(_)
            | OrchestratorError::UnsafePath(_)
            | OrchestratorError::Yaml(_)
            | OrchestratorError::Json(_) => 400,
            OrchestratorError::Blocked(_) => 409,
            // Infrastructure adapters redact I/O details and map them to the
            // domain-level dependency failure before crossing this boundary.
            OrchestratorError::Dependency(_) => 500,
        };
    }
    if err.downcast_ref::<serde_json::Error>().is_some() {
        return 400;
    }
    500
}

pub(crate) fn handle_api_request_with_internal_token(
    console: &mut OrchestratorActionConsole,
    request: ApiRequest,
    expected_internal_token: Option<&str>,
) -> ApiResponse {
    match route_api_request_with_internal_token(console, request, expected_internal_token) {
        Ok(response) => response,
        Err(err) => ApiResponse::error(status_for_error(&err), err.to_string()),
    }
}

fn route_api_request_with_internal_token(
    console: &mut OrchestratorActionConsole,
    request: ApiRequest,
    expected_internal_token: Option<&str>,
) -> Result<ApiResponse> {
    let path = request.path.split('?').next().unwrap_or("/");
    let query = request
        .path
        .split_once('?')
        .map(|(_, query)| query)
        .unwrap_or("");
    let segments = path_segments(path).map_err(|err| StatusError::new(400, err.to_string()))?;
    let segment_refs = segments.iter().map(String::as_str).collect::<Vec<_>>();
    internal_token_check(
        request.method.as_str(),
        segment_refs.as_slice(),
        request
            .headers
            .get(ORCHESTRATOR_INTERNAL_TOKEN_HEADER)
            .map(String::as_str),
        expected_internal_token,
    )?;
    match (request.method.as_str(), segment_refs.as_slice()) {
        #[cfg(feature = "legacy-0_2")]
        ("POST", ["api", "node", "services", "install"]) => {
            require_node_install_credentials(&request, expected_internal_token)?;
            let request = serde_json::from_str::<NodeServiceDispatchRequest>(&request.body)?;
            Ok(ApiResponse::ok(json!({
                "node_dispatch_result": console.accept_node_service_install(request)?,
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
        ("GET", ["deployments"]) => Ok(ApiResponse::ok(json!({
            "deployments": console.view()?.deployments,
        }))),
        ("GET", ["nodes"]) => Ok(ApiResponse::ok(json!({
            "nodes": console.nodes()?,
        }))),
        ("POST", ["nodes"]) => {
            let node = node_record_from_body(&request.body, None, None)?;
            ensure_node_parent_exists_for_http(console, &node)?;
            let node = console.upsert_node(node)?;
            Ok(ApiResponse::created(json!({
                "node": node,
            })))
        }
        ("GET", ["nodes", node_id, "routes"]) => {
            Ok(ApiResponse::ok(json!(internal_effective_route_table(
                console,
                node_id,
                query_bool(query, "include_upstream")?
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
            ensure_node_parent_exists_for_http(console, &node)?;
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
            "service_definitions": internal_service_definitions(console, query_bool(query, "include_disabled")?),
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
            if let Some(node_id) = query_value(query, "node_id")? {
                Ok(ApiResponse::ok(json!(internal_effective_route_table(
                    console,
                    &node_id,
                    query_bool(query, "include_upstream")?
                )?)))
            } else {
                Ok(ApiResponse::ok(json!(internal_route_table(
                    console,
                    query_bool(query, "include_disabled")?,
                    query_bool(query, "include_upstream")?
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
                query_bool(query, "include_upstream")?
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
            let action = action_from_body(console, "release.create", &request.body, [])?;
            Ok(ApiResponse::created(json!({
                "action_result": console.dispatch(action)?,
            })))
        }
        ("PATCH", ["releases", service_name]) => {
            let action = action_from_body(
                console,
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
                console,
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
                console,
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
                console,
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
                console,
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
                console,
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
            let action = action_from_body(console, "endpoint.create", &request.body, [])?;
            Ok(ApiResponse::created(json!({
                "action_result": console.dispatch(action)?,
            })))
        }
        ("POST", ["actions"]) => {
            let action = action_request_from_body(console, &request.body)?;
            Ok(ApiResponse::ok(json!({
                "action_result": console.dispatch(action)?,
            })))
        }
        ("PATCH", ["endpoints", endpoint]) => {
            validate_endpoint_id(endpoint)?;
            let action = action_from_body(
                console,
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
                console,
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
                console,
                "endpoint.health.check",
                &request.body,
                [("endpoint", *endpoint)],
            )?;
            Ok(ApiResponse::ok(json!({
                "action_result": console.dispatch(action)?,
            })))
        }
        ("POST", ["endpoints", "health"]) => {
            let action = action_from_body(console, "endpoint.health.check", &request.body, [])?;
            Ok(ApiResponse::ok(json!({
                "action_result": console.dispatch(action)?,
            })))
        }
        ("GET", ["links"]) => Ok(ApiResponse::ok(json!({
            "links": console.view()?.links,
        }))),
        ("POST", ["links"]) => {
            let action =
                action_from_body(console, "link.create", &request.body, [("confirm", "true")])?;
            Ok(ApiResponse::created(json!({
                "action_result": console.dispatch(action)?,
            })))
        }
        ("PATCH", ["links", source, target]) => {
            validate_endpoint_id(source)?;
            validate_endpoint_id(target)?;
            let action = action_from_body(
                console,
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
                console,
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
        ("POST", ["links", source, target, "enable"]) => {
            validate_endpoint_id(source)?;
            validate_endpoint_id(target)?;
            let action = action_from_body(
                console,
                "link.enable",
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
        ("POST", ["links", source, target, "disable"]) => {
            validate_endpoint_id(source)?;
            validate_endpoint_id(target)?;
            let action = action_from_body(
                console,
                "link.disable",
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
        ("POST", ["links", source, target, "health"]) => {
            validate_endpoint_id(source)?;
            validate_endpoint_id(target)?;
            let action = action_from_body(
                console,
                "link.health.check",
                &request.body,
                [("source_endpoint", *source), ("target_endpoint", *target)],
            )?;
            Ok(ApiResponse::ok(json!({
                "action_result": console.dispatch(action)?,
            })))
        }
        ("POST", ["links", "health"]) => {
            let action = action_from_body(console, "link.health.check", &request.body, [])?;
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
            let action = action_from_body(console, "operation.create", &request.body, [])?;
            Ok(ApiResponse::created(json!({
                "action_result": console.dispatch(action)?,
            })))
        }
        ("GET", ["operations", operation_id]) => {
            let operation = console.operation(operation_id)?.ok_or_else(|| {
                StatusError::new(404, format!("operation {operation_id} not found"))
            })?;
            Ok(ApiResponse::ok(json!({ "operation": operation })))
        }
        ("POST", ["operations", operation_id, "confirm"]) => {
            let action =
                operation_action(console, "operation.confirm", operation_id, &request.body)?;
            Ok(ApiResponse::ok(json!({
                "action_result": console.dispatch(action)?,
            })))
        }
        ("POST", ["operations", operation_id, "apply"]) => {
            let action = operation_action(console, "operation.apply", operation_id, &request.body)?;
            Ok(ApiResponse::ok(json!({
                "action_result": console.dispatch(action)?,
            })))
        }
        ("POST", ["operations", operation_id, "rollback"]) => {
            let action =
                operation_action(console, "operation.rollback", operation_id, &request.body)?;
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
            let action = action_from_body(console, "diagnostic.create", &request.body, [])?;
            Ok(ApiResponse::created(json!({
                "action_result": console.dispatch(action)?,
            })))
        }
        ("GET", ["diagnostics"]) => {
            let cursor = query_value(query, "cursor")
                .map_err(|error| StatusError::new(400, error.to_string()))?
                .unwrap_or_default();
            let limit = query_value(query, "limit")
                .map_err(|error| StatusError::new(400, error.to_string()))?
                .map(|value| value.parse::<usize>())
                .transpose()
                .map_err(|_| StatusError::new(400, "limit must be an integer"))?
                .unwrap_or(50);
            if !(1..=200).contains(&limit) {
                return Err(StatusError::new(400, "limit must be between 1 and 200").into());
            }
            let mut reports = console.diagnostic_reports()?;
            reports.sort_by(|left, right| left.report_id.cmp(&right.report_id));
            let mut reports = reports
                .into_iter()
                .filter(|report| report.report_id.as_str() > cursor.as_str())
                .take(limit + 1)
                .collect::<Vec<_>>();
            let next_cursor = if reports.len() > limit {
                reports.truncate(limit);
                reports.last().map(|report| report.report_id.clone())
            } else {
                None
            };
            Ok(ApiResponse::ok(json!({
                "items": reports,
                "next_cursor": next_cursor,
            })))
        }
        ("GET", [report_file]) if report_file.ends_with(".json") => {
            let report_id = report_file.trim_end_matches(".json");
            require_diagnostic_report(console, report_id)?;
            let export = console.diagnostic_export(report_id, "json")?;
            Ok(ApiResponse::ok(json!({
                "report_id": export.report_id,
                "format": export.format,
                "content": export.content,
            })))
        }
        ("GET", [report_file]) if report_file.ends_with(".md") => {
            let report_id = report_file.trim_end_matches(".md");
            require_diagnostic_report(console, report_id)?;
            let export = console.diagnostic_export(report_id, "markdown")?;
            Ok(ApiResponse::ok(json!({
                "report_id": export.report_id,
                "format": export.format,
                "content": export.content,
            })))
        }
        ("GET", ["diagnostics", report_file]) if report_file.ends_with(".json") => {
            let report_id = report_file.trim_end_matches(".json");
            require_diagnostic_report(console, report_id)?;
            let export = console.diagnostic_export(report_id, "json")?;
            Ok(ApiResponse::ok(json!({
                "report_id": export.report_id,
                "format": export.format,
                "content": export.content,
            })))
        }
        ("GET", ["diagnostics", report_file]) if report_file.ends_with(".md") => {
            let report_id = report_file.trim_end_matches(".md");
            require_diagnostic_report(console, report_id)?;
            let export = console.diagnostic_export(report_id, "markdown")?;
            Ok(ApiResponse::ok(json!({
                "report_id": export.report_id,
                "format": export.format,
                "content": export.content,
            })))
        }
        ("GET", ["diagnostics", report_id]) => {
            let report = console.diagnostic_report(report_id)?.ok_or_else(|| {
                StatusError::new(404, format!("diagnostic report {report_id} not found"))
            })?;
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

fn require_diagnostic_report(console: &OrchestratorActionConsole, report_id: &str) -> Result<()> {
    if console.diagnostic_report(report_id)?.is_none() {
        return Err(
            StatusError::new(404, format!("diagnostic report {report_id} not found")).into(),
        );
    }
    Ok(())
}

fn action_request_from_body(
    console: &OrchestratorActionConsole,
    body: &str,
) -> Result<ActionRequest> {
    let trimmed = body.trim();
    if trimmed.is_empty() {
        return Err(StatusError::new(400, "POST /actions requires a JSON body").into());
    }
    let value = serde_json::from_str::<Value>(trimmed)?;
    let object = value
        .as_object()
        .ok_or_else(|| StatusError::new(400, "request body must be a JSON object"))?;
    let action = object
        .get("action")
        .or_else(|| object.get("action_id"))
        .and_then(Value::as_str)
        .ok_or_else(|| StatusError::new(400, "POST /actions requires action"))?;
    let mut request = empty_http_action_request(console, action)?;
    merge_action_body(&mut request, object)?;
    validate_required_action_fields(console, &request)?;
    Ok(request)
}

fn action_from_body<const N: usize>(
    console: &OrchestratorActionConsole,
    action: &str,
    body: &str,
    overrides: [(&str, &str); N],
) -> Result<ActionRequest> {
    let mut request = empty_http_action_request(console, action)?;
    if let Some(object) = action_body_object(body)? {
        merge_action_body(&mut request, &object)?;
    }
    for (key, value) in overrides {
        request.fields.insert(key.to_string(), value.to_string());
    }
    validate_required_action_fields(console, &request)?;
    Ok(request)
}

fn operation_action(
    console: &OrchestratorActionConsole,
    action: &str,
    operation_id: &str,
    body: &str,
) -> Result<ActionRequest> {
    let mut request = empty_http_action_request(console, action)?;
    if let Some(object) = action_body_object(body)? {
        merge_action_body(&mut request, &object)?;
    }
    request
        .fields
        .insert("operation_id".to_string(), operation_id.to_string());
    request
        .fields
        .insert("confirm".to_string(), "true".to_string());
    validate_required_action_fields(console, &request)?;
    Ok(request)
}

pub(crate) fn empty_http_action_request(
    console: &OrchestratorActionConsole,
    action: &str,
) -> Result<ActionRequest> {
    let action = action.trim();
    if action.is_empty() {
        return Err(StatusError::new(400, "action must not be empty").into());
    }
    if console.action_form(action).is_none() {
        return Err(StatusError::new(400, format!("unknown action {action}")).into());
    }
    Ok(ActionRequest::new("", action, BTreeMap::new()))
}

fn action_body_object(body: &str) -> Result<Option<serde_json::Map<String, Value>>> {
    let trimmed = body.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    let value = serde_json::from_str::<Value>(trimmed)?;
    let object = value
        .as_object()
        .ok_or_else(|| StatusError::new(400, "request body must be a JSON object"))?;
    Ok(Some(object.clone()))
}

fn merge_action_body(
    request: &mut ActionRequest,
    object: &serde_json::Map<String, Value>,
) -> Result<()> {
    for (key, value) in object {
        match key.as_str() {
            "action" | "action_id" => {}
            "operation_id" => request.operation_id = field_value(value)?,
            "fields" => merge_json_fields(&mut request.fields, value)?,
            _ => {
                request.fields.insert(key.clone(), field_value(value)?);
            }
        }
    }
    Ok(())
}

pub(crate) fn validate_required_action_fields(
    console: &OrchestratorActionConsole,
    request: &ActionRequest,
) -> Result<()> {
    let form = console
        .action_form(&request.action)
        .ok_or_else(|| StatusError::new(400, format!("unknown action {}", request.action)))?;
    let missing = form
        .fields
        .iter()
        .filter(|field| field.required && request.field(&field.name).is_none())
        .map(|field| field.name.as_str())
        .collect::<Vec<_>>();
    if !missing.is_empty() {
        return Err(StatusError::new(
            400,
            format!(
                "{} requires form field{} {}",
                request.action,
                if missing.len() == 1 { "" } else { "s" },
                missing.join(", ")
            ),
        )
        .into());
    }
    Ok(())
}

fn node_record_from_body(
    body: &str,
    path_node_id: Option<&str>,
    existing: Option<NodeRecord>,
) -> Result<NodeRecord> {
    let value = serde_json::from_str::<Value>(body.trim())?;
    let object = value
        .as_object()
        .ok_or_else(|| StatusError::new(400, "node request body must be a JSON object"))?;
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
                return Err(StatusError::new(400, "node_id body/path mismatch").into());
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

fn ensure_node_parent_exists_for_http(
    console: &OrchestratorActionConsole,
    node: &NodeRecord,
) -> Result<()> {
    let parent_node_id = node.parent_node_id.trim();
    if node.role == "node" && !parent_node_id.is_empty() && console.node(parent_node_id)?.is_none()
    {
        return Err(StatusError::new(
            400,
            format!("invalid node topology: parent node {parent_node_id} not found"),
        )
        .into());
    }
    Ok(())
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

/// Web 服务页使用的部署视图。一行严格对应一条 HostService，而不是 Service manifest
/// 注册表；这样同一服务部署到两台主机时不会被折叠成一个含糊的“服务”按钮。
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

fn merge_json_fields(fields: &mut BTreeMap<String, String>, value: &Value) -> Result<()> {
    let object = value
        .as_object()
        .ok_or_else(|| StatusError::new(400, "fields must be a JSON object"))?;
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
