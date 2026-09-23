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
