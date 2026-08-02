//! 连接层：TCP 监听、固定大小工作线程池、请求分发与静态资源写出。

use crate::auth::{
    ORCHESTRATOR_INTERNAL_TOKEN_HEADER, configured_internal_token, internal_token_check,
};
use crate::http::{
    ApiRequest, ApiResponse, SECURITY_RESPONSE_HEADERS, WRITE_TIMEOUT, has_json_content_type,
    query_bool, read_http_request, requires_json_content_type, write_http_response,
};
use crate::routes::{handle_api_request, status_for_error};
use crate::{market_api, static_site, ui_layout};
use anyhow::{Context, Result, anyhow};
use orchestrator_core::OrchestratorActionConsole;
use std::io::Write;
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, TrySendError};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};

/// 默认工作线程数；可用 `ORCHESTRATOR_MAX_WORKERS` 覆盖。
const DEFAULT_MAX_WORKERS: usize = 32;
/// 待处理连接的有界队列容量。队列满说明已经过载，直接对新连接回 503，
/// 而不是无上限地 spawn 线程把整台机器拖垮。
const CONNECTION_QUEUE_CAPACITY: usize = 64;

struct ServerContext {
    console: Mutex<OrchestratorActionConsole>,
    store_state: market_api::StoreState,
    repo_root: PathBuf,
    web_root: PathBuf,
    persistent_store: bool,
    startup_warnings: Vec<String>,
}

fn max_workers() -> usize {
    std::env::var("ORCHESTRATOR_MAX_WORKERS")
        .ok()
        .and_then(|value| value.trim().parse::<usize>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(DEFAULT_MAX_WORKERS)
}

pub(crate) fn serve(
    bind: String,
    console: OrchestratorActionConsole,
    repo_root: PathBuf,
    web_root: PathBuf,
) -> Result<()> {
    let listener = TcpListener::bind(&bind).with_context(|| format!("bind {bind}"))?;
    eprintln!("OJOS Orchestrator daemon listening on {bind}");
    if web_root.join("index.html").is_file() {
        eprintln!(
            "OJOS Orchestrator web UI: http://{bind}/ (root {})",
            web_root.display()
        );
    } else {
        eprintln!(
            "web UI assets not found at {}; serving API and a placeholder page only",
            web_root.display()
        );
    }
    let persistent_store = console.uses_persistent_store();
    let startup_warnings = console.warnings().to_vec();
    let context = Arc::new(ServerContext {
        console: Mutex::new(console),
        store_state: market_api::StoreState::new(),
        repo_root,
        web_root,
        persistent_store,
        startup_warnings,
    });

    let worker_count = max_workers();
    eprintln!(
        "orchestrator daemon worker pool: {worker_count} threads, queue {CONNECTION_QUEUE_CAPACITY}"
    );
    let (sender, receiver) = mpsc::sync_channel::<TcpStream>(CONNECTION_QUEUE_CAPACITY);
    let receiver = Arc::new(Mutex::new(receiver));
    let mut workers: Vec<JoinHandle<()>> = Vec::with_capacity(worker_count);
    for _ in 0..worker_count {
        let receiver = Arc::clone(&receiver);
        let context = Arc::clone(&context);
        workers.push(thread::spawn(move || worker_loop(&context, &receiver)));
    }

    for stream in listener.incoming() {
        let stream = match stream {
            Ok(stream) => stream,
            Err(err) => {
                eprintln!("orchestrator daemon accept error: {err}");
                continue;
            }
        };
        match sender.try_send(stream) {
            Ok(()) => {}
            Err(TrySendError::Full(mut stream)) => reject_overloaded(&mut stream),
            Err(TrySendError::Disconnected(_)) => {
                eprintln!("orchestrator daemon worker pool stopped; leaving the accept loop");
                break;
            }
        }
    }

    drop(sender);
    for worker in workers {
        let _ = worker.join();
    }
    Ok(())
}

/// 工作线程主循环：从共享队列取连接，队列关闭后自然退出。
fn worker_loop(context: &ServerContext, receiver: &Mutex<Receiver<TcpStream>>) {
    loop {
        let received = {
            let guard = match receiver.lock() {
                Ok(guard) => guard,
                Err(poisoned) => poisoned.into_inner(),
            };
            guard.recv()
        };
        let Ok(mut stream) = received else {
            return;
        };
        if let Err(err) = handle_connection(context, &mut stream) {
            eprintln!("orchestrator daemon connection error: {err}");
        }
    }
}

/// 队列已满：立刻回 503 并关闭连接，把背压传给调用方。
fn reject_overloaded(stream: &mut TcpStream) {
    let _ = stream.set_write_timeout(Some(WRITE_TIMEOUT));
    let response = ApiResponse::error(
        503,
        "orchestrator daemon connection queue is full; retry shortly",
    );
    if let Err(err) = write_http_response(stream, response) {
        eprintln!("orchestrator daemon overload response error: {err}");
    }
}

fn handle_connection(context: &ServerContext, stream: &mut TcpStream) -> Result<()> {
    match read_http_request(stream) {
        Ok(request) => dispatch_request(context, stream, request),
        Err(err) => write_http_response(stream, ApiResponse::error(400, err.to_string())),
    }
}

/// 已注册的 API 首段：这些路径交给 JSON API 路由，其余交给静态层。
fn is_api_path(path: &str) -> bool {
    let first = path.trim_start_matches('/').split('/').next().unwrap_or("");
    matches!(
        first,
        "health"
            | "services"
            | "deployments"
            | "nodes"
            | "releases"
            | "release-registry"
            | "templates"
            | "sets"
            | "endpoints"
            | "links"
            | "operations"
            | "topology"
            | "diagnostics"
            | "actions"
            | "internal"
            | "api"
    )
}

fn dispatch_request(
    context: &ServerContext,
    stream: &mut TcpStream,
    request: ApiRequest,
) -> Result<()> {
    // 所有变更请求（包括空 body）都必须声明 JSON 内容类型。空表单 POST 同样能被
    // 任意站点跨域直接发出，不能让它绕过最基本的 CSRF 门禁。
    if requires_json_content_type(&request) && !has_json_content_type(&request.headers) {
        return write_http_response(
            stream,
            ApiResponse::error(
                415,
                "mutating requests must send Content-Type: application/json",
            ),
        );
    }

    let path = request.path.split('?').next().unwrap_or("/").to_string();
    let query = request
        .path
        .split_once('?')
        .map(|(_, query)| query)
        .unwrap_or("")
        .to_string();

    // 健康探针不能等待 console 全局锁。安装下载、driver 和慢 store 操作可能长时间
    // 持锁；若 /health 也排队，容器运行时会把一个仍在工作的 daemon 误判为失效。
    if request.method == "GET" && path == "/health" {
        return write_http_response(
            stream,
            ApiResponse::ok(serde_json::json!({
                "service": "ojos-orchestrator-daemon",
                "store": if context.persistent_store { "persistent" } else { "memory" },
                "orchestrator_database_url": std::env::var("ORCHESTRATOR_DATABASE_URL").is_ok(),
                "warnings": &context.startup_warnings,
            })),
        );
    }

    // 画布布局持久化（无需 console；与控制面一样受内部令牌门禁约束）。
    if path == "/ui/layout" {
        if let Err(err) = internal_token_check(
            request.method.as_str(),
            &["ui", "layout"],
            request
                .headers
                .get(ORCHESTRATOR_INTERNAL_TOKEN_HEADER)
                .map(String::as_str),
            configured_internal_token().as_deref(),
        ) {
            return write_http_response(stream, ApiResponse::error(401, err.to_string()));
        }
        let result = match request.method.as_str() {
            "GET" => ui_layout::get_layout(&context.repo_root),
            "PUT" | "POST" => ui_layout::put_layout(&context.repo_root, &request.body),
            _ => Err(anyhow!(
                "unsupported method {} for /ui/layout",
                request.method
            )),
        };
        let response = match result {
            Ok(body) => ApiResponse::ok(body),
            Err(err) => ApiResponse::error(400, err.to_string()),
        };
        return write_http_response(stream, response);
    }

    // 插件商店 API（变更端点与既有控制面一样受内部令牌门禁约束）。
    if path == "/store" || path.starts_with("/store/") {
        if let Err(err) = internal_token_check(
            request.method.as_str(),
            &["store"],
            request
                .headers
                .get(ORCHESTRATOR_INTERNAL_TOKEN_HEADER)
                .map(String::as_str),
            configured_internal_token().as_deref(),
        ) {
            return write_http_response(stream, ApiResponse::error(401, err.to_string()));
        }
        // 网络请求不占用 console 全局锁：GitHub Release 查询与索引拉取先在锁外完成。
        if request.method == "GET" && path == "/store/github/releases" {
            let response = market_api::github_releases_response(&query)
                .unwrap_or_else(|err| ApiResponse::error(status_for_error(&err), err.to_string()));
            return write_http_response(stream, response);
        }
        if request.method == "GET" && path == "/store/index" {
            let refresh = query_bool(&query, "refresh")?;
            let payload =
                market_api::store_index_payload(&context.store_state, &context.repo_root, refresh);
            let response = match payload {
                Ok((index_url, cached, index)) => {
                    let installed = match context.console.lock() {
                        Ok(console) => market_api::installed_services(&console),
                        Err(_) => Err(anyhow!("orchestrator console lock poisoned")),
                    };
                    match installed {
                        Ok(installed) => ApiResponse::ok(serde_json::json!({
                            "index_url": index_url,
                            "cached": cached,
                            "index": index,
                            "installed": installed,
                        })),
                        Err(err) => ApiResponse::error(500, err.to_string()),
                    }
                }
                Err(err) => ApiResponse::error(status_for_error(&err), err.to_string()),
            };
            return write_http_response(stream, response);
        }
        let mut console = match context.console.lock() {
            Ok(console) => console,
            Err(_) => {
                return write_http_response(
                    stream,
                    ApiResponse::error(500, "orchestrator console lock poisoned"),
                );
            }
        };
        let response = match market_api::route_store_request(
            &context.store_state,
            &mut console,
            &context.repo_root,
            &request,
            &path,
            &query,
        ) {
            Some(Ok(response)) => response,
            Some(Err(err)) => ApiResponse::error(status_for_error(&err), err.to_string()),
            None => ApiResponse::error(
                404,
                format!("unsupported store route {} {}", request.method, path),
            ),
        };
        return write_http_response(stream, response);
    }

    // 既有控制面 API。
    if is_api_path(&path) {
        let mut console = match context.console.lock() {
            Ok(console) => console,
            Err(_) => {
                return write_http_response(
                    stream,
                    ApiResponse::error(500, "orchestrator console lock poisoned"),
                );
            }
        };
        let response = handle_api_request(&mut console, request);
        return write_http_response(stream, response);
    }

    // 静态文件 / SPA。静态层不做令牌检查（否则浏览器打不开页面），
    // 它只读 web_root 之内的文件，且拒绝 symlink 逃逸。
    if let Some(static_response) =
        static_site::try_serve(&context.web_root, request.method.as_str(), &path)
    {
        return write_static_response(stream, static_response);
    }
    if request.method == "GET" && path == "/" {
        return write_static_response(stream, static_site::placeholder_page());
    }

    // 其余交回既有路由（顶级诊断导出等），未知路径由其返回 404。
    let mut console = match context.console.lock() {
        Ok(console) => console,
        Err(_) => {
            return write_http_response(
                stream,
                ApiResponse::error(500, "orchestrator console lock poisoned"),
            );
        }
    };
    let response = handle_api_request(&mut console, request);
    write_http_response(stream, response)
}

fn write_static_response(
    stream: &mut TcpStream,
    response: static_site::StaticResponse,
) -> Result<()> {
    let status_text = match response.status {
        200 => "OK",
        404 => "Not Found",
        _ => "OK",
    };
    write!(
        stream,
        "HTTP/1.1 {} {}\r\nContent-Type: {}\r\nContent-Length: {}\r\nCache-Control: {}\r\n{}\r\nConnection: close\r\n\r\n",
        response.status,
        status_text,
        response.content_type,
        response.body.len(),
        response.cache_control,
        SECURITY_RESPONSE_HEADERS
    )?;
    stream.write_all(&response.body)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_env::TestEnv;

    #[test]
    fn api_paths_are_separated_from_static_assets() {
        assert!(is_api_path("/health"));
        assert!(is_api_path("/deployments"));
        assert!(is_api_path("/nodes/child-node/routes"));
        assert!(is_api_path("/api/node/services/install"));
        // 根路径与前端构建产物交给静态层，因此不经过令牌门禁。
        assert!(!is_api_path("/"));
        assert!(!is_api_path("/assets/index-abc123.js"));
        assert!(!is_api_path("/favicon.ico"));
    }

    #[test]
    fn max_workers_defaults_and_rejects_invalid_values() {
        let mut env = TestEnv::lock();
        env.set("ORCHESTRATOR_MAX_WORKERS", "8");
        let configured = max_workers();
        env.set("ORCHESTRATOR_MAX_WORKERS", "0");
        let zero = max_workers();
        env.set("ORCHESTRATOR_MAX_WORKERS", "not-a-number");
        let invalid = max_workers();
        assert_eq!(configured, 8);
        assert_eq!(zero, DEFAULT_MAX_WORKERS);
        assert_eq!(invalid, DEFAULT_MAX_WORKERS);
    }
}
