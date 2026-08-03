//! 编排器控制面路由：URL 到 core 动作的映射，以及路由用到的请求体解析与视图辅助函数。
//!
//! 本模块只做“HTTP 语义 → core 动作”的翻译，业务规则一律留在 core 里。

#[cfg(test)]
use crate::auth::configured_internal_token;
#[cfg(any(feature = "legacy-0_2", test))]
use crate::auth::require_node_install_credentials;
use crate::auth::{ORCHESTRATOR_INTERNAL_TOKEN_HEADER, internal_token_check, smoke_mode_enabled};
use crate::http::{ApiRequest, ApiResponse, StatusError, path_segments, query_bool, query_value};
use anyhow::Result;
#[cfg(any(feature = "legacy-0_2", test))]
use orchestrator_legacy::NodeServiceDispatchRequest;
use orchestrator_legacy::{
    ActionRequest, EffectiveApiRoute, Endpoint, NodeRecord, OrchestratorActionConsole,
    OrchestratorError, ServiceRoute, SmokeControlPlaneSeed, SmokeNodeTreeSeed, parse_endpoint_id,
    validate_endpoint_id,
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

#[cfg(test)]
pub(crate) fn handle_api_request(
    console: &mut OrchestratorActionConsole,
    request: ApiRequest,
) -> ApiResponse {
    let internal_token = configured_internal_token();
    handle_api_request_with_internal_token(console, request, internal_token.as_deref())
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

#[cfg(test)]
pub(crate) fn route_api_request(
    console: &mut OrchestratorActionConsole,
    request: ApiRequest,
) -> Result<ApiResponse> {
    let internal_token = configured_internal_token();
    route_api_request_with_internal_token(console, request, internal_token.as_deref())
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
        #[cfg(any(feature = "legacy-0_2", test))]
        ("POST", ["api", "node", "services", "install"]) => {
            require_node_install_credentials(&request, expected_internal_token)?;
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_env::TestEnv;
    use std::io::{ErrorKind, Read, Write};
    use std::net::TcpListener;
    use std::ops::{Deref, DerefMut};
    use std::path::PathBuf;
    use std::thread;
    use std::time::{Duration, Instant};

    const ROUTE_TEST_ENV: [&str; 6] = [
        "ORCHESTRATOR_INTERNAL_TOKEN",
        "ORCHESTRATOR_NODE_TOKEN",
        "ORCHESTRATOR_NODE_EXECUTE_SERVICE_DRIVER",
        "ORCHESTRATOR_NODE_HOST_IP",
        "OJOS_ORCHESTRATOR_DOCKER_BINARY",
        "OJOS_SMOKE_MODE",
    ];

    /// 路由会在请求期间读取进程环境，因此测试 console 持有共享环境锁直到测试结束。
    /// 这也让默认并行 `cargo test` 不会观察到其他测试的临时令牌。
    struct TestConsole {
        console: OrchestratorActionConsole,
        _env: TestEnv,
    }

    impl Deref for TestConsole {
        type Target = OrchestratorActionConsole;

        fn deref(&self) -> &Self::Target {
            &self.console
        }
    }

    impl DerefMut for TestConsole {
        fn deref_mut(&mut self) -> &mut Self::Target {
            &mut self.console
        }
    }

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

    fn console_with_env(overrides: &[(&str, Option<&str>)]) -> TestConsole {
        let mut env = TestEnv::lock();
        for name in ROUTE_TEST_ENV {
            env.remove(name);
        }
        for (name, value) in overrides {
            env.apply(name, *value);
        }
        let console = OrchestratorActionConsole::load_with_database_url(repo_root(), None)
            .expect("daemon console");
        TestConsole { console, _env: env }
    }

    fn console() -> TestConsole {
        console_with_env(&[])
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

    #[test]
    fn operation_route_action_keeps_options_and_uses_path_target() {
        let console = console();
        let request = operation_action(
            &console,
            "operation.rollback",
            "op-target",
            r#"{
                "operation_id": "op-request",
                "confirm": false,
                "execute_service_driver": true,
                "gateway_node_id": "gateway-node",
                "fields": {
                    "operation_id": "op-field-must-not-win"
                }
            }"#,
        )
        .expect("operation route action");

        assert_eq!(request.operation_id, "op-request");
        assert_eq!(request.field("operation_id"), Some("op-target"));
        assert_eq!(request.field("confirm"), Some("true"));
        assert_eq!(request.field("execute_service_driver"), Some("true"));
        assert_eq!(request.field("gateway_node_id"), Some("gateway-node"));
    }

    #[test]
    fn convenience_action_path_fields_override_body_without_inheriting_defaults() {
        let console = console();
        let request = action_from_body(
            &console,
            "release.delete",
            r#"{
                "operation_id": "op-release-delete-request",
                "service_id": "body-service",
                "version": "",
                "confirm": false
            }"#,
            [("service_id", "path-service"), ("confirm", "true")],
        )
        .expect("release delete request");

        assert_eq!(request.operation_id, "op-release-delete-request");
        assert_eq!(request.field("service_id"), Some("path-service"));
        assert_eq!(request.field("confirm"), Some("true"));
        assert_eq!(request.field("version"), None);
        assert_eq!(request.field("endpoint"), None);
        assert_eq!(request.field("gateway_node_id"), None);
    }

    fn node_install_body_for(
        service_yaml: &str,
        operation_id: &str,
        host_ip: &str,
        port: u16,
        execute_service_driver: bool,
    ) -> String {
        let root = repo_root();
        let service = orchestrator_legacy::validate_service_manifest_file(
            &root,
            std::path::Path::new(service_yaml),
        )
        .expect("service manifest");
        let endpoint_id = format!("{host_ip}:{port}:{}", service.id);
        serde_json::to_string(&NodeServiceDispatchRequest {
            operation_id: operation_id.to_string(),
            execute_service_driver,
            service: service.clone(),
            release: None,
            host_service: orchestrator_legacy::HostService {
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
            false,
        )
    }

    fn authorized_node_install_body() -> String {
        node_install_body_for(
            "services/gateway/service.yaml",
            "op-node-install-gateway",
            "10.10.0.7",
            8080,
            true,
        )
    }

    fn node_install_request_with_credentials(body: &str) -> ApiRequest {
        let mut api_request = request("POST", "/api/node/services/install", body);
        api_request.headers.insert(
            "authorization".to_string(),
            "Bearer node-secret".to_string(),
        );
        api_request.headers.insert(
            ORCHESTRATOR_INTERNAL_TOKEN_HEADER.to_string(),
            "control-secret".to_string(),
        );
        api_request
    }

    #[test]
    fn daemon_node_install_route_accepts_dispatched_service() {
        let mut console = console();
        let response = route_api_request(
            &mut console,
            request("POST", "/api/node/services/install", &node_install_body()),
        )
        .expect("node install response");

        assert_eq!(response.status, 200);
        assert_eq!(response.body["node_dispatch_result"]["accepted"], true);
        assert_eq!(
            response.body["node_dispatch_result"]["driver_executed"],
            json!(false)
        );
        assert_eq!(
            response.body["node_dispatch_result"]["driver_status"],
            json!("DEFERRED")
        );
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
        let mut console = console_with_env(&[("ORCHESTRATOR_NODE_TOKEN", Some("node-secret"))]);
        let response = handle_api_request(
            &mut console,
            request("POST", "/api/node/services/install", &node_install_body()),
        );

        // 鉴权失败必须是 401，不能再和内部故障一样报 500。
        assert_eq!(response.status, 401);
        assert!(
            response.body["message"]
                .as_str()
                .unwrap_or("")
                .contains("unauthorized")
        );
    }

    #[test]
    fn daemon_node_install_route_accepts_node_and_control_plane_tokens_together() {
        let mut console = console_with_env(&[("ORCHESTRATOR_NODE_TOKEN", Some("node-secret"))]);
        let mut api_request = request("POST", "/api/node/services/install", &node_install_body());
        api_request.headers.insert(
            "authorization".to_string(),
            "Bearer node-secret".to_string(),
        );
        api_request.headers.insert(
            ORCHESTRATOR_INTERNAL_TOKEN_HEADER.to_string(),
            "control-secret".to_string(),
        );
        let response = route_api_request_with_internal_token(
            &mut console,
            api_request,
            Some("control-secret"),
        )
        .expect("node install with both credentials");

        assert_eq!(response.status, 200);
        assert_eq!(response.body["node_dispatch_result"]["accepted"], true);
    }

    #[test]
    fn daemon_node_driver_mode_requires_configured_node_token() {
        let mut console = console_with_env(&[
            ("ORCHESTRATOR_INTERNAL_TOKEN", Some("control-secret")),
            ("ORCHESTRATOR_NODE_EXECUTE_SERVICE_DRIVER", Some("true")),
        ]);
        let response = handle_api_request(
            &mut console,
            node_install_request_with_credentials(&authorized_node_install_body()),
        );

        assert_eq!(response.status, 401);
        assert!(
            response.body["message"]
                .as_str()
                .unwrap_or("")
                .contains("ORCHESTRATOR_NODE_TOKEN")
        );
    }

    #[test]
    fn daemon_node_driver_mode_requires_configured_internal_token() {
        let mut console = console_with_env(&[
            ("ORCHESTRATOR_NODE_TOKEN", Some("node-secret")),
            ("ORCHESTRATOR_NODE_EXECUTE_SERVICE_DRIVER", Some("true")),
        ]);
        let response = handle_api_request(
            &mut console,
            node_install_request_with_credentials(&authorized_node_install_body()),
        );

        assert_eq!(response.status, 401);
        assert!(
            response.body["message"]
                .as_str()
                .unwrap_or("")
                .contains("ORCHESTRATOR_INTERNAL_TOKEN")
        );
    }

    #[test]
    fn daemon_node_driver_mode_requires_bound_host_identity() {
        let mut console = console_with_env(&[
            ("ORCHESTRATOR_NODE_TOKEN", Some("node-secret")),
            ("ORCHESTRATOR_INTERNAL_TOKEN", Some("control-secret")),
            ("ORCHESTRATOR_NODE_EXECUTE_SERVICE_DRIVER", Some("true")),
        ]);
        let response = handle_api_request(
            &mut console,
            node_install_request_with_credentials(&authorized_node_install_body()),
        );

        assert_eq!(response.status, 409);
        assert!(
            response.body["message"]
                .as_str()
                .unwrap_or("")
                .contains("ORCHESTRATOR_NODE_HOST_IP")
        );
    }

    #[test]
    fn daemon_node_driver_mode_rejects_mismatched_host_identity() {
        let mut console = console_with_env(&[
            ("ORCHESTRATOR_NODE_TOKEN", Some("node-secret")),
            ("ORCHESTRATOR_INTERNAL_TOKEN", Some("control-secret")),
            ("ORCHESTRATOR_NODE_EXECUTE_SERVICE_DRIVER", Some("true")),
            ("ORCHESTRATOR_NODE_HOST_IP", Some("10.10.0.99")),
        ]);
        let response = handle_api_request(
            &mut console,
            node_install_request_with_credentials(&authorized_node_install_body()),
        );

        assert_eq!(response.status, 409);
        assert!(
            response.body["message"]
                .as_str()
                .unwrap_or("")
                .contains("target identity mismatch")
        );
    }

    #[test]
    fn daemon_node_install_route_runs_driver_when_enabled() {
        let mut console = console_with_env(&[
            ("ORCHESTRATOR_NODE_TOKEN", Some("node-secret")),
            ("ORCHESTRATOR_INTERNAL_TOKEN", Some("control-secret")),
            ("ORCHESTRATOR_NODE_EXECUTE_SERVICE_DRIVER", Some("true")),
            ("ORCHESTRATOR_NODE_HOST_IP", Some("10.10.0.7")),
            (
                "OJOS_ORCHESTRATOR_DOCKER_BINARY",
                Some("ojos-docker-compose-missing"),
            ),
        ]);
        let response = route_api_request(
            &mut console,
            node_install_request_with_credentials(&authorized_node_install_body()),
        )
        .expect("node install response");

        assert_eq!(response.status, 200);
        assert_eq!(response.body["node_dispatch_result"]["accepted"], true);
        assert_eq!(
            response.body["node_dispatch_result"]["driver_executed"],
            json!(true)
        );
        assert_eq!(
            response.body["node_dispatch_result"]["driver_status"],
            json!("FAILED")
        );
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
    fn daemon_node_install_route_never_runs_driver_without_request_authorization() {
        let mut console = console_with_env(&[
            ("ORCHESTRATOR_NODE_TOKEN", Some("node-secret")),
            ("ORCHESTRATOR_INTERNAL_TOKEN", Some("control-secret")),
            ("ORCHESTRATOR_NODE_EXECUTE_SERVICE_DRIVER", Some("true")),
            (
                "OJOS_ORCHESTRATOR_DOCKER_BINARY",
                Some("ojos-docker-compose-must-not-run"),
            ),
        ]);
        let response = route_api_request(
            &mut console,
            node_install_request_with_credentials(&node_install_body()),
        )
        .expect("unauthorized node install response");

        assert_eq!(response.status, 200);
        assert_eq!(
            response.body["node_dispatch_result"]["driver_executed"],
            json!(false)
        );
        assert_eq!(
            response.body["node_dispatch_result"]["driver_status"],
            json!("DEFERRED")
        );
        assert!(
            response.body["node_dispatch_result"]["message"]
                .as_str()
                .unwrap_or("")
                .contains("driver execution deferred")
        );
        let operation = get(&mut console, "/operations/op-node-install-gateway");
        assert_eq!(operation.body["operation"]["status"], json!("SUCCEEDED"));
        let logs = get(&mut console, "/operations/op-node-install-gateway/logs");
        let logs = logs.body["logs"].as_array().expect("operation logs");
        assert!(logs.iter().any(|log| {
            log["step_id"] == "node-driver"
                && log["data"]["request_authorized"] == json!(false)
                && log["data"]["node_execution_enabled"] == json!(true)
        }));
    }

    #[test]
    fn daemon_node_install_route_requires_node_execution_ceiling() {
        let mut console = console_with_env(&[(
            "OJOS_ORCHESTRATOR_DOCKER_BINARY",
            Some("ojos-docker-compose-must-not-run"),
        )]);
        let response = handle_api_request(
            &mut console,
            request(
                "POST",
                "/api/node/services/install",
                &authorized_node_install_body(),
            ),
        );

        assert_eq!(response.status, 409);
        assert!(
            response.body["message"]
                .as_str()
                .unwrap_or("")
                .contains("ORCHESTRATOR_NODE_EXECUTE_SERVICE_DRIVER")
        );
        let operation = get(&mut console, "/operations/op-node-install-gateway");
        assert_eq!(operation.body["operation"]["status"], json!("FAILED"));
        let logs = get(&mut console, "/operations/op-node-install-gateway/logs");
        let logs = logs.body["logs"].as_array().expect("operation logs");
        assert!(logs.iter().any(|log| {
            log["step_id"] == "node-driver"
                && log["level"] == json!("error")
                && log["message"]
                    .as_str()
                    .unwrap_or("")
                    .contains("ORCHESTRATOR_NODE_EXECUTE_SERVICE_DRIVER")
        }));
    }

    #[test]
    fn daemon_node_install_route_records_driver_setup_failure() {
        let mut console = console_with_env(&[
            ("ORCHESTRATOR_NODE_TOKEN", Some("node-secret")),
            ("ORCHESTRATOR_INTERNAL_TOKEN", Some("control-secret")),
            ("ORCHESTRATOR_NODE_EXECUTE_SERVICE_DRIVER", Some("true")),
            ("ORCHESTRATOR_NODE_HOST_IP", Some("10.10.0.8")),
        ]);
        let response = handle_api_request(
            &mut console,
            node_install_request_with_credentials(&node_install_body_for(
                "services/orchestrator/service.yaml",
                "op-node-install-orchestrator",
                "10.10.0.8",
                8090,
                true,
            )),
        );

        assert_eq!(response.status, 409);
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
        let mut request = request("GET", path, "");
        if let Some(token) = configured_internal_token() {
            request
                .headers
                .insert(ORCHESTRATOR_INTERNAL_TOKEN_HEADER.to_string(), token);
        }
        route_api_request(console, request).expect("GET response")
    }

    #[test]
    fn deployments_route_keeps_same_service_on_two_hosts_as_two_precise_rows() {
        let mut console = console();
        for (suffix, root_host, child_host, port) in [
            ("a", "10.77.0.1", "10.77.0.2", 19001),
            ("b", "10.78.0.1", "10.78.0.2", 19002),
        ] {
            console
                .seed_smoke_control_plane(SmokeControlPlaneSeed {
                    root_node_id: format!("deploy-root-{suffix}"),
                    root_host_ip: root_host.to_string(),
                    child_node_id: format!("deploy-child-{suffix}"),
                    child_host_ip: child_host.to_string(),
                    storage_service_name: "multi-host-service".to_string(),
                    storage_version: "1.2.3".to_string(),
                    storage_endpoint: format!("{root_host}:{port}:multi-host-service"),
                    storage_protocol: "http".to_string(),
                })
                .expect("seed deployment");
        }

        let response = get(&mut console, "/deployments");
        assert_eq!(response.status, 200);
        let mut deployments = response.body["deployments"]
            .as_array()
            .expect("deployment rows")
            .iter()
            .filter(|row| row["service_id"] == "multi-host-service")
            .collect::<Vec<_>>();
        deployments.sort_by_key(|row| row["host_ip"].as_str().unwrap_or("").to_string());

        assert_eq!(deployments.len(), 2);
        assert_eq!(deployments[0]["host_ip"], "10.77.0.1");
        assert_eq!(
            deployments[0]["endpoint"],
            "10.77.0.1:19001:multi-host-service"
        );
        assert_eq!(deployments[0]["version"], "1.2.3");
        assert_eq!(deployments[0]["status"], "running");
        assert_eq!(deployments[0]["protocol"], "http");
        assert_eq!(deployments[0]["endpoint_health"], "ok");
        assert_eq!(deployments[0]["reachable"], true);
        assert_eq!(deployments[1]["host_ip"], "10.78.0.1");
        assert_eq!(
            deployments[1]["endpoint"],
            "10.78.0.1:19002:multi-host-service"
        );
        assert_eq!(deployments[1]["version"], "1.2.3");
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
                    "service_id": "gateway",
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
        let mut console = console_with_env(&[("OJOS_SMOKE_MODE", Some("1"))]);
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
        let mut console = console_with_env(&[("OJOS_SMOKE_MODE", Some("1"))]);
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
        assert_eq!(orphan.status, 400);
        assert!(
            orphan.body["message"]
                .as_str()
                .unwrap_or("")
                .contains("invalid node topology")
        );
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
        assert_eq!(cycle.status, 400);
        assert!(
            cycle.body["message"]
                .as_str()
                .unwrap_or("")
                .contains("invalid manifest")
        );
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

        let listener = TcpListener::bind("127.0.0.1:0").expect("bind external storage health");
        listener
            .set_nonblocking(true)
            .expect("configure external storage health listener");
        let port = listener
            .local_addr()
            .expect("external storage health address")
            .port();
        let health_server = thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(5);
            loop {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        stream
                            .set_nonblocking(false)
                            .expect("make external health connection blocking");
                        stream
                            .set_read_timeout(Some(Duration::from_secs(2)))
                            .expect("bound external health read");
                        let mut buffer = [0_u8; 1024];
                        let _ = stream.read(&mut buffer).expect("read health request");
                        stream
                            .write_all(
                                b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok",
                            )
                            .expect("write health response");
                        return;
                    }
                    Err(error)
                        if error.kind() == ErrorKind::WouldBlock && Instant::now() < deadline =>
                    {
                        thread::sleep(Duration::from_millis(10));
                    }
                    Err(error) if error.kind() == ErrorKind::WouldBlock => {
                        panic!("external storage health probe did not arrive before timeout");
                    }
                    Err(error) => panic!("accept external storage health probe: {error}"),
                }
            }
        });
        let endpoint = format!("127.0.0.1:{port}:storage-service");
        let expected_upstream = format!("http://127.0.0.1:{port}");
        let install = post_json(
            &mut console,
            "/releases/storage-service/install",
            &format!(
                r#"{{
                "operation_id": "op-node-api-storage-install",
                "host_ip": "127.0.0.1",
                "endpoint": "{endpoint}",
                "gateway_node_id": "child-node",
                "execute_service_driver": false,
                "external_service_running": true
            }}"#
            ),
        );
        health_server
            .join()
            .expect("external storage health server");
        assert_eq!(install.status, 200);
        assert_eq!(
            install.body["action_result"]["status"], "SUCCEEDED",
            "install response: {}",
            install.body
        );

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
                        && route["provider_endpoint"] == endpoint
                        && route["upstream_base"] == expected_upstream
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
                "/releases/auth-service/0.1.0",
                r#"{
                    "operation_id": "op-daemon-release-route-delete"
                }"#,
            ),
        )
        .expect("release delete route");
        assert_eq!(delete.status, 204);
        assert_eq!(delete.body["action_result"]["action_id"], "release.delete");
        assert_eq!(delete.body["action_result"]["status"], "SUCCEEDED");
    }

    #[test]
    fn daemon_release_delete_action_updates_release_registry() {
        let mut console = console();
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
                    "confirm": "true",
                    "execute_service_driver": "true"
                }
            }"#,
        );
        assert_eq!(rollback.status, 200);
        assert_eq!(rollback.body["action_result"]["status"], "SUCCEEDED");
        assert_eq!(
            rollback.body["action_result"]["capability_status"],
            "RUNTIME_PIPELINE"
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

        let list = get(&mut console, "/diagnostics?limit=1");
        assert_eq!(list.status, 200);
        assert_eq!(list.body["items"][0]["report_id"], json!(report_id));
        assert!(list.body["next_cursor"].is_null());

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

        // Expected route errors must cross the same response boundary as a
        // real HTTP request. The lower-level `route_api_request` deliberately
        // returns `Err(StatusError)` so the outer handler can grade it.
        let missing = handle_api_request(
            &mut console,
            request("GET", "/diagnostics/missing.json", ""),
        );
        assert_eq!(missing.status, 404);
        assert_eq!(missing.body["status"], "error");
        assert!(
            missing.body["message"]
                .as_str()
                .is_some_and(|message| message.contains("diagnostic report missing not found"))
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
    fn actions_require_explicit_destructive_targets_and_confirmation() {
        let mut console = console();
        let cases = [
            (
                r#"{"action":"endpoint.delete"}"#,
                &["endpoint", "confirm"][..],
            ),
            (
                r#"{
                    "action":"service.delete",
                    "fields":{"service_id":"problem-service"}
                }"#,
                &["confirm"][..],
            ),
            (
                r#"{"action":"release.delete","fields":{"confirm":true}}"#,
                &["service_id"][..],
            ),
            (
                r#"{
                    "action":"release.rollback",
                    "fields":{"service_id":"gateway"}
                }"#,
                &["confirm"][..],
            ),
        ];

        for (body, missing_fields) in cases {
            let response = handle_api_request(&mut console, request("POST", "/actions", body));
            assert_eq!(response.status, 400, "body: {body}");
            let message = response.body["message"].as_str().unwrap_or("");
            for field in missing_fields {
                assert!(message.contains(field), "{message}");
            }
        }
    }

    #[test]
    fn convenience_routes_reject_missing_form_fields() {
        let mut console = console();
        for (path, expected_field) in [
            ("/releases", "service_id"),
            ("/endpoints", "endpoint"),
            ("/endpoints/health", "endpoint"),
            ("/links", "source_endpoint"),
            ("/operations/plan", "action"),
        ] {
            let response = handle_api_request(&mut console, request("POST", path, "{}"));
            assert_eq!(response.status, 400, "path: {path}");
            assert!(
                response.body["message"]
                    .as_str()
                    .unwrap_or("")
                    .contains(expected_field),
                "path: {path}, response: {}",
                response.body
            );
        }
    }

    #[test]
    fn convenience_endpoint_delete_uses_path_target_and_forced_confirmation() {
        let mut console = console();
        for (operation_id, endpoint) in [
            ("op-path-target", "127.0.0.1:19021:gateway"),
            ("op-body-decoy", "127.0.0.1:19022:gateway"),
        ] {
            let response = post_json(
                &mut console,
                "/endpoints",
                &format!(
                    r#"{{
                        "operation_id":"{operation_id}",
                        "endpoint":"{endpoint}",
                        "service_id":"gateway",
                        "protocol":"http"
                    }}"#
                ),
            );
            assert_eq!(response.status, 201);
        }

        let response = handle_api_request(
            &mut console,
            request(
                "DELETE",
                "/endpoints/127.0.0.1%3A19021%3Agateway",
                r#"{
                    "operation_id":"op-path-delete",
                    "endpoint":"127.0.0.1:19022:gateway",
                    "confirm":false
                }"#,
            ),
        );
        assert_eq!(response.status, 204);
        let endpoints = console.endpoints().expect("endpoints");
        assert!(
            !endpoints
                .iter()
                .any(|endpoint| endpoint.endpoint == "127.0.0.1:19021:gateway")
        );
        assert!(
            endpoints
                .iter()
                .any(|endpoint| endpoint.endpoint == "127.0.0.1:19022:gateway")
        );
    }

    #[test]
    fn actions_generate_operation_id_without_preview_defaults() {
        let mut console = console();
        let response = handle_api_request(
            &mut console,
            request(
                "POST",
                "/actions",
                r#"{
                    "action":"endpoint.create",
                    "fields":{
                        "endpoint":"127.0.0.1:19023:gateway",
                        "service_id":"gateway",
                        "protocol":"http"
                    }
                }"#,
            ),
        );
        assert_eq!(response.status, 200);
        let operation_id = response.body["action_result"]["operation_id"]
            .as_str()
            .expect("generated operation id");
        assert!(!operation_id.is_empty());
        assert!(!operation_id.starts_with("preview-"));
    }

    #[test]
    fn daemon_grades_error_status_codes() {
        let mut console = console();
        // 请求体不是合法 JSON：客户端错误，400 而不是 500。
        let malformed =
            handle_api_request(&mut console, request("POST", "/endpoints", "{not json"));
        assert_eq!(malformed.status, 400);
        // 请求体是合法 JSON 但不是对象：同样是 400。
        let not_object = handle_api_request(&mut console, request("POST", "/actions", "[]"));
        assert_eq!(not_object.status, 400);
        // 引用了不存在的对象：404。
        let missing = handle_api_request(
            &mut console,
            request("GET", "/operations/op-daemon-does-not-exist", ""),
        );
        assert_eq!(missing.status, 404);

        // core 明确报告的 manifest 校验错误属于客户端输入。
        let invalid_manifest = handle_api_request(
            &mut console,
            request(
                "POST",
                "/endpoints",
                r#"{
                    "operation_id": "op-daemon-invalid-endpoint",
                    "endpoint": "not-an-endpoint",
                    "service_id": "gateway",
                    "protocol": "http"
                }"#,
            ),
        );
        assert_eq!(invalid_manifest.status, 400);
    }

    #[test]
    fn status_for_error_defaults_to_internal_error() {
        // core / IO 故障不应被伪装成客户端错误。
        let err = anyhow::anyhow!("orchestrator store is unavailable");
        assert_eq!(status_for_error(&err), 500);
        let parse: anyhow::Error = serde_json::from_str::<Value>("{oops").unwrap_err().into();
        assert_eq!(status_for_error(&parse), 400);
        let unauthorized: anyhow::Error = StatusError::new(401, "nope").into();
        assert_eq!(status_for_error(&unauthorized), 401);

        for core_error in [
            OrchestratorError::InvalidManifest("bad manifest".to_string()),
            OrchestratorError::UnsafePath("../outside".to_string()),
        ] {
            assert_eq!(status_for_error(&core_error.into()), 400);
        }
        let core_json = serde_json::from_str::<Value>("{bad json").unwrap_err();
        let core_json: anyhow::Error = OrchestratorError::Json(core_json).into();
        assert_eq!(status_for_error(&core_json), 400);
        let blocked: anyhow::Error =
            OrchestratorError::Blocked("operation already finished".to_string()).into();
        assert_eq!(status_for_error(&blocked), 409);
    }
}
