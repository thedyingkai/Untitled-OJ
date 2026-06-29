use anyhow::{Context, Result, anyhow};
use clap::Parser;
use orchestrator_core::{
    ActionRequest, OrchestratorActionConsole, default_console_request, validate_endpoint_id,
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
        let mut stream = stream?;
        let response = match read_http_request(&mut stream) {
            Ok(request) => handle_api_request(&mut console, request),
            Err(err) => ApiResponse::error(400, err.to_string()),
        };
        write_http_response(&mut stream, response)?;
    }
    Ok(())
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
    let segments = path_segments(path)?;
    let segment_refs = segments.iter().map(String::as_str).collect::<Vec<_>>();
    match (request.method.as_str(), segment_refs.as_slice()) {
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
        ("GET", ["sets"]) => Ok(ApiResponse::ok(json!({
            "sets": console.view()?.sets,
        }))),
        ("GET", ["endpoints"]) => Ok(ApiResponse::ok(json!({
            "endpoints": console.view()?.endpoints,
        }))),
        ("POST", ["endpoints"]) => {
            let action = action_from_body("endpoint.register", &request.body, [])?;
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
        ("POST", ["sets", set_id, "expand"]) => {
            let action = action_from_body("set.expand", &request.body, [("set_id", *set_id)])?;
            Ok(ApiResponse::ok(json!({
                "action_result": console.dispatch(action)?,
            })))
        }
        ("POST", ["sets", set_id, "apply"]) => {
            let mut action = action_from_body("set.apply", &request.body, [("set_id", *set_id)])?;
            action.fields.remove("confirm");
            Ok(ApiResponse::created(json!({
                "action_result": console.dispatch(action)?,
            })))
        }
        ("GET", ["operations"]) => Ok(ApiResponse::ok(json!({
            "operations": console.view()?.operations,
        }))),
        ("POST", ["operations", "plan"]) => {
            let action = action_from_body("operation.plan", &request.body, [])?;
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
            let action = action_from_body("diagnostics.run", &request.body, [])?;
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
    let content_length = content_length(headers)?;
    let body_bytes = bytes
        .get(header_end + 4..header_end + 4 + content_length)
        .ok_or_else(|| anyhow!("HTTP body is incomplete"))?;
    let body = String::from_utf8(body_bytes.to_vec())?;
    Ok(ApiRequest { method, path, body })
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
            anyhow::bail!("无法将 Windows 控制台输入/输出编码设置为 UTF-8");
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

    fn repo_root() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(|path| path.parent())
            .expect("repo root")
            .to_path_buf()
    }

    fn console() -> OrchestratorActionConsole {
        OrchestratorActionConsole::load(repo_root()).expect("daemon console")
    }

    fn request(method: &str, path: &str, body: &str) -> ApiRequest {
        ApiRequest {
            method: method.to_string(),
            path: path.to_string(),
            body: body.to_string(),
        }
    }

    fn post_json(console: &mut OrchestratorActionConsole, path: &str, body: &str) -> ApiResponse {
        route_api_request(console, request("POST", path, body)).expect("POST response")
    }

    fn get(console: &mut OrchestratorActionConsole, path: &str) -> ApiResponse {
        route_api_request(console, request("GET", path, "")).expect("GET response")
    }

    fn seed_gateway_auth_link(console: &mut OrchestratorActionConsole) {
        let gateway = post_json(
            console,
            "/endpoints",
            r#"{
                "operation_id": "op-daemon-gateway-endpoint",
                "endpoint": "127.0.0.1:19180",
                "service_id": "gateway",
                "protocol": "http",
                "health_path": "/health"
            }"#,
        );
        assert_eq!(gateway.status, 201);

        let auth = post_json(
            console,
            "/endpoints",
            r#"{
                "operation_id": "op-daemon-auth-endpoint",
                "endpoint": "127.0.0.1:19181",
                "service_id": "auth",
                "protocol": "http",
                "health_path": "/health"
            }"#,
        );
        assert_eq!(auth.status, 201);

        let link = post_json(
            console,
            "/links",
            r#"{
                "operation_id": "op-daemon-gateway-auth-link",
                "source_endpoint": "127.0.0.1:19180",
                "target_endpoint": "127.0.0.1:19181",
                "protocol": "http",
                "auth_mode": "internal",
                "scope": "gateway-to-auth"
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
                    "endpoint": "127.0.0.1:19011",
                    "service_id": "gateway",
                    "protocol": "http",
                    "health_path": "/health"
                }"#,
            ),
        )
        .expect("endpoint register response");
        assert_eq!(response.status, 201);
        assert_eq!(
            response.body["action_result"]["action_id"],
            "endpoint.register"
        );
        assert_ne!(response.body["action_result"]["status"], "UNSUPPORTED");

        let response = route_api_request(&mut console, request("GET", "/endpoints", ""))
            .expect("endpoints response");
        assert!(
            response.body["endpoints"]
                .as_array()
                .expect("endpoint rows")
                .iter()
                .any(|endpoint| endpoint["endpoint"] == "127.0.0.1:19011")
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
                    "endpoint": "127.0.0.1:19012",
                    "service_id": "gateway",
                    "protocol": "http"
                }"#,
            ),
        )
        .expect("endpoint register response");
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
            "diagnostics.run"
        );
    }

    #[test]
    fn daemon_topology_reflects_endpoint_link_mutations() {
        let mut console = console();
        seed_gateway_auth_link(&mut console);

        let response = get(&mut console, "/topology");
        assert_eq!(response.status, 200);
        let endpoints = response.body["topology"]["endpoints"]
            .as_array()
            .expect("topology endpoints");
        assert!(
            endpoints
                .iter()
                .any(|endpoint| endpoint["endpoint"] == "127.0.0.1:19180")
        );
        assert!(
            endpoints
                .iter()
                .any(|endpoint| endpoint["endpoint"] == "127.0.0.1:19181")
        );
        let links = response.body["topology"]["links"]
            .as_array()
            .expect("topology links");
        assert!(links.iter().any(|link| {
            link["source_endpoint"] == "127.0.0.1:19180"
                && link["target_endpoint"] == "127.0.0.1:19181"
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
                "action": "endpoint.register",
                "operation_id": "op-daemon-actions-endpoint",
                "fields": {
                    "endpoint": "127.0.0.1:19182",
                    "service_id": "gateway",
                    "protocol": "http"
                }
            }"#,
        );
        assert_eq!(
            response.body["action_result"]["action_id"],
            "endpoint.register"
        );

        let after = get(&mut console, "/topology");
        let endpoints = after.body["topology"]["endpoints"]
            .as_array()
            .expect("topology endpoints");
        assert!(endpoints.len() >= before_count);
        assert!(
            endpoints
                .iter()
                .any(|endpoint| endpoint["endpoint"] == "127.0.0.1:19182")
        );
    }

    #[test]
    fn daemon_endpoint_health_route_dispatches_action() {
        let mut console = console();
        seed_gateway_auth_link(&mut console);

        let response = post_json(&mut console, "/endpoints/127.0.0.1%3A19180/health", "{}");
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
        seed_gateway_auth_link(&mut console);

        let response = post_json(
            &mut console,
            "/links/127.0.0.1%3A19180/127.0.0.1%3A19181/health",
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
    fn daemon_set_expand_route_dispatches_action() {
        let mut console = console();
        let response = post_json(&mut console, "/sets/single-node-oj/expand", "{}");
        assert_eq!(response.status, 200);
        assert_eq!(response.body["action_result"]["action_id"], "set.expand");
        assert_eq!(
            response.body["action_result"]["capability_status"],
            "READONLY"
        );
        assert_eq!(
            response.body["action_result"]["result"]["set_id"],
            "single-node-oj"
        );
    }

    #[test]
    fn daemon_set_apply_route_creates_operation() {
        let mut console = console();
        let response = post_json(
            &mut console,
            "/sets/single-node-oj/apply",
            r#"{"operation_id": "op-daemon-set-apply"}"#,
        );
        assert_eq!(response.status, 201);
        assert_eq!(response.body["action_result"]["action_id"], "set.apply");
        assert_eq!(
            response.body["action_result"]["capability_status"],
            "STORE_BACKED"
        );
        assert_eq!(response.body["action_result"]["status"], "PLANNED");

        let operation = get(&mut console, "/operations/op-daemon-set-apply");
        assert_eq!(operation.status, 200);
        assert_eq!(
            operation.body["operation"]["operation_id"],
            "op-daemon-set-apply"
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
                "endpoint": "127.0.0.1:19183",
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
            "endpoint.register"
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
