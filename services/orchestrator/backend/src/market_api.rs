//! 插件商店 API：索引拉取、GitHub Release 解析、外部模块导入与安装。

use crate::routes::{empty_http_action_request, validate_required_action_fields};
use crate::{ApiRequest, ApiResponse, StatusError};
use anyhow::{Result, anyhow};
use orchestrator_core::{HostService, OrchestratorActionConsole};
use serde_json::{Value, json};
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

const INDEX_CACHE_TTL: Duration = Duration::from_secs(60);
const MAX_FETCH_BYTES: usize = 1024 * 1024;
const USER_AGENT: &str = "ojos-orchestrator-daemon";
const DEFAULT_INDEX_PATH: &str = "store/index.json";
/// 手动跟随重定向的跳数上限（ureq 侧配置为 max_redirects(0)，见 http_get_json）。
const MAX_REDIRECTS: u32 = 5;

pub struct StoreState {
    index_cache: Mutex<Option<(Instant, String, Value)>>,
}

impl StoreState {
    pub fn new() -> Self {
        Self {
            index_cache: Mutex::new(None),
        }
    }
}

fn configured_index_url() -> String {
    std::env::var("OJOS_STORE_INDEX_URL")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| DEFAULT_INDEX_PATH.to_string())
}

fn github_token() -> Option<String> {
    for name in ["OJOS_GITHUB_TOKEN", "GITHUB_TOKEN"] {
        if let Ok(value) = std::env::var(name) {
            let value = value.trim().to_string();
            if !value.is_empty() {
                return Some(value);
            }
        }
    }
    None
}

fn env_flag(name: &str) -> bool {
    std::env::var(name)
        .map(|value| {
            matches!(
                value.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
        })
        .unwrap_or(false)
}

fn package_load_enabled() -> bool {
    env_flag("ORCHESTRATOR_RELEASE_PACKAGE_LOAD")
}

/// 生产开关：开启后 import/install 必须显式带上 release 包的校验和。
///
/// 默认**关闭**——core 的 `import_external_release` 允许 `expected_checksum` 为 None，
/// 现有 smoke / dev 流程也都不带 checksum，默认打开会直接把它们打断。
/// 生产部署文档要求把这个变量设为 1：否则任何能改写包源（或劫持其传输）的人
/// 都能替换掉被安装的产物。
fn require_release_checksum() -> bool {
    env_flag("ORCHESTRATOR_REQUIRE_RELEASE_CHECKSUM")
}

/// 与 core 的 `ORCHESTRATOR_ALLOW_PRIVATE_RELEASE_SOURCE` 同名开关，仅用于在
/// /store/status 里回显当前出网策略；真正的判定在 core 的 validate_outbound_url。
fn allow_private_release_source() -> bool {
    env_flag("ORCHESTRATOR_ALLOW_PRIVATE_RELEASE_SOURCE")
}

fn missing_checksum_response() -> ApiResponse {
    ApiResponse::error(
        400,
        "checksum is required while ORCHESTRATOR_REQUIRE_RELEASE_CHECKSUM is enabled; provide the sha256 of the release package (\"sha256:<hex>\") so the daemon can verify what it installs",
    )
}

fn http_get_json(url: &str, github: bool) -> Result<Value> {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(15)))
        .http_status_as_error(false)
        // 重定向自己跟：每一跳的目标都要重新过一次 SSRF 校验，交给 ureq 自动跟随
        // 就没有拦截点了。max_redirects(0) 时 ureq 原样返回 3xx，不会报 TooManyRedirects。
        .max_redirects(0)
        .build()
        .into();
    let mut current = url.trim().to_string();
    let mut hops: u32 = 0;
    let (status, body) = loop {
        // 索引 URL 来自 OJOS_STORE_INDEX_URL，GitHub URL 里嵌着调用方给的 repo，
        // 两者都不能直接拿去发请求：先挡掉 loopback / 内网 / 云元数据地址。
        orchestrator_core::validate_outbound_url(&current)
            .map_err(|err| anyhow!("fetch {url} blocked: {err}"))?;
        let mut request = agent.get(&current).header("User-Agent", USER_AGENT);
        if github {
            request = request.header("Accept", "application/vnd.github+json");
            // 令牌只在第一跳带：重定向目标未必还是 GitHub，跟着重定向送 Authorization
            // 等于把令牌交给对方（ureq 自动跟随时的默认行为也是 RedirectAuthHeaders::Never）。
            if hops == 0
                && let Some(token) = github_token()
            {
                let bearer = format!("Bearer {token}");
                request = request.header("Authorization", bearer.as_str());
            }
        }
        let response = request
            .call()
            .map_err(|err| anyhow!("fetch {url} failed: {err}"))?;
        let status = response.status().as_u16();
        if matches!(status, 301 | 302 | 303 | 307 | 308) {
            if hops >= MAX_REDIRECTS {
                return Err(anyhow!(
                    "fetch {url} failed: more than {MAX_REDIRECTS} redirects"
                ));
            }
            let location = response
                .headers()
                .get("location")
                .and_then(|value| value.to_str().ok())
                .ok_or_else(|| {
                    anyhow!("fetch {url} failed: http {status} without a usable location header")
                })?
                .to_string();
            current = orchestrator_core::resolve_outbound_redirect(&current, &location)
                .map_err(|err| anyhow!("fetch {url} failed: {err}"))?;
            hops += 1;
            continue;
        }
        let mut body = Vec::new();
        response
            .into_body()
            .into_reader()
            .take((MAX_FETCH_BYTES as u64) + 1)
            .read_to_end(&mut body)
            .map_err(|err| anyhow!("read {url} failed: {err}"))?;
        break (status, body);
    };
    if body.len() > MAX_FETCH_BYTES {
        return Err(anyhow!(
            "response from {url} exceeds {MAX_FETCH_BYTES} bytes"
        ));
    }
    if !(200..=299).contains(&status) {
        // 上游响应体只进日志，不回显给调用方：它可能带着令牌回显、内网地址或其他
        // 不该出现在 API 响应里的细节。仓库禁止有损解码（见 core tests 的
        // orchestrator_code_forbids_lossy_text_decoding），所以非 UTF-8 的响应体
        // 不做替换字符降级，只记录字节数。日志同样截断，避免 1MB 错误页刷屏。
        match std::str::from_utf8(&body) {
            Ok(text) => eprintln!(
                "store fetch {url} failed: http {status}: {}",
                text.chars().take(500).collect::<String>()
            ),
            Err(_) => eprintln!(
                "store fetch {url} failed: http {status}: non-UTF-8 body ({} bytes)",
                body.len()
            ),
        }
        return Err(anyhow!("fetch {url} failed: http {status}"));
    }
    serde_json::from_slice(&body).map_err(|err| anyhow!("parse json from {url} failed: {err}"))
}

fn load_index_document(repo_root: &Path, index_url: &str) -> Result<Value> {
    if index_url.starts_with("http://") || index_url.starts_with("https://") {
        return http_get_json(index_url, false);
    }
    let relative = index_url
        .strip_prefix("file://")
        .unwrap_or(index_url)
        .trim_start_matches('/');
    let path = safe_repo_child(repo_root, relative)?;
    let text = fs::read_to_string(&path)
        .map_err(|err| anyhow!("read store index {} failed: {err}", path.display()))?;
    serde_json::from_str(&text)
        .map_err(|err| anyhow!("parse store index {} failed: {err}", path.display()))
}

fn safe_repo_child(repo_root: &Path, relative: &str) -> Result<PathBuf> {
    let relative_path = Path::new(relative);
    if relative_path.is_absolute()
        || relative_path.components().any(|component| {
            matches!(
                component,
                std::path::Component::ParentDir | std::path::Component::Prefix(_)
            )
        })
    {
        return Err(anyhow!("store index path must stay inside the repository"));
    }
    Ok(repo_root.join(relative_path))
}

pub fn installed_services(console: &OrchestratorActionConsole) -> Result<Value> {
    let service_kinds = console
        .services()?
        .into_iter()
        .map(|service| (service.id, service.kind))
        .collect::<std::collections::BTreeMap<_, _>>();
    Ok(installed_services_payload(
        service_kinds,
        console.host_services()?,
    ))
}

fn installed_services_payload(
    service_kinds: std::collections::BTreeMap<String, String>,
    host_services: Vec<HostService>,
) -> Value {
    let mut deployments_by_service = std::collections::BTreeMap::<_, Vec<_>>::new();
    for host_service in host_services {
        deployments_by_service
            .entry(host_service.service_name.clone())
            .or_default()
            .push(host_service);
    }

    let mut map = serde_json::Map::new();
    for (service_name, mut host_services) in deployments_by_service {
        host_services.sort_by(|left, right| {
            left.host_ip
                .cmp(&right.host_ip)
                .then_with(|| left.version.cmp(&right.version))
        });
        let kind = service_kinds
            .get(&service_name)
            .cloned()
            .unwrap_or_else(|| "unknown".to_string());
        let versions = host_services
            .iter()
            .map(|host_service| host_service.version.clone())
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();
        let deployments = host_services
            .into_iter()
            .map(|host_service| {
                json!({
                    "version": host_service.version,
                    "host_ip": host_service.host_ip,
                    "status": host_service.status,
                })
            })
            .collect::<Vec<_>>();
        map.insert(
            service_name,
            json!({
                "version": versions.join(" / "),
                "versions": versions,
                "kind": kind,
                "deployments": deployments,
            }),
        );
    }
    Value::Object(map)
}

/// 拉取（或读缓存）商店索引；不需要 console，调用方应在 console 锁外执行。
pub fn store_index_payload(
    state: &StoreState,
    repo_root: &Path,
    refresh: bool,
) -> Result<(String, bool, Value)> {
    let index_url = configured_index_url();
    let mut cache = state
        .index_cache
        .lock()
        .map_err(|_| anyhow!("store index cache lock poisoned"))?;
    let cached = if refresh {
        None
    } else {
        cache.as_ref().and_then(|(at, url, value)| {
            (url == &index_url && at.elapsed() < INDEX_CACHE_TTL).then(|| value.clone())
        })
    };
    let (index, from_cache) = match cached {
        Some(value) => (value, true),
        None => {
            let value = load_index_document(repo_root, &index_url)?;
            *cache = Some((Instant::now(), index_url.clone(), value.clone()));
            (value, false)
        }
    };
    Ok((index_url, from_cache, index))
}

fn valid_repo_slug(repo: &str) -> bool {
    let parts: Vec<&str> = repo.split('/').collect();
    parts.len() == 2
        && parts.iter().all(|part| {
            !part.is_empty()
                && part.len() <= 100
                && part
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
        })
}

/// GitHub Release 查询；纯网络请求，调用方应在 console 锁外执行。
pub fn github_releases_response(query: &str) -> Result<ApiResponse> {
    let repo = crate::query_value(query, "repo")
        .map_err(|err| StatusError::new(400, err.to_string()))?
        .ok_or_else(|| StatusError::new(400, "query parameter repo=owner/name is required"))?;
    if !valid_repo_slug(&repo) {
        return Ok(ApiResponse::error(400, "repo must look like owner/name"));
    }
    let per_page = crate::query_value(query, "per_page")
        .map_err(|err| StatusError::new(400, err.to_string()))?
        .and_then(|value| value.parse::<u8>().ok())
        .unwrap_or(10)
        .min(30);
    let url = format!("https://api.github.com/repos/{repo}/releases?per_page={per_page}");
    let releases = http_get_json(&url, true)?;
    let releases = releases
        .as_array()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .map(|release| {
            json!({
                "tag_name": release["tag_name"],
                "name": release["name"],
                "prerelease": release["prerelease"],
                "published_at": release["published_at"],
                "html_url": release["html_url"],
                "assets": release["assets"].as_array().cloned().unwrap_or_default().iter().map(|asset| json!({
                    "name": asset["name"],
                    "size": asset["size"],
                    "browser_download_url": asset["browser_download_url"],
                    "content_type": asset["content_type"],
                })).collect::<Vec<_>>(),
            })
        })
        .collect::<Vec<_>>();
    Ok(ApiResponse::ok(json!({
        "repo": repo,
        "releases": releases,
    })))
}

#[cfg(test)]
// The test module remains next to the store-index helpers it exercises.
#[allow(clippy::items_after_test_module)]
mod tests {
    use super::*;

    fn repo_root() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .join("..")
    }

    #[test]
    fn repository_catalog_is_not_reported_as_installed() {
        let console = OrchestratorActionConsole::load_with_database_url(repo_root(), None)
            .expect("load in-memory console");
        assert!(
            !console.services().expect("catalog services").is_empty(),
            "test repository should contain catalog manifests"
        );

        let installed = installed_services(&console).expect("installed services");
        assert_eq!(installed, json!({}));
    }

    #[test]
    fn installed_services_keeps_every_host_deployment() {
        let kinds =
            std::collections::BTreeMap::from([("gateway".to_string(), "gateway".to_string())]);
        let host_services = vec![
            HostService {
                host_ip: "10.0.0.2".to_string(),
                service_name: "gateway".to_string(),
                version: "1.1.0".to_string(),
                status: "stopped".to_string(),
                config: Value::Null,
                labels: Value::Null,
                created_at: String::new(),
                updated_at: String::new(),
            },
            HostService {
                host_ip: "10.0.0.1".to_string(),
                service_name: "gateway".to_string(),
                version: "1.0.0".to_string(),
                status: "running".to_string(),
                config: Value::Null,
                labels: Value::Null,
                created_at: String::new(),
                updated_at: String::new(),
            },
        ];

        let installed = installed_services_payload(kinds, host_services);
        assert_eq!(installed["gateway"]["versions"], json!(["1.0.0", "1.1.0"]));
        assert_eq!(
            installed["gateway"]["deployments"][0]["host_ip"],
            "10.0.0.1"
        );
        assert_eq!(
            installed["gateway"]["deployments"][1]["host_ip"],
            "10.0.0.2"
        );
    }

    #[test]
    fn github_release_route_reports_missing_repo_as_bad_request() {
        let error = github_releases_response("").expect_err("repo is required");
        let status = error
            .downcast_ref::<StatusError>()
            .expect("missing repo should carry an HTTP status");
        assert_eq!(status.0, 400);
        assert!(status.1.contains("repo=owner/name"));
    }

    #[test]
    fn store_mutation_routes_report_missing_targets_as_bad_requests() {
        let state = StoreState::new();
        let mut console = OrchestratorActionConsole::load_with_database_url(repo_root(), None)
            .expect("load in-memory console");
        for (path, expected_field) in [
            ("/store/import", "source_url"),
            ("/store/install", "service_id or source_url"),
        ] {
            let request = ApiRequest {
                method: "POST".to_string(),
                path: path.to_string(),
                headers: std::collections::BTreeMap::new(),
                body: "{}".to_string(),
            };
            let error =
                match route_store_request(&state, &mut console, &repo_root(), &request, path, "") {
                    Some(Err(error)) => error,
                    Some(Ok(response)) => {
                        panic!("{path} unexpectedly returned HTTP {}", response.status)
                    }
                    None => panic!("{path} was not recognized as a store route"),
                };
            let status = error
                .downcast_ref::<StatusError>()
                .expect("missing field should carry an HTTP status");
            assert_eq!(status.0, 400, "path: {path}");
            assert!(status.1.contains(expected_field), "{}", status.1);
        }
    }
}

fn body_object(body: &str) -> Result<serde_json::Map<String, Value>> {
    let trimmed = body.trim();
    if trimmed.is_empty() {
        return Err(StatusError::new(400, "request body must be a JSON object").into());
    }
    serde_json::from_str::<Value>(trimmed)?
        .as_object()
        .cloned()
        .ok_or_else(|| StatusError::new(400, "request body must be a JSON object").into())
}

fn body_str(object: &serde_json::Map<String, Value>, key: &str) -> Option<String> {
    object
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

/// 外部包加载默认关闭。未显式开启时拒绝导入/安装，避免任意远端产物在生产落盘。
fn package_load_disabled_response() -> ApiResponse {
    ApiResponse::error(
        403,
        "external package load is disabled; set ORCHESTRATOR_RELEASE_PACKAGE_LOAD=1 (dev/staging only) to allow store import and install",
    )
}

fn import_response(
    console: &mut OrchestratorActionConsole,
    repo_root: &Path,
    body: &str,
) -> Result<ApiResponse> {
    let object = body_object(body)?;
    let source_url = body_str(&object, "source_url")
        .ok_or_else(|| StatusError::new(400, "source_url is required"))?;
    if !package_load_enabled() {
        return Ok(package_load_disabled_response());
    }
    // body_str 已经过滤了空串，所以 None 覆盖了"没给"和"给了空字符串"两种情况。
    let checksum = body_str(&object, "checksum");
    if require_release_checksum() && checksum.is_none() {
        return Ok(missing_checksum_response());
    }
    let import = console.import_external_release(repo_root, &source_url, checksum.as_deref())?;
    Ok(ApiResponse::created(json!({
        "imported": import,
    })))
}

fn install_response(
    console: &mut OrchestratorActionConsole,
    repo_root: &Path,
    body: &str,
) -> Result<ApiResponse> {
    let object = body_object(body)?;
    let source_url = body_str(&object, "source_url");
    let requested_service_id = body_str(&object, "service_id");
    if source_url.is_none() && requested_service_id.is_none() {
        return Err(StatusError::new(400, "service_id or source_url is required").into());
    }
    if !package_load_enabled() {
        return Ok(package_load_disabled_response());
    }
    let mut imported = None;
    if let Some(source_url) = source_url {
        let checksum = body_str(&object, "checksum");
        if require_release_checksum() && checksum.is_none() {
            return Ok(missing_checksum_response());
        }
        imported =
            Some(console.import_external_release(repo_root, &source_url, checksum.as_deref())?);
    }
    let service_id = requested_service_id
        .or_else(|| imported.as_ref().map(|import| import.service.id.clone()))
        .ok_or_else(|| StatusError::new(400, "service_id or source_url is required"))?;
    let mut request = empty_http_action_request(console, "release.install")?;
    request
        .fields
        .insert("service_id".to_string(), service_id.clone());
    request
        .fields
        .insert("confirm".to_string(), "true".to_string());
    if let Some(version) = body_str(&object, "version").or_else(|| {
        imported
            .as_ref()
            .map(|import| import.release.version.clone())
    }) {
        request.fields.insert("version".to_string(), version);
    }
    for key in [
        "host_ip",
        "endpoint",
        "execute_service_driver",
        "external_service_running",
        "migration_dry_run",
        "gateway_node_id",
    ] {
        if let Some(value) = object.get(key) {
            let text = match value {
                Value::String(text) => text.clone(),
                Value::Bool(flag) => flag.to_string(),
                Value::Number(number) => number.to_string(),
                _ => continue,
            };
            if !text.trim().is_empty() {
                request.fields.insert(key.to_string(), text);
            }
        }
    }
    validate_required_action_fields(console, &request)?;
    let action_result = console.dispatch(request)?;
    Ok(ApiResponse::ok(json!({
        "service_id": service_id,
        "imported": imported,
        "action_result": action_result,
    })))
}

/// 商店路由分发（需要 console 的部分；index 与 github releases 由主路由在锁外处理）。
/// 不匹配返回 None 交回主路由。
///
/// 注意：import/install 会在持有 console 锁期间下载 release 包（最大 64MB）。
/// 这是 v1 的已知取舍——core 的 import_external_release 将拉取与注册作为一个
/// 原子步骤；后续可在 core 拆分 fetch/register 后把下载挪到锁外。
pub fn route_store_request(
    _state: &StoreState,
    console: &mut OrchestratorActionConsole,
    repo_root: &Path,
    request: &ApiRequest,
    path: &str,
    _query: &str,
) -> Option<Result<ApiResponse>> {
    let segments: Vec<&str> = path
        .trim_matches('/')
        .split('/')
        .filter(|segment| !segment.is_empty())
        .collect();
    match (request.method.as_str(), segments.as_slice()) {
        ("GET", ["store", "status"]) => Some(Ok(ApiResponse::ok(json!({
            "index_url": configured_index_url(),
            "package_load_enabled": package_load_enabled(),
            "github_token_configured": github_token().is_some(),
            "require_release_checksum": require_release_checksum(),
            "allow_private_release_source": allow_private_release_source(),
            "store": if console.uses_persistent_store() { "persistent" } else { "memory" },
        })))),
        ("POST", ["store", "import"]) => Some(import_response(console, repo_root, &request.body)),
        ("POST", ["store", "install"]) => Some(install_response(console, repo_root, &request.body)),
        _ => None,
    }
}
