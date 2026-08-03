//! HTTP 协议层：请求/响应模型、报文解析与写出、查询串工具。
//!
//! 这里只关心传输语义，不认识任何编排器业务路由。需要精确状态码的业务失败通过
//! [`StatusError`] 穿过 anyhow 错误链向上传递，由路由层统一翻译成响应码。

use anyhow::{Result, anyhow};
use serde_json::{Map, Value, json};
use std::collections::BTreeMap;
use std::fmt;
use std::io::{Read, Write};
use std::net::TcpStream;
use std::time::{Duration, Instant};

/// 读超时：慢速客户端不应长期占用工作线程。
const READ_TIMEOUT: Duration = Duration::from_secs(5);
/// 写超时：对端不读走响应时，避免工作线程永久阻塞在 write 上。
pub(crate) const WRITE_TIMEOUT: Duration = Duration::from_secs(10);
/// 单个请求（含 body）的字节上限。
const MAX_REQUEST_BYTES: usize = 1024 * 1024;
/// A complete 2,000 Endpoint / 8,000 Link TopologySpec is intentionally sent
/// as one immutable revision. Keep the larger limit scoped to those two
/// revision-writing routes instead of widening every mutation endpoint.
const MAX_TOPOLOGY_SPEC_REQUEST_BYTES: usize = 8 * 1024 * 1024;
pub(crate) const SECURITY_RESPONSE_HEADERS: &str = concat!(
    "Content-Security-Policy: frame-ancestors 'none'; object-src 'none'; base-uri 'none'\r\n",
    "X-Frame-Options: DENY\r\n",
    "X-Content-Type-Options: nosniff\r\n",
    "Referrer-Policy: no-referrer\r\n",
    "Permissions-Policy: camera=(), microphone=(), geolocation=()",
);

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ApiRequest {
    pub(crate) method: String,
    pub(crate) path: String,
    pub(crate) headers: BTreeMap<String, String>,
    pub(crate) body: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ApiResponse {
    pub(crate) status: u16,
    pub(crate) body: Value,
    pub(crate) content_type: String,
    pub(crate) headers: BTreeMap<String, String>,
}

impl ApiResponse {
    pub(crate) fn ok(body: Value) -> Self {
        Self::json(200, body)
    }

    pub(crate) fn created(body: Value) -> Self {
        Self::json(201, body)
    }

    pub(crate) fn accepted(body: Value) -> Self {
        Self::json(202, body)
    }

    pub(crate) fn no_content(body: Value) -> Self {
        Self::json(204, body)
    }

    pub(crate) fn event_stream(body: impl Into<String>) -> Self {
        Self {
            status: 200,
            body: Value::String(body.into()),
            content_type: "text/event-stream; charset=utf-8".to_string(),
            headers: BTreeMap::new(),
        }
    }

    pub(crate) fn text(
        status: u16,
        body: impl Into<String>,
        content_type: impl Into<String>,
    ) -> Self {
        Self {
            status,
            body: Value::String(body.into()),
            content_type: content_type.into(),
            headers: BTreeMap::new(),
        }
    }

    pub(crate) fn error(status: u16, message: impl Into<String>) -> Self {
        Self::json(
            status,
            json!({
                "status": "error",
                "message": message.into(),
            }),
        )
    }

    pub(crate) fn problem(
        status: u16,
        code: impl Into<String>,
        detail: impl Into<String>,
        request_id: impl Into<String>,
        operation_id: Option<&str>,
    ) -> Self {
        let mut body = json!({
            "type": "about:blank",
            "title": status_reason_phrase(status),
            "status": status,
            "code": code.into(),
            "detail": detail.into(),
            "request_id": request_id.into(),
        });
        if let Some(operation_id) = operation_id {
            body["operation_id"] = Value::String(operation_id.to_string());
        }
        let mut response = Self::json(status, body);
        response.content_type = "application/problem+json; charset=utf-8".to_string();
        response
    }

    pub(crate) fn with_header(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        let name = name.into();
        let value = value.into();
        if !name.contains(['\r', '\n']) && !value.contains(['\r', '\n']) {
            self.headers.insert(name, value);
        }
        self
    }

    fn json(status: u16, body: Value) -> Self {
        Self {
            status,
            body,
            content_type: "application/json; charset=utf-8".to_string(),
            headers: BTreeMap::new(),
        }
    }
}

/// 带 HTTP 状态码的错误。鉴权失败、请求体/参数校验失败、对象未找到都用它标注，
/// 使响应不再一律退化成 500。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct StatusError(pub(crate) u16, pub(crate) String);

impl StatusError {
    pub(crate) fn new(status: u16, message: impl Into<String>) -> Self {
        Self(status, message.into())
    }
}

impl fmt::Display for StatusError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.1)
    }
}

impl std::error::Error for StatusError {}

/// 所有变更请求都必须显式声明 JSON 内容类型，包括空 body。空表单 POST 也是浏览器
/// 可以跨站直接发出的“简单请求”，不能因为没有 body 就绕过 CSRF 门禁。
pub(crate) fn requires_json_content_type(request: &ApiRequest) -> bool {
    matches!(request.method.as_str(), "POST" | "PUT" | "PATCH" | "DELETE")
}

pub(crate) fn has_json_content_type(headers: &BTreeMap<String, String>) -> bool {
    headers
        .get("content-type")
        .map(|value| {
            value
                .trim()
                .to_ascii_lowercase()
                .starts_with("application/json")
        })
        .unwrap_or(false)
}

pub(crate) trait HttpStream: Read + Write {
    fn set_http_read_timeout(&mut self, timeout: Option<Duration>) -> std::io::Result<()>;
    fn set_http_write_timeout(&mut self, timeout: Option<Duration>) -> std::io::Result<()>;
}

impl HttpStream for TcpStream {
    fn set_http_read_timeout(&mut self, timeout: Option<Duration>) -> std::io::Result<()> {
        self.set_read_timeout(timeout)
    }

    fn set_http_write_timeout(&mut self, timeout: Option<Duration>) -> std::io::Result<()> {
        self.set_write_timeout(timeout)
    }
}

pub(crate) fn read_http_request(stream: &mut impl HttpStream) -> Result<ApiRequest> {
    read_http_request_with_timeout(stream, READ_TIMEOUT)
}

fn read_http_request_with_timeout(
    stream: &mut impl HttpStream,
    total_timeout: Duration,
) -> Result<ApiRequest> {
    stream.set_http_write_timeout(Some(WRITE_TIMEOUT))?;
    let deadline = Instant::now() + total_timeout;
    let mut bytes = Vec::new();
    let mut buffer = [0_u8; 4096];
    loop {
        let remaining = deadline
            .checked_duration_since(Instant::now())
            .filter(|remaining| !remaining.is_zero())
            .ok_or_else(|| anyhow!("request read timed out"))?;
        stream.set_http_read_timeout(Some(remaining))?;
        let read = stream.read(&mut buffer).map_err(|err| {
            if matches!(
                err.kind(),
                std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
            ) {
                anyhow!("request read timed out")
            } else {
                err.into()
            }
        })?;
        if read == 0 {
            break;
        }
        bytes.extend_from_slice(&buffer[..read]);
        if complete_http_request(&bytes)? {
            break;
        }
        if bytes.len() > MAX_TOPOLOGY_SPEC_REQUEST_BYTES {
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
    let request_line = headers
        .lines()
        .next()
        .ok_or_else(|| anyhow!("missing request line"))?;
    let content_length = content_length(headers)?;
    let expected_length =
        expected_request_length(header_end, content_length, request_byte_limit(request_line))?;
    Ok(bytes.len() >= expected_length)
}

fn parse_http_request_bytes(bytes: Vec<u8>) -> Result<ApiRequest> {
    let header_end = header_end(&bytes).ok_or_else(|| anyhow!("HTTP headers are incomplete"))?;
    let headers = std::str::from_utf8(&bytes[..header_end])?;
    let mut lines = headers.lines();
    let request_line = lines
        .next()
        .ok_or_else(|| anyhow!("missing request line"))?;
    let max_request_bytes = request_byte_limit(request_line);
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
    let expected_length = expected_request_length(header_end, content_length, max_request_bytes)?;
    let body_bytes = bytes
        .get(header_end + 4..expected_length)
        .ok_or_else(|| anyhow!("HTTP body is incomplete"))?;
    let body = String::from_utf8(body_bytes.to_vec())?;
    Ok(ApiRequest {
        method,
        path,
        headers,
        body,
    })
}

fn request_byte_limit(request_line: &str) -> usize {
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or_default();
    let path = parts
        .next()
        .unwrap_or_default()
        .split('?')
        .next()
        .unwrap_or_default();
    let is_initial_revision = method == "POST" && path == "/api/v1/topologies";
    let is_next_revision = method == "POST"
        && path.starts_with("/api/v1/topologies/")
        && path.ends_with("/revisions")
        && path
            .trim_start_matches("/api/v1/topologies/")
            .strip_suffix("/revisions")
            .is_some_and(|topology_id| {
                !topology_id.is_empty() && !topology_id.trim_end_matches('/').contains('/')
            });
    if is_initial_revision || is_next_revision {
        MAX_TOPOLOGY_SPEC_REQUEST_BYTES
    } else {
        MAX_REQUEST_BYTES
    }
}

fn expected_request_length(
    header_end: usize,
    content_length: usize,
    max_request_bytes: usize,
) -> Result<usize> {
    let body_start = header_end
        .checked_add(4)
        .ok_or_else(|| anyhow!("request length overflow"))?;
    let expected_length = body_start
        .checked_add(content_length)
        .ok_or_else(|| anyhow!("request length overflow"))?;
    if expected_length > max_request_bytes {
        return Err(anyhow!("request body is too large"));
    }
    Ok(expected_length)
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

pub(crate) fn write_http_response(
    stream: &mut impl Write,
    mut response: ApiResponse,
) -> Result<()> {
    if matches!(response.status, 429 | 503) && !response.headers.contains_key("Retry-After") {
        response
            .headers
            .insert("Retry-After".to_string(), "1".to_string());
    }
    crate::observability::record_response(&response);
    let body = if !response.content_type.starts_with("application/json")
        && !response
            .content_type
            .starts_with("application/problem+json")
    {
        response
            .body
            .as_str()
            .ok_or_else(|| anyhow!("non-JSON response body must be a string"))?
            .to_string()
    } else {
        response_json(response.body)?
    };
    let status_text = status_reason_phrase(response.status);
    let extra_headers = response
        .headers
        .iter()
        .map(|(name, value)| format!("{name}: {value}\r\n"))
        .collect::<String>();
    write!(
        stream,
        "HTTP/1.1 {} {}\r\nContent-Type: {}\r\nContent-Length: {}\r\n{}\r\n{}Connection: close\r\n\r\n{}",
        response.status,
        status_text,
        response.content_type,
        body.len(),
        SECURITY_RESPONSE_HEADERS,
        extra_headers,
        body
    )?;
    Ok(())
}

fn status_reason_phrase(status: u16) -> &'static str {
    match status {
        200 => "OK",
        201 => "Created",
        202 => "Accepted",
        204 => "No Content",
        302 => "Found",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        409 => "Conflict",
        410 => "Gone",
        415 => "Unsupported Media Type",
        422 => "Unprocessable Content",
        428 => "Precondition Required",
        429 => "Too Many Requests",
        500 => "Internal Server Error",
        501 => "Not Implemented",
        502 => "Bad Gateway",
        503 => "Service Unavailable",
        _ => "Internal Server Error",
    }
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

pub(crate) fn path_segments(path: &str) -> Result<Vec<String>> {
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

pub(crate) fn query_bool(query: &str, name: &str) -> Result<bool> {
    Ok(query_value(query, name)?
        .is_some_and(|value| matches!(value.trim().to_ascii_lowercase().as_str(), "1" | "true")))
}

pub(crate) fn query_value(query: &str, name: &str) -> Result<Option<String>> {
    for pair in query.split('&') {
        let Some((key, value)) = pair.split_once('=') else {
            continue;
        };
        if decode_query_component(key)? != name {
            continue;
        }
        let value = decode_query_component(value)?;
        return Ok((!value.trim().is_empty()).then_some(value));
    }
    Ok(None)
}

fn decode_query_component(component: &str) -> Result<String> {
    let bytes = component.as_bytes();
    let mut output = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'%' => {
                let hex = bytes
                    .get(index + 1..index + 3)
                    .ok_or_else(|| anyhow!("invalid percent-encoded query parameter"))?;
                let text = std::str::from_utf8(hex)?;
                let value = u8::from_str_radix(text, 16)
                    .map_err(|_| anyhow!("invalid percent-encoded query parameter"))?;
                output.push(value);
                index += 3;
            }
            b'+' => {
                output.push(b' ');
                index += 1;
            }
            byte => {
                output.push(byte);
                index += 1;
            }
        }
    }
    String::from_utf8(output).map_err(Into::into)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request_with(method: &str, body: &str, content_type: Option<&str>) -> ApiRequest {
        let mut headers = BTreeMap::new();
        if let Some(content_type) = content_type {
            headers.insert("content-type".to_string(), content_type.to_string());
        }
        ApiRequest {
            method: method.to_string(),
            path: "/endpoints".to_string(),
            headers,
            body: body.to_string(),
        }
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
    fn oversized_content_length_is_rejected_without_overflow() {
        let request = format!(
            "POST /endpoints HTTP/1.1\r\nContent-Length: {}\r\n\r\n",
            usize::MAX
        );
        let error = complete_http_request(request.as_bytes())
            .expect_err("oversized declared body must be rejected");
        assert!(
            error.to_string().contains("too large")
                || error.to_string().contains("length overflow")
        );
    }

    #[test]
    fn large_request_limit_is_scoped_to_immutable_topology_revision_writes() {
        let two_mib = 2 * 1024 * 1024;
        for target in [
            "/api/v1/topologies",
            "/api/v1/topologies/capacity-primary/revisions",
            "/api/v1/topologies?source=capacity",
        ] {
            let request = format!("POST {target} HTTP/1.1\r\nContent-Length: {two_mib}\r\n\r\n");
            assert!(
                !complete_http_request(request.as_bytes())
                    .expect("a production-sized topology body is allowed"),
                "headers alone cannot complete {target}"
            );
        }

        for target in [
            "/api/v1/store/releases:install",
            "/api/v1/topologies/capacity-primary:apply",
            "/api/v1/topologies/capacity-primary/revisions/extra",
        ] {
            let request = format!("POST {target} HTTP/1.1\r\nContent-Length: {two_mib}\r\n\r\n");
            assert!(
                complete_http_request(request.as_bytes())
                    .expect_err("non-Spec routes retain the default request limit")
                    .to_string()
                    .contains("too large")
            );
        }

        let nine_mib = 9 * 1024 * 1024;
        let request =
            format!("POST /api/v1/topologies HTTP/1.1\r\nContent-Length: {nine_mib}\r\n\r\n");
        assert!(
            complete_http_request(request.as_bytes())
                .expect_err("TopologySpec writes still have an absolute upper bound")
                .to_string()
                .contains("too large")
        );
    }

    #[test]
    fn query_parameters_are_form_url_decoded() {
        assert_eq!(
            query_value(
                "repo=owner%2Frepository&label=release+candidate%26beta",
                "repo"
            )
            .expect("encoded slash should decode"),
            Some("owner/repository".to_string())
        );
        assert_eq!(
            query_value(
                "repo=owner%2Frepository&label=release+candidate%26beta",
                "label"
            )
            .expect("plus and ampersand should decode"),
            Some("release candidate&beta".to_string())
        );
        assert!(query_bool("include_upstream=tr%75e", "include_upstream").unwrap());
        assert!(query_value("repo=owner%2", "repo").is_err());
    }

    #[test]
    fn json_content_type_is_required_for_every_mutation() {
        let json = request_with("POST", "{}", Some("application/json; charset=utf-8"));
        assert!(requires_json_content_type(&json));
        assert!(has_json_content_type(&json.headers));

        let form = request_with("POST", "{}", Some("application/x-www-form-urlencoded"));
        assert!(requires_json_content_type(&form));
        assert!(!has_json_content_type(&form.headers));

        let missing = request_with("PATCH", "{}", None);
        assert!(requires_json_content_type(&missing));
        assert!(!has_json_content_type(&missing.headers));

        // 读请求不受门禁约束；空 body 的变更仍必须使用 JSON，避免简单表单 CSRF。
        assert!(!requires_json_content_type(&request_with("GET", "", None)));
        assert!(requires_json_content_type(&request_with("POST", "", None)));
        assert!(requires_json_content_type(&request_with(
            "DELETE", "", None
        )));
    }

    #[test]
    fn request_read_timeout_is_a_total_deadline() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let reader = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let started = Instant::now();
            let result = read_http_request_with_timeout(&mut stream, Duration::from_millis(60));
            (started.elapsed(), result)
        });

        let mut client = TcpStream::connect(address).unwrap();
        for _ in 0..10 {
            if client.write_all(b"G").is_err() {
                break;
            }
            std::thread::sleep(Duration::from_millis(15));
        }

        let (elapsed, result) = reader.join().unwrap();
        let error = result.expect_err("a trickle client must hit the total read deadline");
        assert!(error.to_string().contains("timed out"));
        assert!(elapsed < Duration::from_millis(300));
    }

    #[test]
    fn response_security_headers_block_framing_and_content_sniffing() {
        for header in [
            "frame-ancestors 'none'",
            "X-Frame-Options: DENY",
            "X-Content-Type-Options: nosniff",
            "Referrer-Policy: no-referrer",
        ] {
            assert!(
                SECURITY_RESPONSE_HEADERS.contains(header),
                "missing response security header {header}"
            );
        }
    }

    #[test]
    fn event_stream_body_is_written_verbatim_instead_of_json_quoted() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let writer = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            write_http_response(
                &mut stream,
                ApiResponse::event_stream("id: 1\nevent: job\ndata: {}\n\n"),
            )
            .unwrap();
        });
        let mut client = TcpStream::connect(address).unwrap();
        let mut response = String::new();
        client.read_to_string(&mut response).unwrap();
        writer.join().unwrap();
        assert!(response.contains("Content-Type: text/event-stream"));
        assert!(response.ends_with("id: 1\nevent: job\ndata: {}\n\n"));
        assert!(!response.ends_with("\"id: 1\\nevent: job\\ndata: {}\\n\\n\""));
    }

    #[test]
    fn status_error_carries_status_and_message() {
        let err: anyhow::Error = StatusError::new(404, "operation op-1 not found").into();
        assert_eq!(err.to_string(), "operation op-1 not found");
        assert_eq!(
            err.downcast_ref::<StatusError>().map(|status| status.0),
            Some(404)
        );
    }

    #[test]
    fn conflict_status_has_the_standard_reason_phrase() {
        assert_eq!(status_reason_phrase(409), "Conflict");
    }
}
