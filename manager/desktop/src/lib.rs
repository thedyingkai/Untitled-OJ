//! OJOS 编排器桌面入口：以原生 WebView 承载现有 Web 控制面。

mod local_agent;

pub use local_agent::{
    DesktopAgentHandle, DesktopAgentPhase, DesktopAgentShutdown, DesktopAgentStatus,
    DesktopHostPlatform, DesktopManagedExecutionCapability,
    DesktopManagedExecutionUnavailableReason, desktop_managed_execution_capability,
    desktop_managed_execution_capability_for, unavailable_desktop_agent,
};

use anyhow::{Context, Result, anyhow, bail};
use clap::Parser;
use directories::ProjectDirs;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::Duration;
use url::{Host, Url};

const MAX_AUTH_CONFIG_BYTES: usize = 64 * 1024;
pub const DESKTOP_SMOKE_SUCCESS_PATH: &str = "/__ojos_desktop_smoke_success__";
pub const DESKTOP_SMOKE_FAILURE_PATH: &str = "/__ojos_desktop_smoke_failure__";

#[derive(Debug, Parser)]
#[command(name = "ojos-orchestrator-desktop")]
#[command(about = "OJOS Orchestrator 本地桌面控制面")]
#[command(version)]
pub struct Cli {
    /// 仓库或发行包根目录；安装版默认使用随应用打包的资源目录。
    #[arg(long)]
    pub repo_root: Option<PathBuf>,

    /// Web UI 构建产物；默认 <repo-root>/manager/web/dist。
    #[arg(long)]
    pub web_root: Option<PathBuf>,

    /// Embedded SQLite and UI state directory.
    #[arg(long)]
    pub data_dir: Option<PathBuf>,

    /// Compatibility flag; rejected while managed local execution is unavailable.
    #[arg(long, value_name = "PATH")]
    pub registry_credentials: Option<PathBuf>,

    /// 连接已有 daemon，而不是启动内嵌控制面。
    #[arg(long)]
    pub daemon_url: Option<String>,
}

#[derive(Debug, Clone)]
pub enum LaunchConfig {
    Embedded {
        repo_root: Option<PathBuf>,
        web_root: Option<PathBuf>,
        data_dir: PathBuf,
        bootstrap_secret: String,
    },
    External {
        url: Url,
    },
}

pub fn resolve_launch_config(cli: Cli) -> Result<LaunchConfig> {
    if cli.registry_credentials.is_some() {
        bail!(
            "--registry-credentials is unavailable because Desktop does not run a managed local execution Agent; configure credentials on a standalone Agent"
        );
    }
    if let Some(raw_url) = cli.daemon_url {
        return Ok(LaunchConfig::External {
            url: validate_external_url(&raw_url)?,
        });
    }

    let bootstrap_secret = generate_session_secret()?;
    let data_dir = cli.data_dir.unwrap_or_else(default_data_dir);
    Ok(LaunchConfig::Embedded {
        repo_root: cli.repo_root,
        web_root: cli.web_root,
        data_dir,
        bootstrap_secret,
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmbeddedPaths {
    pub repo_root: PathBuf,
    pub web_root: PathBuf,
}

/// Resolve runtime assets only after Tauri knows the installed application's
/// resource directory. Explicit CLI overrides win; otherwise packaged assets
/// are preferred and the current repository is the development fallback.
pub fn resolve_embedded_paths(
    repo_root: Option<&Path>,
    web_root: Option<&Path>,
    resource_dir: &Path,
) -> Result<EmbeddedPaths> {
    let executable = std::env::current_exe().ok();
    resolve_embedded_paths_from(repo_root, web_root, resource_dir, executable.as_deref())
}

fn resolve_embedded_paths_from(
    repo_root: Option<&Path>,
    web_root: Option<&Path>,
    resource_dir: &Path,
    executable: Option<&Path>,
) -> Result<EmbeddedPaths> {
    let repo_root = match repo_root {
        Some(path) => canonical_directory(path, "repository root")?,
        None => match packaged_runtime_root(resource_dir, executable) {
            Some(path) => canonical_directory(&path, "Desktop resource root")?,
            None => canonical_directory(Path::new("."), "development repository root")?,
        },
    };
    if !schemas_present(&repo_root) {
        bail!(
            "Orchestrator schemas are missing under {}; reinstall Desktop or pass --repo-root",
            repo_root.join("platform/schemas/orchestrator").display()
        );
    }
    let requested_web_root = web_root
        .map(Path::to_path_buf)
        .unwrap_or_else(|| repo_root.join("manager").join("web").join("dist"));
    let web_root = canonical_directory(&requested_web_root, "Web UI root")?;
    if !web_root.join("index.html").is_file() {
        bail!(
            "Web UI entry is missing at {}; rebuild or reinstall Desktop",
            web_root.join("index.html").display()
        );
    }
    Ok(EmbeddedPaths {
        repo_root,
        web_root,
    })
}

fn runtime_assets_present(root: &Path) -> bool {
    schemas_present(root)
        && root
            .join("manager")
            .join("web")
            .join("dist")
            .join("index.html")
            .is_file()
}

fn packaged_runtime_root(resource_dir: &Path, executable: Option<&Path>) -> Option<PathBuf> {
    let executable_dir = executable.and_then(Path::parent);
    [
        Some(resource_dir),
        resource_dir.parent(),
        executable_dir,
        executable_dir.and_then(Path::parent),
    ]
    .into_iter()
    .flatten()
    .find(|candidate| runtime_assets_present(candidate))
    .map(Path::to_path_buf)
}

fn schemas_present(root: &Path) -> bool {
    root.join("platform")
        .join("schemas")
        .join("orchestrator")
        .join("actions-v1.yaml")
        .is_file()
}

fn default_data_dir() -> PathBuf {
    ProjectDirs::from("org", "OJOS", "Untitled-OJ")
        .map(|directories| directories.data_local_dir().join("orchestrator"))
        .unwrap_or_else(|| PathBuf::from(".ojos-data").join("orchestrator"))
}

pub fn validate_external_url(raw: &str) -> Result<Url> {
    let mut url = Url::parse(raw).with_context(|| format!("invalid daemon URL {raw}"))?;
    if !matches!(url.scheme(), "http" | "https") {
        bail!("daemon URL must use http or https");
    }
    if url.username() != "" || url.password().is_some() {
        bail!("daemon URL must not contain credentials");
    }
    if url.query().is_some() || url.fragment().is_some() {
        bail!("daemon URL must not contain a query or fragment");
    }
    if url.path() != "/" && !url.path().is_empty() {
        bail!("daemon URL must point to the origin root");
    }
    let loopback = match url.host() {
        Some(Host::Ipv4(address)) => address.is_loopback(),
        Some(Host::Ipv6(address)) => address.is_loopback(),
        Some(Host::Domain(domain)) => domain.eq_ignore_ascii_case("localhost"),
        None => false,
    };
    if url.scheme() == "http" && !loopback {
        bail!("every non-loopback control plane must use HTTPS");
    }
    url.set_path("/");
    Ok(url)
}

pub fn same_origin(candidate: &Url, expected: &Url) -> bool {
    candidate.scheme() == expected.scheme()
        && candidate.host() == expected.host()
        && candidate.port_or_known_default() == expected.port_or_known_default()
}

pub fn navigation_allowed(
    candidate: &Url,
    daemon_origin: &Url,
    authorization_origin: Option<&Url>,
) -> bool {
    same_origin(candidate, daemon_origin)
        || authorization_origin.is_some_and(|origin| same_origin(candidate, origin))
}

/// Reads the unauthenticated v1 auth configuration before an external Desktop
/// window is created. This lets Tauri permit only the configured OIDC
/// authorization origin while keeping every unrelated top-level navigation
/// blocked.
pub fn discover_external_authorization_origin(daemon_origin: &Url) -> Result<Option<Url>> {
    let config_url = daemon_origin
        .join("/api/v1/auth/config")
        .context("construct remote auth configuration URL")?;
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(5)))
        .http_status_as_error(false)
        .max_redirects(0)
        .build()
        .into();
    let response = agent
        .get(config_url.as_str())
        .header("Accept", "application/json")
        .call()
        .with_context(|| format!("fetch remote auth configuration from {config_url}"))?;
    let status = response.status().as_u16();
    let mut bytes = Vec::new();
    response
        .into_body()
        .into_reader()
        .take(MAX_AUTH_CONFIG_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .context("read remote auth configuration")?;
    if status != 200 {
        bail!("remote auth configuration returned HTTP {status}");
    }
    if bytes.len() > MAX_AUTH_CONFIG_BYTES {
        bail!("remote auth configuration exceeds {MAX_AUTH_CONFIG_BYTES} bytes");
    }
    authorization_origin_from_auth_config(&bytes)
}

fn authorization_origin_from_auth_config(bytes: &[u8]) -> Result<Option<Url>> {
    let envelope: serde_json::Value =
        serde_json::from_slice(bytes).context("parse remote auth configuration")?;
    let request_id = envelope
        .pointer("/meta/request_id")
        .and_then(serde_json::Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| anyhow!("remote auth configuration is missing v1 request_id"))?;
    let data = envelope
        .get("data")
        .and_then(serde_json::Value::as_object)
        .ok_or_else(|| anyhow!("remote auth configuration is missing v1 data"))?;
    let mode = data
        .get("mode")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| anyhow!("remote auth configuration is missing mode ({request_id})"))?;
    if mode != "oidc" {
        return Ok(None);
    }
    let endpoint = data
        .get("authorization_endpoint")
        .and_then(serde_json::Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| anyhow!("OIDC auth configuration is missing authorization_endpoint"))?;
    let parsed = Url::parse(endpoint).context("parse OIDC authorization_endpoint")?;
    if parsed.scheme() != "https" || parsed.host().is_none() {
        bail!("OIDC authorization_endpoint must use HTTPS and include a host");
    }
    if !parsed.username().is_empty() || parsed.password().is_some() {
        bail!("OIDC authorization_endpoint must not contain credentials");
    }
    let serialized = format!("{}/", parsed.origin().ascii_serialization());
    Url::parse(&serialized)
        .map(Some)
        .context("normalize OIDC authorization origin")
}

pub fn initialization_script(url: &Url, token: Option<&str>, embedded: bool) -> Result<String> {
    let origin = url.origin().ascii_serialization();
    let origin = serde_json::to_string(&origin)?;
    let token = serde_json::to_string(token.unwrap_or(""))?;
    let auth = if embedded {
        format!(
            "let secret = {token}; window.__OJOS_AUTH_READY__ = fetch('/api/v1/auth/desktop/exchange', {{ method: 'POST', credentials: 'same-origin', headers: {{ 'Content-Type': 'application/json', 'x-ojos-desktop-bootstrap': secret }}, body: '{{}}' }}).then(async response => {{ const body = await response.json(); if (!response.ok) throw new Error(body.detail || 'desktop bootstrap failed'); window.__OJOS_CSRF_TOKEN__ = body.csrf_token || ''; }}).finally(() => {{ secret = ''; }});"
        )
    } else {
        "window.__OJOS_AUTH_READY__ = Promise.resolve();".to_string()
    };
    Ok(format!(
        "(() => {{ if (window.location.origin === {origin}) {{ {auth} Object.defineProperty(window, '__OJOS_DESKTOP__', {{ value: true }}); }} }})();"
    ))
}

fn canonical_directory(path: &Path, label: &str) -> Result<PathBuf> {
    let canonical = fs::canonicalize(path)
        .with_context(|| format!("{label} {} does not exist", path.display()))?;
    if !canonical.is_dir() {
        bail!("{label} {} is not a directory", canonical.display());
    }
    Ok(canonical)
}

fn generate_session_secret() -> Result<String> {
    let mut bytes = [0_u8; 32];
    getrandom::fill(&mut bytes).map_err(|err| anyhow!("generate Desktop session secret: {err}"))?;
    let mut token = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        use std::fmt::Write as _;
        write!(token, "{byte:02x}").expect("writing to String cannot fail");
    }
    Ok(token)
}

pub fn desktop_smoke_mode() -> bool {
    std::env::var("OJOS_DESKTOP_SMOKE")
        .ok()
        .is_some_and(|value| matches!(value.trim(), "1" | "true" | "TRUE" | "True"))
}

pub fn desktop_smoke_duration_ms() -> Result<u64> {
    let raw = std::env::var("OJOS_DESKTOP_SMOKE_DURATION_MS").ok();
    parse_desktop_smoke_duration_ms(raw.as_deref(), desktop_smoke_mode())
}

fn parse_desktop_smoke_duration_ms(raw: Option<&str>, smoke_mode: bool) -> Result<u64> {
    let Some(raw) = raw else { return Ok(0) };
    if !smoke_mode {
        bail!("OJOS_DESKTOP_SMOKE_DURATION_MS requires OJOS_DESKTOP_SMOKE=1");
    }
    let duration = raw
        .trim()
        .parse::<u64>()
        .context("OJOS_DESKTOP_SMOKE_DURATION_MS must be an integer")?;
    if duration > 3_600_000 {
        bail!("OJOS_DESKTOP_SMOKE_DURATION_MS must not exceed one hour");
    }
    Ok(duration)
}

/// Browser-side half of the Desktop startup integration check. The native
/// harness exits successfully only after the one-time bootstrap promise has
/// produced a CSRF token and an authenticated v1 request returns a valid
/// envelope. The sentinel navigation is same-origin and never opens a browser.
pub fn desktop_smoke_script() -> &'static str {
    r#"(() => {
      const success = '/__ojos_desktop_smoke_success__';
      const failure = '/__ojos_desktop_smoke_failure__';
      const durationMs = __OJOS_DESKTOP_SMOKE_DURATION_MS__;
      const fail = (error) => {
        const detail = String(error instanceof Error ? error.message : error).slice(0, 512);
        window.location.replace(`${failure}?detail=${encodeURIComponent(detail)}`);
      };
      (async () => {
        const ready = window.__OJOS_AUTH_READY__;
        if (!ready || typeof ready.then !== 'function') {
          throw new Error('Desktop auth readiness promise is missing');
        }
        let timer = 0;
        try {
          await Promise.race([
            ready,
            new Promise((_, reject) => {
              timer = window.setTimeout(
                () => reject(new Error('Desktop bootstrap timed out')),
                15000,
              );
            }),
          ]);
        } finally {
          if (timer) window.clearTimeout(timer);
        }
        if (typeof window.__OJOS_CSRF_TOKEN__ !== 'string' || !window.__OJOS_CSRF_TOKEN__.trim()) {
          throw new Error('Desktop bootstrap did not issue a CSRF token');
        }
        const probe = async () => {
          const controller = new AbortController();
          const timeout = window.setTimeout(() => controller.abort(), 5000);
          try {
            const response = await fetch('/api/v1/capabilities', {
              method: 'GET',
              credentials: 'same-origin',
              headers: { Accept: 'application/json' },
              signal: controller.signal,
            });
            const body = await response.json();
            if (!response.ok) throw new Error(body.detail || `Capabilities returned HTTP ${response.status}`);
            if (!body?.meta?.request_id || body.meta.api_version !== 'v1') {
              throw new Error('Capabilities returned an invalid v1 envelope');
            }
            if (!Array.isArray(body?.data?.actions)) {
              throw new Error('Capabilities did not publish an action matrix');
            }
          } finally {
            window.clearTimeout(timeout);
          }
        };
        await probe();
        let shell = null;
        for (let attempt = 0; attempt < 100 && !shell; attempt += 1) {
          shell = document.querySelector('.shell');
          if (!shell) await new Promise((resolve) => window.setTimeout(resolve, 50));
        }
        if (!shell) throw new Error('Embedded Web UI did not mount its application shell');
        const soakStarted = performance.now();
        while (performance.now() - soakStarted < durationMs) {
          if (!shell.isConnected) throw new Error('Embedded Web UI shell was detached');
          await probe();
          const remaining = durationMs - (performance.now() - soakStarted);
          if (remaining <= 0) break;
          const delayStarted = performance.now();
          await new Promise((resolve) => window.setTimeout(resolve, Math.min(1000, remaining)));
          if (performance.now() - delayStarted > 15000) {
            throw new Error('Desktop WebView event loop was unresponsive for more than 15 seconds');
          }
        }
        window.location.replace(success);
      })().catch(fail);
    })();"#
}

pub fn desktop_smoke_script_for(duration_ms: u64) -> String {
    desktop_smoke_script().replace(
        "__OJOS_DESKTOP_SMOKE_DURATION_MS__",
        &duration_ms.to_string(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    fn cli() -> Cli {
        Cli {
            repo_root: None,
            web_root: None,
            data_dir: None,
            registry_credentials: None,
            daemon_url: None,
        }
    }

    #[test]
    fn external_mode_accepts_loopback_http_and_rejects_remote_http_by_default() {
        assert!(validate_external_url("http://127.0.0.1:8090").is_ok());
        assert!(validate_external_url("http://localhost:8090/").is_ok());
        assert!(validate_external_url("http://192.0.2.10:8090").is_err());
        assert!(validate_external_url("https://orchestrator.example.test").is_ok());
        assert!(validate_external_url("file:///tmp/index.html").is_err());
    }

    #[test]
    fn navigation_policy_matches_only_the_configured_origin() {
        let expected = Url::parse("http://127.0.0.1:38123/").unwrap();
        assert!(same_origin(
            &Url::parse("http://127.0.0.1:38123/operations").unwrap(),
            &expected
        ));
        assert!(!same_origin(
            &Url::parse("http://127.0.0.1:8090/").unwrap(),
            &expected
        ));
        assert!(!same_origin(
            &Url::parse("https://127.0.0.1:38123/").unwrap(),
            &expected
        ));

        let authorization = Url::parse("https://login.example.test/").unwrap();
        assert!(navigation_allowed(
            &Url::parse("https://login.example.test/authorize?state=opaque").unwrap(),
            &expected,
            Some(&authorization),
        ));
        assert!(navigation_allowed(
            &Url::parse("http://127.0.0.1:38123/api/v1/auth/oidc/callback").unwrap(),
            &expected,
            Some(&authorization),
        ));
        assert!(!navigation_allowed(
            &Url::parse("https://unknown.example.test/").unwrap(),
            &expected,
            Some(&authorization),
        ));
    }

    #[test]
    fn remote_oidc_config_allows_only_the_exact_https_authorization_origin() {
        let body = br#"{
          "data": {
            "mode": "oidc",
            "authorization_endpoint": "https://login.example.test:8443/oauth2/authorize?tenant=ojos"
          },
          "meta": {"request_id": "req-auth", "api_version": "v1"}
        }"#;
        let origin = authorization_origin_from_auth_config(body)
            .unwrap()
            .unwrap();
        assert_eq!(origin.as_str(), "https://login.example.test:8443/");

        let insecure = String::from_utf8_lossy(body).replace(
            "https://login.example.test:8443",
            "http://login.example.test:8443",
        );
        assert!(authorization_origin_from_auth_config(insecure.as_bytes()).is_err());

        let development = br#"{
          "data": {"mode": "development"},
          "meta": {"request_id": "req-dev", "api_version": "v1"}
        }"#;
        assert!(
            authorization_origin_from_auth_config(development)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn external_auth_discovery_fetches_the_v1_config_without_following_login_redirects() {
        use std::io::{Read as _, Write as _};
        use std::net::TcpListener;
        use std::thread;

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0_u8; 4096];
            let read = stream.read(&mut request).unwrap();
            let request = String::from_utf8_lossy(&request[..read]);
            assert!(request.starts_with("GET /api/v1/auth/config HTTP/1.1"));
            let body = r#"{"data":{"mode":"oidc","authorization_endpoint":"https://login.example.test/authorize"},"meta":{"request_id":"req-live","api_version":"v1"}}"#;
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            )
            .unwrap();
        });
        let daemon = Url::parse(&format!("http://{address}/")).unwrap();
        let authorization = discover_external_authorization_origin(&daemon)
            .unwrap()
            .unwrap();
        assert_eq!(authorization.as_str(), "https://login.example.test/");
        server.join().unwrap();
    }

    #[test]
    fn initialization_script_scopes_and_escapes_the_token() {
        let url = Url::parse("http://127.0.0.1:38123/").unwrap();
        let script = initialization_script(&url, Some("quote-'\"-token"), true).unwrap();
        assert!(script.contains("window.location.origin === \"http://127.0.0.1:38123\""));
        assert!(script.contains("/api/v1/auth/desktop/exchange"));
        assert!(script.contains("quote-'\\\"-token"));
        assert!(!script.contains("localStorage"));
    }

    #[test]
    fn desktop_smoke_waits_for_bootstrap_and_an_authenticated_v1_envelope() {
        let script = desktop_smoke_script_for(1_800_000);
        assert!(script.contains("window.__OJOS_AUTH_READY__"));
        assert!(script.contains("window.__OJOS_CSRF_TOKEN__"));
        assert!(script.contains("/api/v1/capabilities"));
        assert!(script.contains("document.querySelector('.shell')"));
        assert!(script.contains("credentials: 'same-origin'"));
        assert!(script.contains("const durationMs = 1800000"));
        assert!(script.contains("controller.abort()"));
        assert!(script.contains("shell.isConnected"));
        assert!(script.contains("event loop was unresponsive"));
        assert!(script.contains(DESKTOP_SMOKE_SUCCESS_PATH));
        assert!(script.contains(DESKTOP_SMOKE_FAILURE_PATH));
        assert!(!script.contains("localStorage"));
        assert!(!script.contains("window.open"));
    }

    #[test]
    fn desktop_soak_duration_is_bounded_and_requires_smoke_mode() {
        assert_eq!(parse_desktop_smoke_duration_ms(None, false).unwrap(), 0);
        assert_eq!(
            parse_desktop_smoke_duration_ms(Some("1800000"), true).unwrap(),
            1_800_000
        );
        assert!(parse_desktop_smoke_duration_ms(Some("1"), false).is_err());
        assert!(parse_desktop_smoke_duration_ms(Some("invalid"), true).is_err());
        assert!(parse_desktop_smoke_duration_ms(Some("3600001"), true).is_err());
        assert!(desktop_smoke_script_for(0).contains("const durationMs = 0"));
        assert!(desktop_smoke_script_for(3_600_000).contains("const durationMs = 3600000"));
    }

    #[test]
    fn external_mode_uses_oidc_web_session_without_injecting_a_bearer_token() {
        let mut args = cli();
        args.daemon_url = Some("http://127.0.0.1:8090".to_string());
        let config = resolve_launch_config(args).unwrap();
        match config {
            LaunchConfig::External { url } => {
                assert_eq!(url.as_str(), "http://127.0.0.1:8090/");
            }
            LaunchConfig::Embedded { .. } => panic!("expected external mode"),
        }
        let script = initialization_script(
            &Url::parse("https://orchestrator.example.test/").unwrap(),
            None,
            false,
        )
        .unwrap();
        assert!(!script.contains("LEGACY_TOKEN"));
        assert!(!script.contains("Bearer"));
    }

    #[test]
    fn registry_credentials_are_rejected_without_a_managed_local_agent() {
        let mut args = cli();
        args.registry_credentials = Some(PathBuf::from("unused-secret.json"));
        let error = resolve_launch_config(args).unwrap_err().to_string();
        assert!(error.contains("Desktop does not run a managed local execution Agent"));
        assert!(error.contains("standalone Agent"));
        assert!(!error.contains("unused-secret.json"));
    }

    #[test]
    fn embedded_mode_generates_a_strong_web_bootstrap_secret() {
        let generated = generate_session_secret().unwrap();
        assert_eq!(generated.len(), 64);
        assert!(
            generated
                .chars()
                .all(|character| character.is_ascii_hexdigit())
        );
        let root = tempfile::tempdir().unwrap();
        let web_root = root.path().join("manager").join("web").join("dist");
        fs::create_dir_all(&web_root).unwrap();
        fs::write(web_root.join("index.html"), "<!doctype html>").unwrap();
        let mut args = cli();
        let schema_dir = root
            .path()
            .join("platform")
            .join("schemas")
            .join("orchestrator");
        fs::create_dir_all(&schema_dir).unwrap();
        fs::write(schema_dir.join("actions-v1.yaml"), "version: 1").unwrap();
        args.repo_root = Some(root.path().to_path_buf());
        let config = resolve_launch_config(args).unwrap();
        match config {
            LaunchConfig::Embedded {
                repo_root,
                web_root: configured_web_root,
                data_dir: _,
                bootstrap_secret,
            } => {
                let paths = resolve_embedded_paths(
                    repo_root.as_deref(),
                    configured_web_root.as_deref(),
                    root.path(),
                )
                .unwrap();
                assert_eq!(paths.repo_root, fs::canonicalize(root.path()).unwrap());
                assert_eq!(paths.web_root, fs::canonicalize(web_root).unwrap());
                assert_eq!(bootstrap_secret.len(), 64);
                assert!(
                    bootstrap_secret
                        .chars()
                        .all(|character| character.is_ascii_hexdigit())
                );
            }
            LaunchConfig::External { .. } => panic!("expected embedded mode"),
        }
    }

    #[test]
    fn packaged_resources_are_used_without_current_directory_assumptions() {
        let resources = tempfile::tempdir().unwrap();
        let schema_dir = resources
            .path()
            .join("platform")
            .join("schemas")
            .join("orchestrator");
        let web_dir = resources.path().join("manager").join("web").join("dist");
        fs::create_dir_all(&schema_dir).unwrap();
        fs::create_dir_all(&web_dir).unwrap();
        fs::write(schema_dir.join("actions-v1.yaml"), "version: 1").unwrap();
        fs::write(web_dir.join("index.html"), "packaged").unwrap();

        let paths = resolve_embedded_paths(None, None, resources.path()).unwrap();
        assert_eq!(paths.repo_root, fs::canonicalize(resources.path()).unwrap());
        assert_eq!(paths.web_root, fs::canonicalize(web_dir).unwrap());
    }

    #[test]
    fn tauri_bundle_maps_every_runtime_asset_to_the_resolver_layout() {
        let config: serde_json::Value =
            serde_json::from_str(include_str!("../tauri.conf.json")).unwrap();
        assert_eq!(
            config
                .pointer("/build/frontendDist")
                .and_then(|value| value.as_str()),
            Some("../web/dist")
        );
        let resources = config
            .pointer("/bundle/resources")
            .and_then(|value| value.as_object())
            .expect("bundle.resources must use explicit source-to-target mappings");
        let targets = resources
            .values()
            .map(|value| {
                value
                    .as_str()
                    .expect("resource target must be a string")
                    .trim_end_matches('/')
            })
            .collect::<HashSet<_>>();
        for target in [
            "manager/web/dist",
            "platform/schemas/orchestrator",
            "sets",
            "store/index.json",
        ] {
            assert!(
                targets.contains(target),
                "missing installed resource {target}"
            );
        }

        let repository = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let services = repository.join("services");
        for entry in fs::read_dir(&services).unwrap() {
            let entry = entry.unwrap();
            if !entry.file_type().unwrap().is_dir() {
                continue;
            }
            let service_id = entry.file_name().to_string_lossy().into_owned();
            for manifest in ["service.yaml", "release.yaml"] {
                if !entry.path().join(manifest).is_file() {
                    continue;
                }
                let source = format!("../../services/{service_id}/{manifest}");
                let target = format!("services/{service_id}/{manifest}");
                assert_eq!(
                    resources.get(&source).and_then(|value| value.as_str()),
                    Some(target.as_str()),
                    "installed Desktop resource map is missing {source}"
                );
            }
        }
    }

    #[test]
    fn portable_archive_finds_resources_next_to_its_bin_directory() {
        let archive = tempfile::tempdir().unwrap();
        let bin = archive.path().join("bin");
        let executable = bin.join(if cfg!(windows) {
            "ojos-orchestrator-desktop.exe"
        } else {
            "ojos-orchestrator-desktop"
        });
        let schema_dir = archive
            .path()
            .join("platform")
            .join("schemas")
            .join("orchestrator");
        let web_dir = archive.path().join("manager").join("web").join("dist");
        fs::create_dir_all(&bin).unwrap();
        fs::create_dir_all(&schema_dir).unwrap();
        fs::create_dir_all(&web_dir).unwrap();
        fs::write(&executable, "desktop").unwrap();
        fs::write(schema_dir.join("actions-v1.yaml"), "version: 1").unwrap();
        fs::write(web_dir.join("index.html"), "portable").unwrap();

        // Linux resolves non-AppImage resources to /usr/lib/<app-name>, not
        // next to a tarball executable. The executable fallback is what makes
        // the portable archive independent of the current working directory.
        let unrelated_resource_dir = archive.path().join("missing-system-resource-dir");
        let paths =
            resolve_embedded_paths_from(None, None, &unrelated_resource_dir, Some(&executable))
                .unwrap();
        assert_eq!(paths.repo_root, fs::canonicalize(archive.path()).unwrap());
        assert_eq!(paths.web_root, fs::canonicalize(web_dir).unwrap());
    }
}
