//! 连接层：TCP 监听、固定大小工作线程池、请求分发与静态资源写出。

use crate::artifact_store::{ArtifactRetentionPolicy, ArtifactStore};
use crate::auth::{
    ORCHESTRATOR_INTERNAL_TOKEN_HEADER, Principal, internal_token_check, resolve_principal,
};
use crate::build_identity::{BuildIdentity, RuntimeProfile};
use crate::catalog_registry::CatalogRegistry;
use crate::compatibility::{self, LegacyApiMode};
use crate::desktop_session::{
    DESKTOP_BOOTSTRAP_HEADER, DESKTOP_CSRF_HEADER, DesktopSessionManager, session_cookie,
};
use crate::durable::{DurableJobStore, DurableStore};
use crate::http::{
    ApiRequest, ApiResponse, HttpStream, SECURITY_RESPONSE_HEADERS, WRITE_TIMEOUT,
    has_json_content_type, query_bool, query_value, read_http_request, requires_json_content_type,
    write_agent_protocol_response, write_http_response,
};
use crate::node_identity::{NodeIdentityService, NodePeerIdentity};
use crate::observability::{self, Observability};
use crate::oidc::{OidcConfig, OidcVerifier};
use crate::oidc_web::{
    CSRF_HEADER as OIDC_CSRF_HEADER, OidcBrowserConfig, OidcWebError, OidcWebSessionManager,
    expired_session_cookie, session_cookie as oidc_session_cookie,
};
use crate::routes::{handle_api_request_with_internal_token, status_for_error};
use crate::topology_provider::{
    HttpManagementProviderConfig, TopologyProviderConfig, TopologyProviderSaga,
};
use crate::workload_credentials::{HttpWorkloadTokenIssuer, WorkloadTokenIssuer};
use crate::{agent_api, api_v1, market_api, static_site, topology_worker, ui_layout};
use anyhow::{Context, Result, anyhow};
use orchestrator_control_plane::{
    DurableOperationStatus, Job, JobStore, OperationCoordinator, OperationRepository,
};
use orchestrator_legacy::{
    NodeRecord, OrchestratorActionConsole, OrchestratorStore, TopologyDrift, TopologyDriftKind,
    TopologyReconciliationState, TopologyResourceKind,
};
use orchestrator_runtime::ArtifactReference;
use orchestrator_storage::{
    AdvisoryLockGuard, JobMetricsSnapshot, PostgresOptions, PostgresOrchestratorStore,
    PostgresTlsTrust, SqliteOrchestratorStore,
};
use std::io::{ErrorKind, Read, Write};
use std::net::{Shutdown, SocketAddr, TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, TrySendError};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

/// Desktop/test keeps a compact pool. A production daemon reserves enough
/// workers for 100 simultaneous Agent long polls plus UI/operation traffic.
const DEFAULT_MAX_WORKERS: usize = 32;
const DEFAULT_PRODUCTION_MAX_WORKERS: usize = 160;
const MIN_PRODUCTION_MAX_WORKERS: usize = 128;
const DEFAULT_LOG_RETENTION_DAYS: u64 = 30;
const DEFAULT_RETENTION_SWEEP_SECONDS: u64 = 3_600;
/// 待处理连接的有界队列容量。队列满说明已经过载，直接对新连接回 503，
/// 而不是无上限地 spawn 线程把整台机器拖垮。
const CONNECTION_QUEUE_CAPACITY: usize = 64;
const DESKTOP_AGENT_BOOTSTRAP_HEADER: &str = "x-ojos-agent-bootstrap";

struct ServerContext {
    console: ConsoleState,
    store_state: market_api::StoreState,
    repo_root: PathBuf,
    web_root: PathBuf,
    internal_token: Option<String>,
    store_kind: String,
    startup_warnings: Vec<String>,
    build_identity: BuildIdentity,
    desktop_session: Option<DesktopSessionManager>,
    durable_store: Option<DurableStore>,
    job_store: Option<Mutex<DurableJobStore>>,
    topology_provider: Option<TopologyProviderSaga>,
    catalog_registry: Option<CatalogRegistry>,
    artifact_store: ArtifactStore,
    oidc_verifier: Option<OidcVerifier>,
    oidc_web_session: Option<OidcWebSessionManager>,
    ephemeral_dev: bool,
    node_identity: Option<Arc<NodeIdentityService>>,
    desktop_agent_secret: Option<String>,
    legacy_api_mode: LegacyApiMode,
    observability: Arc<Observability>,
    history_retention: Option<HistoryRetentionPolicy>,
    workload_token_issuer: Option<Arc<dyn WorkloadTokenIssuer>>,
}

#[derive(Debug, Clone, Copy)]
struct HistoryRetentionPolicy {
    retention_ms: i64,
    sweep_interval: Duration,
}

impl HistoryRetentionPolicy {
    fn from_env() -> Result<Self> {
        let days = std::env::var("ORCHESTRATOR_LOG_RETENTION_DAYS")
            .ok()
            .map(|value| value.parse::<u64>())
            .transpose()
            .context("ORCHESTRATOR_LOG_RETENTION_DAYS must be an integer")?
            .unwrap_or(DEFAULT_LOG_RETENTION_DAYS);
        if !(1..=3_650).contains(&days) {
            return Err(anyhow!(
                "ORCHESTRATOR_LOG_RETENTION_DAYS must be between 1 and 3650"
            ));
        }
        let sweep_seconds = std::env::var("ORCHESTRATOR_LOG_RETENTION_SWEEP_SECONDS")
            .ok()
            .map(|value| value.parse::<u64>())
            .transpose()
            .context("ORCHESTRATOR_LOG_RETENTION_SWEEP_SECONDS must be an integer")?
            .unwrap_or(DEFAULT_RETENTION_SWEEP_SECONDS);
        if !(60..=86_400).contains(&sweep_seconds) {
            return Err(anyhow!(
                "ORCHESTRATOR_LOG_RETENTION_SWEEP_SECONDS must be between 60 and 86400"
            ));
        }
        let retention_ms = days
            .checked_mul(24 * 60 * 60 * 1_000)
            .and_then(|value| i64::try_from(value).ok())
            .ok_or_else(|| anyhow!("configured log retention exceeds the supported range"))?;
        Ok(Self {
            retention_ms,
            sweep_interval: Duration::from_secs(sweep_seconds),
        })
    }
}

enum ConsoleState {
    /// Durable consoles are request-local coordinators over transactional
    /// repositories. Cloning does not copy database state and requires no
    /// process-wide console lock.
    Durable(OrchestratorActionConsole),
    /// The in-memory console exists only for explicit ephemeral development
    /// and tests, where the console itself owns the mutable state.
    Ephemeral(Mutex<OrchestratorActionConsole>),
}

impl ServerContext {
    fn with_console<R>(
        &self,
        callback: impl FnOnce(&mut OrchestratorActionConsole) -> R,
    ) -> std::result::Result<R, ()> {
        match &self.console {
            ConsoleState::Durable(template) => {
                let mut console = template.clone();
                Ok(callback(&mut console))
            }
            ConsoleState::Ephemeral(console) => {
                let mut console = console.lock().map_err(|_| ())?;
                Ok(callback(&mut console))
            }
        }
    }
}

enum ConnectionStream {
    Plain(TcpStream),
    Tls(Box<rustls::StreamOwned<rustls::ServerConnection, TcpStream>>),
}

impl ConnectionStream {
    fn accept(stream: TcpStream, tls: Option<Arc<rustls::ServerConfig>>) -> Result<Self> {
        match tls {
            Some(config) => Ok(Self::Tls(Box::new(rustls::StreamOwned::new(
                rustls::ServerConnection::new(config).context("create TLS server connection")?,
                stream,
            )))),
            None => Ok(Self::Plain(stream)),
        }
    }

    fn is_tls(&self) -> bool {
        matches!(self, Self::Tls(_))
    }

    fn peer_addr(&self) -> std::io::Result<SocketAddr> {
        match self {
            Self::Plain(stream) => stream.peer_addr(),
            Self::Tls(stream) => stream.sock.peer_addr(),
        }
    }

    fn shutdown(&self, how: Shutdown) -> std::io::Result<()> {
        match self {
            Self::Plain(stream) => stream.shutdown(how),
            Self::Tls(stream) => stream.sock.shutdown(how),
        }
    }

    fn node_peer_identity(&self) -> Result<Option<NodePeerIdentity>> {
        let Self::Tls(stream) = self else {
            return Ok(None);
        };
        let Some(certificate) = stream
            .conn
            .peer_certificates()
            .and_then(|chain| chain.first())
        else {
            return Ok(None);
        };
        NodePeerIdentity::from_certificate_der(certificate.as_ref()).map(Some)
    }
}

impl Read for ConnectionStream {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        match self {
            Self::Plain(stream) => stream.read(buffer),
            Self::Tls(stream) => stream.read(buffer),
        }
    }
}

impl Write for ConnectionStream {
    fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
        match self {
            Self::Plain(stream) => stream.write(buffer),
            Self::Tls(stream) => stream.write(buffer),
        }
    }

    fn flush(&mut self) -> std::io::Result<()> {
        match self {
            Self::Plain(stream) => stream.flush(),
            Self::Tls(stream) => stream.flush(),
        }
    }
}

impl HttpStream for ConnectionStream {
    fn set_http_read_timeout(&mut self, timeout: Option<Duration>) -> std::io::Result<()> {
        match self {
            Self::Plain(stream) => stream.set_read_timeout(timeout),
            Self::Tls(stream) => stream.sock.set_read_timeout(timeout),
        }
    }

    fn set_http_write_timeout(&mut self, timeout: Option<Duration>) -> std::io::Result<()> {
        match self {
            Self::Plain(stream) => stream.set_write_timeout(timeout),
            Self::Tls(stream) => stream.sock.set_write_timeout(timeout),
        }
    }
}

/// 启动可嵌入 HTTP 服务所需的固定配置。
#[derive(Debug, Clone)]
pub struct EmbeddedServerOptions {
    pub repo_root: PathBuf,
    pub web_root: PathBuf,
    /// Durable, writable storage for bounded Agent artifacts. This must be
    /// separate from read-only installed application resources.
    pub artifact_root: PathBuf,
    pub bind_addr: SocketAddr,
    pub internal_token: Option<String>,
    /// Single-use secret exchanged by the embedded WebView for an HttpOnly
    /// Desktop session. It must never authenticate any other protocol.
    pub desktop_bootstrap_secret: Option<String>,
    /// Independent credential used only by the loopback Desktop Agent.
    pub desktop_agent_secret: Option<String>,
    pub storage: EmbeddedStorage,
}

#[derive(Debug, Clone)]
pub enum EmbeddedStorage {
    /// Explicit developer/test mode. Never selected implicitly.
    Ephemeral,
    /// Desktop's durable local database.
    Sqlite { database_path: PathBuf },
    /// Transitional PostgreSQL path; the storage crate owns the pooled v1
    /// implementation and will replace the legacy console adapter.
    Postgres { database_url: String },
}

/// 与服务线程绑定的生命周期句柄。
///
/// `shutdown` 可以重复调用；若调用方直接丢弃句柄，`Drop` 也会关闭监听器并等待工作
/// 线程退出，桌面应用不会遗留孤儿 daemon。
pub struct EmbeddedServerHandle {
    local_addr: SocketAddr,
    shutdown: Arc<AtomicBool>,
    thread: Option<JoinHandle<Result<()>>>,
}

#[derive(Clone)]
pub struct EmbeddedServerShutdown {
    local_addr: SocketAddr,
    shutdown: Arc<AtomicBool>,
}

impl EmbeddedServerShutdown {
    pub fn shutdown(&self) {
        self.shutdown.store(true, Ordering::Release);
        let _ = TcpStream::connect_timeout(&self.local_addr, Duration::from_millis(100));
    }
}

impl EmbeddedServerHandle {
    pub fn local_addr(&self) -> SocketAddr {
        self.local_addr
    }

    pub fn shutdown(&self) -> Result<()> {
        self.shutdown.store(true, Ordering::Release);
        // 唤醒可能正等待 accept 的服务线程；连接随即关闭，不携带任何数据。
        let _ = TcpStream::connect_timeout(&self.local_addr, Duration::from_millis(100));
        Ok(())
    }

    pub fn shutdown_handle(&self) -> EmbeddedServerShutdown {
        EmbeddedServerShutdown {
            local_addr: self.local_addr,
            shutdown: Arc::clone(&self.shutdown),
        }
    }

    pub fn is_finished(&self) -> bool {
        self.thread.as_ref().is_none_or(JoinHandle::is_finished)
    }

    /// Waits for queued and active requests after admission has stopped. If the
    /// drain deadline expires, the server thread is detached so a standalone
    /// daemon process can terminate instead of hanging indefinitely.
    pub fn join_timeout(mut self, timeout: Duration) -> Result<()> {
        let deadline = std::time::Instant::now() + timeout;
        while !self.is_finished() && std::time::Instant::now() < deadline {
            thread::sleep(Duration::from_millis(25));
        }
        if !self.is_finished() {
            let _detached = self.thread.take();
            return Err(anyhow!(
                "orchestrator graceful drain exceeded {} seconds",
                timeout.as_secs()
            ));
        }
        self.join_thread()
    }

    /// 等待服务线程自然结束。嵌入场景通常先调用 [`Self::shutdown`]。
    pub fn join(mut self) -> Result<()> {
        self.join_thread()
    }

    fn join_thread(&mut self) -> Result<()> {
        let Some(thread) = self.thread.take() else {
            return Ok(());
        };
        thread
            .join()
            .map_err(|_| anyhow!("orchestrator server thread panicked"))?
    }
}

impl Drop for EmbeddedServerHandle {
    fn drop(&mut self) {
        let _ = self.shutdown();
        if let Err(err) = self.join_thread() {
            eprintln!("orchestrator server shutdown error: {err}");
        }
    }
}

fn max_workers(production: bool) -> usize {
    let default = if production {
        DEFAULT_PRODUCTION_MAX_WORKERS
    } else {
        DEFAULT_MAX_WORKERS
    };
    std::env::var("ORCHESTRATOR_MAX_WORKERS")
        .ok()
        .and_then(|value| value.trim().parse::<usize>().ok())
        .filter(|value| *value > 0 && (!production || *value >= MIN_PRODUCTION_MAX_WORKERS))
        .unwrap_or(default)
}

fn configured_topology_provider() -> Result<Option<TopologyProviderSaga>> {
    let gateway_origin = std::env::var("ORCHESTRATOR_GATEWAY_ADMIN_ORIGIN")
        .ok()
        .filter(|value| !value.trim().is_empty());
    let auth_origin = std::env::var("ORCHESTRATOR_AUTH_ADMIN_ORIGIN")
        .ok()
        .filter(|value| !value.trim().is_empty());
    let (gateway_origin, auth_origin) = match (gateway_origin, auth_origin) {
        (Some(gateway_origin), Some(auth_origin)) => (gateway_origin, auth_origin),
        (None, None) => return Ok(None),
        _ => {
            return Err(anyhow!(
                "Gateway and Auth topology management origins must be configured together"
            ));
        }
    };
    let mut gateway = HttpManagementProviderConfig::new(gateway_origin)
        .context("validate Gateway topology management origin")?;
    let mut auth = HttpManagementProviderConfig::new(auth_origin)
        .context("validate Auth topology management origin")?;
    if let Some(token) = std::env::var("ORCHESTRATOR_GATEWAY_ADMIN_TOKEN")
        .ok()
        .filter(|value| !value.is_empty())
    {
        gateway = gateway
            .with_bearer_token(token)
            .context("validate Gateway topology management token")?;
    }
    if let Some(token) = std::env::var("ORCHESTRATOR_AUTH_ADMIN_TOKEN")
        .ok()
        .filter(|value| !value.is_empty())
    {
        auth = auth
            .with_bearer_token(token)
            .context("validate Auth topology management token")?;
    }
    let mut config = TopologyProviderConfig::new(Some(gateway), Some(auth));
    let max_request_bytes = std::env::var("ORCHESTRATOR_PROVIDER_MAX_REQUEST_BYTES")
        .ok()
        .map(|value| value.parse::<usize>())
        .transpose()
        .context("ORCHESTRATOR_PROVIDER_MAX_REQUEST_BYTES must be an integer")?
        .unwrap_or(8 * 1024 * 1024);
    let max_response_bytes = std::env::var("ORCHESTRATOR_PROVIDER_MAX_RESPONSE_BYTES")
        .ok()
        .map(|value| value.parse::<usize>())
        .transpose()
        .context("ORCHESTRATOR_PROVIDER_MAX_RESPONSE_BYTES must be an integer")?
        .unwrap_or(64 * 1024);
    config = config
        .with_size_limits(max_request_bytes, max_response_bytes)
        .context("validate topology provider size limits")?;
    if let Some(timeout_ms) = std::env::var("ORCHESTRATOR_PROVIDER_TIMEOUT_MS")
        .ok()
        .map(|value| value.parse::<u64>())
        .transpose()
        .context("ORCHESTRATOR_PROVIDER_TIMEOUT_MS must be an integer")?
    {
        config = config
            .with_timeout(Duration::from_millis(timeout_ms))
            .context("validate topology provider timeout")?;
    }
    TopologyProviderSaga::from_config(config)
        .map(Some)
        .context("configure topology management providers")
}

/// Reconciles durable state before a listener is bound. Startup therefore
/// fails closed when recovery or schema access fails.
fn recover_control_plane(store: Option<&DurableStore>) -> Result<Option<Mutex<DurableJobStore>>> {
    let Some(store) = store else {
        return Ok(None);
    };
    let mut jobs = store.job_store();
    let mut operations = store.operation_store();
    OperationCoordinator::new(&mut operations, &mut jobs)
        .recover(current_time_ms())
        .map_err(|error| anyhow!("recover Operations and expired jobs: {error}"))?;
    for heads in store
        .list_topology_heads()
        .map_err(|error| anyhow!("load topology apply heads during recovery: {error}"))?
    {
        let Some(operation_id) = heads.applying_operation_id.as_deref() else {
            continue;
        };
        let Some(operation) = operations
            .get(operation_id)
            .map_err(|error| anyhow!("load topology Operation during recovery: {error}"))?
        else {
            continue;
        };
        if operation.status == DurableOperationStatus::Confirmed {
            OperationCoordinator::new(&mut operations, &mut jobs)
                .enqueue(operation_id, current_time_ms())
                .map_err(|error| {
                    anyhow!("resume confirmed topology Operation {operation_id}: {error}")
                })?;
        } else if operation.status == DurableOperationStatus::NeedsAttention {
            let mut status = store
                .topology_status(&heads.topology_id)
                .map_err(|error| anyhow!("load topology status during recovery: {error}"))?
                .ok_or_else(|| anyhow!("topology {} has no durable status", heads.topology_id))?;
            status.state = TopologyReconciliationState::Degraded;
            status.last_operation_id = Some(operation_id.to_string());
            status.updated_at = format!("unix-ms:{}", current_time_ms());
            status.drift = vec![TopologyDrift {
                resource_kind: TopologyResourceKind::Authority,
                resource_id: heads.topology_id.clone(),
                kind: TopologyDriftKind::Unreachable,
                detail: "provider outcome could not be proven after control-plane recovery"
                    .to_string(),
            }];
            store
                .put_topology_status(&status)
                .map_err(|error| anyhow!("mark recovered topology as degraded: {error}"))?;
        }
    }
    Ok(Some(Mutex::new(jobs)))
}

/// 从仓库状态加载控制台并在后台线程启动服务。
pub fn start_embedded_server(options: EmbeddedServerOptions) -> Result<EmbeddedServerHandle> {
    validate_storage_exposure(&options)?;
    let (console, durable_store, active_lock) = match &options.storage {
        EmbeddedStorage::Ephemeral => (
            OrchestratorActionConsole::load_with_database_url(options.repo_root.clone(), None)?,
            None,
            None,
        ),
        EmbeddedStorage::Sqlite { database_path } => {
            let mut store = SqliteOrchestratorStore::open(database_path).with_context(|| {
                format!("open Desktop SQLite store {}", database_path.display())
            })?;
            if options.desktop_agent_secret.is_some() {
                let existing = store.get_node("desktop-local")?;
                if existing.is_none() {
                    store.upsert_node(NodeRecord {
                        node_id: "desktop-local".to_string(),
                        host_ip: "127.0.0.1".to_string(),
                        parent_node_id: String::new(),
                        role: "standalone".to_string(),
                        labels: serde_json::json!({
                            "embedded": true,
                            "runtime": "docker",
                        }),
                        status: "READY".to_string(),
                        created_at: format!("unix-ms:{}", current_time_ms()),
                        // Startup is not an Agent observation. The first real
                        // claim/heartbeat/complete advances this timestamp.
                        updated_at: "unix-ms:0".to_string(),
                    })?;
                }
            }
            let console = OrchestratorActionConsole::load_with_store(
                options.repo_root.clone(),
                "sqlite",
                store.clone(),
            )?;
            (console, Some(DurableStore::Sqlite(store)), None)
        }
        EmbeddedStorage::Postgres { database_url } => {
            let mut postgres_options = PostgresOptions::default();
            if let Some(path) = std::env::var_os("ORCHESTRATOR_POSTGRES_CA_CERT") {
                postgres_options.tls_trust = PostgresTlsTrust::CaCertificate(PathBuf::from(path));
            }
            let store = PostgresOrchestratorStore::connect(database_url, postgres_options)
                .context("open production PostgreSQL store")?;
            let active_lock = store
                .pool()
                .acquire_single_active()
                .context("acquire single-active control-plane lock")?;
            let console = OrchestratorActionConsole::load_with_store(
                options.repo_root.clone(),
                "postgres",
                store.clone(),
            )?;
            (
                console,
                Some(DurableStore::Postgres(store)),
                Some(active_lock),
            )
        }
    };
    start_embedded_server_with_components(options, console, durable_store, active_lock)
}

/// 使用已经构造的控制台启动服务；测试和需要精确数据库配置的宿主可使用此入口。
pub fn start_embedded_server_with_console(
    options: EmbeddedServerOptions,
    console: OrchestratorActionConsole,
) -> Result<EmbeddedServerHandle> {
    validate_storage_exposure(&options)?;
    start_embedded_server_with_components(options, console, None, None)
}

fn validate_storage_exposure(options: &EmbeddedServerOptions) -> Result<()> {
    if !options.bind_addr.ip().is_loopback()
        && !matches!(&options.storage, EmbeddedStorage::Postgres { .. })
    {
        return Err(anyhow!(
            "non-loopback control-plane listeners require PostgreSQL production mode; SQLite and ephemeral storage are loopback-only"
        ));
    }
    Ok(())
}

fn start_embedded_server_with_components(
    options: EmbeddedServerOptions,
    console: OrchestratorActionConsole,
    durable_store: Option<DurableStore>,
    active_lock: Option<AdvisoryLockGuard>,
) -> Result<EmbeddedServerHandle> {
    let production = matches!(&options.storage, EmbeddedStorage::Postgres { .. });
    let runtime_profile = match &options.storage {
        EmbeddedStorage::Postgres { .. } => RuntimeProfile::Production,
        EmbeddedStorage::Sqlite { .. } => RuntimeProfile::Desktop,
        EmbeddedStorage::Ephemeral => RuntimeProfile::Ephemeral,
    };
    let build_identity = BuildIdentity::compiled(runtime_profile);
    build_identity.require_production_commit()?;
    let observability = Observability::from_env().context("configure observability")?;
    let history_retention = durable_store
        .as_ref()
        .map(|store| {
            let policy = HistoryRetentionPolicy::from_env()?;
            run_history_retention_pass(store, policy)?;
            Ok::<_, anyhow::Error>(policy)
        })
        .transpose()?;
    let artifact_store = ArtifactStore::open(&options.artifact_root)
        .context("open durable orchestrator artifact storage")?;
    let legacy_api_mode = LegacyApiMode::configured().context("configure legacy API mode")?;
    let oidc_verifier = if production {
        let config = OidcConfig::from_env().context("configure production OIDC verifier")?;
        Some(OidcVerifier::discover(config).context("initialize production OIDC verifier")?)
    } else {
        None
    };
    let oidc_web_session = oidc_verifier
        .as_ref()
        .map(|verifier| {
            OidcBrowserConfig::from_env(verifier)
                .map(OidcWebSessionManager::new)
                .context("configure production OIDC Web login")
        })
        .transpose()?;
    if production && !options.web_root.join("index.html").is_file() {
        return Err(anyhow!(
            "production PostgreSQL mode requires a built Web UI at {}",
            options.web_root.join("index.html").display()
        ));
    }
    // This is deliberately resolved before bind: a production daemon must not
    // briefly expose plaintext HTTP when TLS/Node CA configuration is absent.
    let node_identity = if production {
        NodeIdentityService::from_env(true)
            .context("configure Node enrollment CA and mTLS listener")?
    } else {
        None
    };
    let topology_provider = configured_topology_provider()?;
    let mut catalog_registry = CatalogRegistry::from_env(&options.repo_root)
        .map_err(|error| anyhow!("configure Catalog v2 registry: {error}"))?;
    if production && catalog_registry.is_none() {
        return Err(anyhow!(
            "production PostgreSQL mode requires ORCHESTRATOR_CATALOG_TRUST_KEYS and ORCHESTRATOR_CATALOG_SOURCES"
        ));
    }
    if catalog_registry.is_none()
        && matches!(&options.storage, EmbeddedStorage::Sqlite { .. })
        && options.desktop_bootstrap_secret.is_some()
    {
        catalog_registry = Some(
            CatalogRegistry::desktop(&options.repo_root)
                .map_err(|error| anyhow!("configure Desktop Catalog v2 registry: {error}"))?,
        );
    }
    if let (Some(registry), Some(storage)) = (&catalog_registry, &durable_store) {
        registry
            .bootstrap(storage)
            .map_err(|error| anyhow!("bootstrap trusted Catalog v2 sources: {error}"))?;
    }
    observability
        .initialize_control_plane_anomalies(durable_store.as_ref())
        .context("initialize durable control-plane anomaly counters")?;
    let job_store = recover_control_plane(durable_store.as_ref())?;
    let retention =
        ArtifactRetentionPolicy::from_env().context("configure artifact retention policy")?;
    let protected_artifacts = job_store
        .as_ref()
        .map(active_artifact_ids)
        .transpose()?
        .unwrap_or_default();
    let gc = artifact_store
        .collect_garbage(
            &protected_artifacts,
            retention,
            std::time::SystemTime::now(),
        )
        .context("collect expired orchestrator artifacts")?;
    if gc.removed_files > 0 {
        eprintln!(
            "orchestrator artifact retention removed {} file(s), {} bytes; {} bytes retained",
            gc.removed_files, gc.removed_bytes, gc.retained_bytes
        );
    }
    let listener = TcpListener::bind(options.bind_addr)
        .with_context(|| format!("bind {}", options.bind_addr))?;
    let local_addr = listener.local_addr().context("read bound server address")?;
    listener
        .set_nonblocking(true)
        .context("configure non-blocking orchestrator listener")?;
    let shutdown = Arc::new(AtomicBool::new(false));
    let thread_shutdown = Arc::clone(&shutdown);
    let thread = thread::Builder::new()
        .name("orchestrator-http".to_string())
        .spawn(move || {
            run_server(
                listener,
                console,
                durable_store,
                active_lock,
                options,
                oidc_verifier,
                oidc_web_session,
                node_identity,
                legacy_api_mode,
                observability,
                topology_provider,
                catalog_registry,
                artifact_store,
                job_store,
                history_retention,
                build_identity,
                thread_shutdown,
            )
        })
        .context("spawn orchestrator server thread")?;
    Ok(EmbeddedServerHandle {
        local_addr,
        shutdown,
        thread: Some(thread),
    })
}

fn active_artifact_ids(
    jobs: &Mutex<DurableJobStore>,
) -> Result<std::collections::BTreeSet<String>> {
    let jobs = jobs
        .lock()
        .map_err(|_| anyhow!("durable Job store lock is poisoned"))?
        .list()
        .map_err(|error| anyhow!("list Jobs for artifact retention: {error}"))?;
    Ok(jobs
        .iter()
        .filter(|job| !job.status.is_terminal())
        .filter_map(job_artifact_reference)
        .map(|reference| reference.artifact_id)
        .collect())
}

fn job_artifact_reference(job: &Job) -> Option<ArtifactReference> {
    ["/offline_oci_artifact", "/install/offline_oci_artifact"]
        .iter()
        .filter_map(|pointer| job.payload.pointer(pointer))
        .find_map(|value| serde_json::from_value(value.clone()).ok())
}

// The embedded and remote launch paths deliberately hand one complete,
// immutable startup snapshot to the server thread.
#[allow(clippy::too_many_arguments)]
fn run_server(
    listener: TcpListener,
    console: OrchestratorActionConsole,
    durable_store: Option<DurableStore>,
    _active_lock: Option<AdvisoryLockGuard>,
    options: EmbeddedServerOptions,
    oidc_verifier: Option<OidcVerifier>,
    oidc_web_session: Option<OidcWebSessionManager>,
    node_identity: Option<Arc<NodeIdentityService>>,
    legacy_api_mode: LegacyApiMode,
    observability: Arc<Observability>,
    topology_provider: Option<TopologyProviderSaga>,
    catalog_registry: Option<CatalogRegistry>,
    artifact_store: ArtifactStore,
    job_store: Option<Mutex<DurableJobStore>>,
    history_retention: Option<HistoryRetentionPolicy>,
    build_identity: BuildIdentity,
    shutdown: Arc<AtomicBool>,
) -> Result<()> {
    let local_addr = listener.local_addr().context("read server address")?;
    let scheme = if node_identity.is_some() {
        "https"
    } else {
        "http"
    };
    eprintln!("OJOS Orchestrator daemon listening on {scheme}://{local_addr}");
    if options.web_root.join("index.html").is_file() {
        eprintln!(
            "OJOS Orchestrator web UI: {scheme}://{local_addr}/ (root {})",
            options.web_root.display()
        );
    } else {
        eprintln!(
            "web UI assets not found at {}; serving API and a placeholder page only",
            options.web_root.display()
        );
    }
    let persistent_store = console.uses_persistent_store();
    let store_kind = console.store_kind().to_string();
    let startup_warnings = console.warnings().to_vec();
    let internal_token = options
        .internal_token
        .map(|token| token.trim().to_string())
        .filter(|token| !token.is_empty());
    let workload_token_issuer = HttpWorkloadTokenIssuer::from_env()?
        .map(|issuer| Arc::new(issuer) as Arc<dyn WorkloadTokenIssuer>);
    let desktop_session = options
        .desktop_bootstrap_secret
        .as_deref()
        .map(DesktopSessionManager::new);
    let desktop_agent_secret = options.desktop_agent_secret.clone();
    let ephemeral_dev = matches!(&options.storage, EmbeddedStorage::Ephemeral)
        && desktop_session.is_none()
        && desktop_agent_secret.is_none()
        && internal_token.is_none();
    let context = Arc::new(ServerContext {
        console: if persistent_store {
            ConsoleState::Durable(console)
        } else {
            ConsoleState::Ephemeral(Mutex::new(console))
        },
        store_state: market_api::StoreState::new(),
        repo_root: options.repo_root,
        web_root: options.web_root,
        internal_token,
        store_kind,
        startup_warnings,
        build_identity,
        desktop_session,
        durable_store,
        job_store,
        topology_provider,
        catalog_registry,
        artifact_store,
        oidc_verifier,
        oidc_web_session,
        ephemeral_dev,
        node_identity,
        desktop_agent_secret,
        legacy_api_mode,
        observability,
        history_retention,
        workload_token_issuer,
    });

    let production = matches!(&options.storage, EmbeddedStorage::Postgres { .. });
    let worker_count = max_workers(production);
    eprintln!(
        "orchestrator daemon worker pool: {worker_count} threads, queue {CONNECTION_QUEUE_CAPACITY}"
    );
    let (sender, receiver) = mpsc::sync_channel::<TcpStream>(CONNECTION_QUEUE_CAPACITY);
    let receiver = Arc::new(Mutex::new(receiver));
    let topology_thread = if let Some(storage) = context.durable_store.as_ref() {
        let storage = storage.clone();
        let provider = context.topology_provider.clone();
        let shutdown = Arc::clone(&shutdown);
        Some(
            thread::Builder::new()
                .name("orchestrator-control-plane".to_string())
                .spawn(move || topology_worker::run_loop(storage, provider, shutdown))
                .context("spawn durable control-plane worker")?,
        )
    } else {
        None
    };
    let lease_recovery_thread = context
        .durable_store
        .as_ref()
        .map(|storage| {
            let storage = storage.clone();
            let shutdown = Arc::clone(&shutdown);
            thread::Builder::new()
                .name("orchestrator-lease-recovery".to_string())
                .spawn(move || topology_worker::run_lease_recovery_loop(storage, shutdown))
                .context("spawn unique lease-recovery worker")
        })
        .transpose()?;
    let history_retention_thread = match (context.durable_store.as_ref(), context.history_retention)
    {
        (Some(storage), Some(policy)) => {
            let storage = storage.clone();
            let shutdown = Arc::clone(&shutdown);
            Some(
                thread::Builder::new()
                    .name("orchestrator-history-retention".to_string())
                    .spawn(move || run_history_retention_loop(storage, policy, shutdown))
                    .context("spawn history-retention worker")?,
            )
        }
        _ => None,
    };
    let mut workers: Vec<JoinHandle<()>> = Vec::with_capacity(worker_count);
    for _ in 0..worker_count {
        let receiver = Arc::clone(&receiver);
        let context = Arc::clone(&context);
        workers.push(thread::spawn(move || worker_loop(&context, &receiver)));
    }

    while !shutdown.load(Ordering::Acquire) {
        let stream = match listener.accept() {
            Ok((stream, _peer)) => stream,
            Err(err) if err.kind() == ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(10));
                continue;
            }
            Err(err) if shutdown.load(Ordering::Acquire) => {
                eprintln!("orchestrator listener stopped during shutdown: {err}");
                break;
            }
            Err(err) => return Err(err).context("accept orchestrator connection"),
        };
        if shutdown.load(Ordering::Acquire) {
            break;
        }
        // A nonblocking listener can yield nonblocking accepted sockets on Windows. The HTTP
        // reader intentionally uses blocking reads with a total deadline; restore that contract
        // explicitly so a packet-arrival race is not misreported as an immediate timeout.
        stream
            .set_nonblocking(false)
            .context("configure accepted orchestrator connection")?;
        match sender.try_send(stream) {
            Ok(()) => {}
            Err(TrySendError::Full(mut stream)) => {
                context.observability.record_overload();
                reject_overloaded(&mut stream, context.node_identity.is_some())
            }
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
    if let Some(worker) = topology_thread {
        let _ = worker.join();
    }
    if let Some(worker) = lease_recovery_thread {
        let _ = worker.join();
    }
    if let Some(worker) = history_retention_thread {
        let _ = worker.join();
    }
    Ok(())
}

fn run_history_retention_pass(store: &DurableStore, policy: HistoryRetentionPolicy) -> Result<()> {
    let now_ms = current_time_ms();
    let completed_before_ms = now_ms.saturating_sub(policy.retention_ms);
    let report = store
        .purge_terminal_history(completed_before_ms, now_ms)
        .map_err(|error| anyhow!("purge retained control-plane history: {error}"))?;
    if report.operation_logs_deleted > 0
        || report.job_events_deleted > 0
        || report.idempotency_records_deleted > 0
    {
        eprintln!(
            "orchestrator history retention removed {} Operation log(s), {} Job event(s), and {} expired idempotency record(s)",
            report.operation_logs_deleted,
            report.job_events_deleted,
            report.idempotency_records_deleted,
        );
    }
    Ok(())
}

fn run_history_retention_loop(
    store: DurableStore,
    policy: HistoryRetentionPolicy,
    shutdown: Arc<AtomicBool>,
) {
    while !shutdown.load(Ordering::Acquire) {
        let deadline = std::time::Instant::now() + policy.sweep_interval;
        while !shutdown.load(Ordering::Acquire) && std::time::Instant::now() < deadline {
            thread::sleep(Duration::from_secs(1));
        }
        if shutdown.load(Ordering::Acquire) {
            break;
        }
        if let Err(error) = run_history_retention_pass(&store, policy) {
            eprintln!("orchestrator history retention sweep failed: {error}");
        }
    }
}

fn current_time_ms() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};

    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(i64::MAX as u128) as i64
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
        let Ok(stream) = received else {
            return;
        };
        let tls_config = context
            .node_identity
            .as_ref()
            .map(|identity| identity.server_config());
        let mut stream = match ConnectionStream::accept(stream, tls_config) {
            Ok(stream) => stream,
            Err(error) => {
                eprintln!("orchestrator daemon TLS connection setup error: {error}");
                continue;
            }
        };
        if let Err(err) = handle_connection(context, &mut stream) {
            eprintln!("orchestrator daemon connection error: {err}");
        }
        // Windows may report WSAECONNRESET to a reader when the socket is simply dropped
        // immediately after the response. Half-close the write side so clients observe EOF/FIN
        // after the complete HTTP body instead of a spurious reset.
        let _ = stream.flush();
        let _ = stream.shutdown(Shutdown::Write);
    }
}

/// 队列已满：立刻回 503 并关闭连接，把背压传给调用方。
fn reject_overloaded(stream: &mut TcpStream, tls_enabled: bool) {
    if tls_enabled {
        let _ = stream.shutdown(Shutdown::Both);
        return;
    }
    let _ = stream.set_write_timeout(Some(WRITE_TIMEOUT));
    let response = ApiResponse::error(
        503,
        "orchestrator daemon connection queue is full; retry shortly",
    );
    if let Err(err) = write_http_response(stream, response) {
        eprintln!("orchestrator daemon overload response error: {err}");
    }
    let _ = stream.flush();
    let _ = stream.shutdown(Shutdown::Write);
}

fn handle_connection(context: &ServerContext, stream: &mut ConnectionStream) -> Result<()> {
    match read_http_request(stream) {
        Ok(request) => {
            let peer = stream
                .peer_addr()
                .map(|address| address.ip().to_string())
                .unwrap_or_else(|_| "unknown".to_string());
            let _observation =
                observability::begin_request(Arc::clone(&context.observability), &request, peer);
            dispatch_request(context, stream, request)
        }
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

fn legacy_compat_response(
    context: &ServerContext,
    path: &str,
    response: ApiResponse,
) -> ApiResponse {
    if compatibility::is_legacy_api_path(path) {
        context.legacy_api_mode.decorate(response)
    } else {
        response
    }
}

fn oidc_web_problem(error: OidcWebError) -> ApiResponse {
    let (status, code) = match &error {
        OidcWebError::InvalidCsrf => (403, "OIDC_CSRF_REJECTED"),
        OidcWebError::InvalidState => (401, "OIDC_STATE_REJECTED"),
        OidcWebError::InvalidSession => (401, "OIDC_SESSION_REJECTED"),
        OidcWebError::MissingCode => (400, "OIDC_CODE_MISSING"),
        OidcWebError::Configuration(_) => (400, "OIDC_REQUEST_INVALID"),
        OidcWebError::Verification(_) => (401, "OIDC_TOKEN_REJECTED"),
        OidcWebError::Capacity => (503, "OIDC_SESSION_CAPACITY"),
        OidcWebError::Entropy | OidcWebError::Poisoned => (503, "OIDC_SESSION_UNAVAILABLE"),
    };
    ApiResponse::problem(
        status,
        code,
        error.to_string(),
        api_v1::next_request_id(),
        None,
    )
    .with_header("Cache-Control", "no-store")
}

fn dispatch_request(
    context: &ServerContext,
    stream: &mut ConnectionStream,
    mut request: ApiRequest,
) -> Result<()> {
    let mut session_principal: Option<Principal> = None;
    let mut oidc_session_csrf: Option<String> = None;
    let path = request.path.split('?').next().unwrap_or("/").to_string();
    let legacy_request = compatibility::is_legacy_api_path(&path);

    if request.method == "GET" && path == "/metrics" {
        let now_ms = current_time_ms();
        let observation = match context.durable_store.as_ref() {
            Some(store) => context
                .observability
                .observe_durable_control_plane(store, now_ms)
                .map(Some),
            None => Ok(None),
        };
        let mut metrics = context.observability.render_prometheus();
        metrics.push_str(&render_job_metrics(
            observation.as_ref().ok().and_then(Option::as_ref),
            context.job_store.is_some(),
        ));
        let status = if observation.is_ok() { 200 } else { 503 };
        return write_http_response(
            stream,
            ApiResponse::text(status, metrics, "text/plain; version=0.0.4; charset=utf-8")
                .with_header("Cache-Control", "no-store"),
        );
    }
    if legacy_request && context.legacy_api_mode == LegacyApiMode::Gone10 {
        let request_id = format!("req-legacy-{}", current_time_ms());
        let response = compatibility::gone_response(&path, &request_id)
            .with_header("X-Request-ID", request_id);
        return write_http_response(stream, response);
    }
    // 所有变更请求（包括空 body）都必须声明 JSON 内容类型。空表单 POST 同样能被
    // 任意站点跨域直接发出，不能让它绕过最基本的 CSRF 门禁。
    if requires_json_content_type(&request) && !has_json_content_type(&request.headers) {
        if agent_api::is_agent_path(&path) {
            return write_agent_protocol_response(
                stream,
                ApiResponse::problem(
                    415,
                    "AGENT_CONTENT_TYPE_REQUIRED",
                    "mutating Agent requests must send Content-Type: application/json",
                    "req-agent-content-type",
                    None,
                ),
            );
        }
        return write_http_response(
            stream,
            ApiResponse::error(
                415,
                "mutating requests must send Content-Type: application/json",
            ),
        );
    }

    let query = request
        .path
        .split_once('?')
        .map(|(_, query)| query)
        .unwrap_or("")
        .to_string();

    if request.method == "GET" && path == "/api/v1/auth/config" {
        let request_id = api_v1::next_request_id();
        let data = if let Some(manager) = &context.oidc_web_session {
            let config = manager.config();
            serde_json::json!({
                "mode": "oidc",
                "issuer": config.issuer,
                "client_id": config.client_id,
                "audience": config.audience,
                "scopes": config.scopes,
                "authorization_endpoint": config.authorization_endpoint,
                "start_url": "/api/v1/auth/oidc/start",
            })
        } else if context.desktop_session.is_some() {
            serde_json::json!({"mode": "desktop", "local_session": true})
        } else if context.ephemeral_dev {
            serde_json::json!({"mode": "development", "local_session": false})
        } else {
            serde_json::json!({"mode": "unconfigured", "local_session": false})
        };
        return write_http_response(
            stream,
            api_v1::envelope(200, data, request_id).with_header("Cache-Control", "no-store"),
        );
    }

    if request.method == "GET" && path == "/api/v1/auth/oidc/start" {
        let Some(manager) = &context.oidc_web_session else {
            return write_http_response(
                stream,
                ApiResponse::problem(
                    404,
                    "OIDC_WEB_LOGIN_DISABLED",
                    "OIDC browser login is not enabled on this daemon",
                    api_v1::next_request_id(),
                    None,
                ),
            );
        };
        let return_to = match query_value(&query, "return_to") {
            Ok(value) => value,
            Err(error) => {
                return write_http_response(
                    stream,
                    ApiResponse::problem(
                        400,
                        "OIDC_RETURN_TO_INVALID",
                        error.to_string(),
                        api_v1::next_request_id(),
                        None,
                    ),
                );
            }
        };
        return match manager.begin(return_to.as_deref()) {
            Ok(start) => {
                let mut response = ApiResponse::ok(serde_json::json!({"redirect": start.location}));
                response.status = 302;
                write_http_response(
                    stream,
                    response
                        .with_header("Location", start.location)
                        .with_header("Cache-Control", "no-store"),
                )
            }
            Err(error) => write_http_response(stream, oidc_web_problem(error)),
        };
    }

    if request.method == "GET" && path == "/api/v1/auth/oidc/callback" {
        let Some(manager) = &context.oidc_web_session else {
            return write_http_response(
                stream,
                ApiResponse::problem(
                    404,
                    "OIDC_WEB_LOGIN_DISABLED",
                    "OIDC browser login is not enabled on this daemon",
                    api_v1::next_request_id(),
                    None,
                ),
            );
        };
        let Some(verifier) = &context.oidc_verifier else {
            return write_http_response(
                stream,
                ApiResponse::problem(
                    503,
                    "OIDC_VERIFIER_UNAVAILABLE",
                    "OIDC verifier is unavailable",
                    api_v1::next_request_id(),
                    None,
                ),
            );
        };
        let state = query_value(&query, "state").ok().flatten();
        if let Some(provider_error) = query_value(&query, "error").ok().flatten() {
            let _ = manager.reject(state.as_deref());
            return write_http_response(
                stream,
                ApiResponse::problem(
                    401,
                    "OIDC_PROVIDER_REJECTED",
                    format!("OIDC provider rejected authorization: {provider_error}"),
                    api_v1::next_request_id(),
                    None,
                )
                .with_header("Cache-Control", "no-store"),
            );
        }
        let code = query_value(&query, "code").ok().flatten();
        return match manager.complete(verifier, state.as_deref(), code.as_deref()) {
            Ok(completion) => {
                let mut response = ApiResponse::ok(serde_json::json!({
                    "authenticated": true,
                    "principal_id": completion.principal.id(),
                    "return_to": completion.return_to,
                }));
                response.status = 302;
                write_http_response(
                    stream,
                    response
                        .with_header("Location", completion.return_to)
                        .with_header(
                            "Set-Cookie",
                            oidc_session_cookie(&completion.session_id, completion.max_age_seconds),
                        )
                        .with_header("Cache-Control", "no-store"),
                )
            }
            Err(error) => write_http_response(stream, oidc_web_problem(error)),
        };
    }

    if path == "/api/v1/auth/desktop/exchange" {
        let Some(session_manager) = &context.desktop_session else {
            return write_http_response(
                stream,
                ApiResponse::problem(
                    404,
                    "DESKTOP_SESSION_DISABLED",
                    "desktop bootstrap is not enabled on this daemon",
                    "req-desktop-disabled",
                    None,
                ),
            );
        };
        if !stream
            .peer_addr()
            .map(|address| address.ip().is_loopback())
            .unwrap_or(false)
        {
            return write_http_response(
                stream,
                ApiResponse::problem(
                    403,
                    "DESKTOP_LOOPBACK_REQUIRED",
                    "desktop bootstrap is restricted to loopback clients",
                    "req-desktop-loopback",
                    None,
                ),
            );
        }
        let secret = request
            .headers
            .get(DESKTOP_BOOTSTRAP_HEADER)
            .map(String::as_str)
            .unwrap_or_default();
        return match session_manager.exchange(secret) {
            Ok(session) => write_http_response(
                stream,
                ApiResponse::ok(serde_json::json!({
                    "status": "ok",
                    "csrf_token": session.csrf_token,
                }))
                .with_header("Set-Cookie", session_cookie(&session.session_id)),
            ),
            Err(error) => write_http_response(
                stream,
                ApiResponse::problem(
                    401,
                    "DESKTOP_BOOTSTRAP_REJECTED",
                    error.to_string(),
                    "req-desktop-bootstrap",
                    None,
                ),
            ),
        };
    }

    // Browser session cookies are shared by host rather than loopback port.
    // A stale cookie from a previous random-port Desktop process must never
    // prevent index.html or hashed assets from loading; browser sessions gate
    // only the v1 API surface (and never the independent Agent protocol).
    let browser_session_path = api_v1::is_v1_path(&path) && !agent_api::is_agent_path(&path);
    if browser_session_path && let Some(session_manager) = &context.desktop_session {
        let mutation = matches!(request.method.as_str(), "POST" | "PUT" | "PATCH" | "DELETE");
        match session_manager.authorize(
            request.headers.get("cookie").map(String::as_str),
            request.headers.get(DESKTOP_CSRF_HEADER).map(String::as_str),
            mutation,
        ) {
            Ok(true) => {
                session_principal = Some(Principal::desktop_admin());
                if let Some(token) = context.internal_token.as_ref() {
                    request.headers.insert(
                        ORCHESTRATOR_INTERNAL_TOKEN_HEADER.to_string(),
                        token.clone(),
                    );
                }
            }
            Ok(false) => {}
            Err(error) => {
                return write_http_response(
                    stream,
                    ApiResponse::problem(
                        403,
                        "DESKTOP_SESSION_REJECTED",
                        error.to_string(),
                        "req-desktop-session",
                        None,
                    ),
                );
            }
        }
    }

    if browser_session_path && let Some(session_manager) = &context.oidc_web_session {
        let mutation = matches!(request.method.as_str(), "POST" | "PUT" | "PATCH" | "DELETE");
        match session_manager.authorize(
            request.headers.get("cookie").map(String::as_str),
            request.headers.get(OIDC_CSRF_HEADER).map(String::as_str),
            mutation,
        ) {
            Ok(Some(session)) => {
                session_principal = Some(session.principal);
                oidc_session_csrf = Some(session.csrf_token);
            }
            Ok(None) => {}
            Err(error) => return write_http_response(stream, oidc_web_problem(error)),
        }
    }

    if request.method == "GET" && path == "/api/v1/auth/session" {
        let request_id = api_v1::next_request_id();
        let data = match session_principal.as_ref() {
            Some(principal) => serde_json::json!({
                "authenticated": true,
                "principal_id": principal.id(),
                "role": principal.role().permission(),
                "mode": if context.desktop_session.is_some() { "desktop" } else { "oidc" },
                "csrf_token": oidc_session_csrf,
            }),
            None => serde_json::json!({"authenticated": false}),
        };
        return write_http_response(
            stream,
            api_v1::envelope(200, data, request_id).with_header("Cache-Control", "no-store"),
        );
    }

    if request.method == "POST" && path == "/api/v1/auth/logout" {
        let Some(manager) = &context.oidc_web_session else {
            return write_http_response(
                stream,
                ApiResponse::problem(
                    404,
                    "OIDC_WEB_LOGIN_DISABLED",
                    "OIDC browser login is not enabled on this daemon",
                    api_v1::next_request_id(),
                    None,
                ),
            );
        };
        return match manager.logout(
            request.headers.get("cookie").map(String::as_str),
            request.headers.get(OIDC_CSRF_HEADER).map(String::as_str),
        ) {
            Ok(()) => write_http_response(
                stream,
                api_v1::envelope(
                    200,
                    serde_json::json!({"authenticated": false}),
                    api_v1::next_request_id(),
                )
                .with_header("Set-Cookie", expired_session_cookie())
                .with_header("Cache-Control", "no-store"),
            ),
            Err(error) => write_http_response(stream, oidc_web_problem(error)),
        };
    }

    // 健康探针不能等待 console 全局锁。安装下载、driver 和慢 store 操作可能长时间
    // 持锁；若 /health 也排队，容器运行时会把一个仍在工作的 daemon 误判为失效。
    if request.method == "GET" && path == "/health" {
        return write_http_response(
            stream,
            legacy_compat_response(
                context,
                &path,
                ApiResponse::ok(serde_json::json!({
                    "service": "ojos-orchestrator-daemon",
                    "store": &context.store_kind,
                    "orchestrator_database_url": std::env::var("ORCHESTRATOR_DATABASE_URL").is_ok(),
                    "warnings": &context.startup_warnings,
                })),
            ),
        );
    }

    if api_v1::is_lock_free_path(&request.method, &path) {
        return write_http_response(
            stream,
            api_v1::lock_free_response(
                &path,
                context.durable_store.as_ref(),
                &context.startup_warnings,
                &context.build_identity,
            ),
        );
    }

    if agent_api::is_agent_path(&path) {
        let peer_identity = if stream.is_tls() {
            match stream.node_peer_identity() {
                Ok(identity) => identity,
                Err(error) => {
                    return write_agent_protocol_response(
                        stream,
                        ApiResponse::problem(
                            401,
                            "AGENT_CERTIFICATE_INVALID",
                            error.to_string(),
                            "req-agent-certificate",
                            None,
                        ),
                    );
                }
            }
        } else {
            None
        };
        let caller = if stream.is_tls() {
            peer_identity
                .as_ref()
                .map(agent_api::AgentCaller::Mtls)
                .unwrap_or(agent_api::AgentCaller::AnonymousTls)
        } else {
            let is_loopback = stream
                .peer_addr()
                .map(|address| address.ip().is_loopback())
                .unwrap_or(false);
            let bootstrap_matches = context
                .desktop_agent_secret
                .as_deref()
                .is_some_and(|secret| {
                    request
                        .headers
                        .get(DESKTOP_AGENT_BOOTSTRAP_HEADER)
                        .map(String::as_str)
                        == Some(secret)
                });
            if !is_loopback || !bootstrap_matches {
                return write_agent_protocol_response(
                    stream,
                    ApiResponse::problem(
                        401,
                        "AGENT_LOCAL_BOOTSTRAP_REJECTED",
                        "plaintext Agent requests require the Desktop loopback bootstrap identity",
                        "req-agent-local-bootstrap",
                        None,
                    ),
                );
            }
            agent_api::AgentCaller::LocalBootstrap {
                node_id: "desktop-local",
            }
        };
        return write_agent_protocol_response(
            stream,
            agent_api::route_authenticated(
                agent_api::AgentRouteContext {
                    storage: context.durable_store.as_ref(),
                    jobs: context.job_store.as_ref(),
                    artifact_store: Some(&context.artifact_store),
                    identity_service: context.node_identity.as_deref(),
                    workload_token_issuer: context.workload_token_issuer.as_deref(),
                    topology_provider: context.topology_provider.as_ref(),
                },
                caller,
                request,
            ),
        );
    }

    if api_v1::is_v1_path(&path) {
        let principal = match resolve_principal(
            &request,
            session_principal.as_ref(),
            context.internal_token.as_deref(),
            context.ephemeral_dev,
            context.oidc_verifier.as_ref().map(|verifier| verifier as _),
        ) {
            Ok(principal) => principal,
            Err(error) => {
                return write_http_response(
                    stream,
                    ApiResponse::problem(
                        401,
                        "PRINCIPAL_VERIFICATION_FAILED",
                        error.to_string(),
                        "req-principal-verification",
                        None,
                    ),
                );
            }
        };
        let response = match context.with_console(|console| {
            api_v1::handle(
                console,
                context.durable_store.as_ref(),
                context.topology_provider.as_ref(),
                context.catalog_registry.as_ref(),
                Some(&context.artifact_store),
                &context.store_state,
                &context.repo_root,
                request,
                context.internal_token.as_deref(),
                principal.as_ref(),
            )
        }) {
            Ok(response) => response,
            Err(()) => ApiResponse::problem(
                500,
                "CONTROL_PLANE_UNAVAILABLE",
                "orchestrator console coordinator is unavailable",
                "req-unavailable",
                None,
            ),
        };
        return write_http_response(stream, response);
    }

    // 画布布局持久化（无需 console；与控制面一样受内部令牌门禁约束）。
    if path == "/ui/layout" {
        let principal = match resolve_principal(
            &request,
            session_principal.as_ref(),
            context.internal_token.as_deref(),
            context.ephemeral_dev,
            context.oidc_verifier.as_ref().map(|verifier| verifier as _),
        ) {
            Ok(Some(principal)) => principal,
            Ok(None) => {
                return write_http_response(
                    stream,
                    ApiResponse::problem(
                        401,
                        "UNAUTHORIZED",
                        "a verified principal is required for UI state",
                        "req-ui-layout-auth",
                        None,
                    ),
                );
            }
            Err(error) => {
                return write_http_response(
                    stream,
                    ApiResponse::problem(
                        401,
                        "PRINCIPAL_VERIFICATION_FAILED",
                        error.to_string(),
                        "req-ui-layout-auth",
                        None,
                    ),
                );
            }
        };
        let user_id = principal.id();
        let topology_id =
            query_value(&query, "topology_id")?.unwrap_or_else(|| "primary".to_string());
        let result = match (request.method.as_str(), context.durable_store.as_ref()) {
            ("GET", Some(store)) => ui_layout::get_durable_layout(
                store,
                store.as_sqlite().map(|_| context.repo_root.as_path()),
                user_id,
                &topology_id,
            ),
            ("PUT" | "POST", Some(store)) => {
                ui_layout::put_durable_layout(store, user_id, &topology_id, &request.body)
            }
            ("GET", None) => ui_layout::get_layout(&context.repo_root),
            ("PUT" | "POST", None) => ui_layout::put_layout(&context.repo_root, &request.body),
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
            context.internal_token.as_deref(),
        ) {
            return write_http_response(
                stream,
                legacy_compat_response(context, &path, ApiResponse::error(401, err.to_string())),
            );
        }
        // 网络请求不占用 console 全局锁：GitHub Release 查询与索引拉取先在锁外完成。
        if request.method == "GET" && path == "/store/github/releases" {
            let response = market_api::github_releases_response(&query)
                .unwrap_or_else(|err| ApiResponse::error(status_for_error(&err), err.to_string()));
            return write_http_response(stream, legacy_compat_response(context, &path, response));
        }
        if request.method == "GET" && path == "/store/index" {
            let refresh = query_bool(&query, "refresh")?;
            let payload =
                market_api::store_index_payload(&context.store_state, &context.repo_root, refresh);
            let response = match payload {
                Ok((index_url, cached, index)) => {
                    let installed = context
                        .with_console(|console| market_api::installed_services(console))
                        .unwrap_or_else(|()| {
                            Err(anyhow!("orchestrator console coordinator is unavailable"))
                        });
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
            return write_http_response(stream, legacy_compat_response(context, &path, response));
        }
        let routed = context.with_console(|console| {
            market_api::route_store_request(
                &context.store_state,
                console,
                &context.repo_root,
                &request,
                &path,
                &query,
            )
        });
        let response = match routed {
            Err(()) => ApiResponse::error(500, "orchestrator console coordinator is unavailable"),
            Ok(Some(Ok(response))) => response,
            Ok(Some(Err(err))) => ApiResponse::error(status_for_error(&err), err.to_string()),
            Ok(None) => ApiResponse::error(
                404,
                format!("unsupported store route {} {}", request.method, path),
            ),
        };
        return write_http_response(stream, legacy_compat_response(context, &path, response));
    }

    // 既有控制面 API。
    if is_api_path(&path) {
        let response = context
            .with_console(|console| {
                handle_api_request_with_internal_token(
                    console,
                    request,
                    context.internal_token.as_deref(),
                )
            })
            .unwrap_or_else(|()| {
                ApiResponse::error(500, "orchestrator console coordinator is unavailable")
            });
        return write_http_response(stream, legacy_compat_response(context, &path, response));
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
    let response = context
        .with_console(|console| {
            handle_api_request_with_internal_token(
                console,
                request,
                context.internal_token.as_deref(),
            )
        })
        .unwrap_or_else(|()| {
            ApiResponse::error(500, "orchestrator console coordinator is unavailable")
        });
    write_http_response(stream, response)
}

fn render_job_metrics(jobs: Option<&JobMetricsSnapshot>, store_configured: bool) -> String {
    let statuses = [
        (
            "QUEUED",
            jobs.map(|snapshot| snapshot.queued).unwrap_or_default(),
        ),
        (
            "LEASED",
            jobs.map(|snapshot| snapshot.leased).unwrap_or_default(),
        ),
        (
            "RETRY_WAIT",
            jobs.map(|snapshot| snapshot.retry_wait).unwrap_or_default(),
        ),
        (
            "CANCEL_REQUESTED",
            jobs.map(|snapshot| snapshot.cancel_requested)
                .unwrap_or_default(),
        ),
        (
            "SUCCEEDED",
            jobs.map(|snapshot| snapshot.succeeded).unwrap_or_default(),
        ),
        (
            "FAILED",
            jobs.map(|snapshot| snapshot.failed).unwrap_or_default(),
        ),
        (
            "CANCELLED",
            jobs.map(|snapshot| snapshot.cancelled).unwrap_or_default(),
        ),
        (
            "NEEDS_ATTENTION",
            jobs.map(|snapshot| snapshot.needs_attention)
                .unwrap_or_default(),
        ),
    ];
    let mut output = String::from(
        "# HELP ojos_orchestrator_jobs Durable Jobs by current state.\n\
         # TYPE ojos_orchestrator_jobs gauge\n",
    );
    for (label, count) in statuses {
        output.push_str(&format!(
            "ojos_orchestrator_jobs{{status=\"{label}\"}} {count}\n"
        ));
    }
    let collection_error = usize::from(store_configured && jobs.is_none());
    output.push_str(
        "# HELP ojos_orchestrator_job_metrics_collection_error Whether the durable Job snapshot could be read.\n\
         # TYPE ojos_orchestrator_job_metrics_collection_error gauge\n",
    );
    output.push_str(&format!(
        "ojos_orchestrator_job_metrics_collection_error {collection_error}\n"
    ));

    let expired_leases = jobs
        .map(|snapshot| snapshot.expired_leases)
        .unwrap_or_default();
    output.push_str(
        "# HELP ojos_orchestrator_expired_job_leases Durable Jobs whose lease deadline has passed.\n\
         # TYPE ojos_orchestrator_expired_job_leases gauge\n",
    );
    output.push_str(&format!(
        "ojos_orchestrator_expired_job_leases {expired_leases}\n"
    ));

    let oldest_leased_heartbeat_age_seconds = jobs
        .map(|snapshot| snapshot.oldest_leased_heartbeat_age_seconds)
        .unwrap_or_default();
    output.push_str(
        "# HELP ojos_orchestrator_oldest_leased_job_heartbeat_age_seconds Seconds since the oldest leased Job heartbeat or state change.\n\
         # TYPE ojos_orchestrator_oldest_leased_job_heartbeat_age_seconds gauge\n",
    );
    output.push_str(&format!(
        "ojos_orchestrator_oldest_leased_job_heartbeat_age_seconds {oldest_leased_heartbeat_age_seconds}\n"
    ));
    output
}

fn write_static_response(
    stream: &mut ConnectionStream,
    response: static_site::StaticResponse,
) -> Result<()> {
    observability::record_status(response.status);
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
    use std::fs;
    use std::io::Read;

    fn repo_root() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(|path| path.parent())
            .and_then(|path| path.parent())
            .expect("workspace root")
            .to_path_buf()
    }

    fn request(addr: SocketAddr, path: &str, token: Option<&str>) -> String {
        let mut stream = TcpStream::connect(addr).expect("connect embedded server");
        stream
            .set_read_timeout(Some(Duration::from_secs(3)))
            .expect("read timeout");
        let token_header = token
            .map(|token| format!("{ORCHESTRATOR_INTERNAL_TOKEN_HEADER}: {token}\r\n"))
            .unwrap_or_default();
        write!(
            stream,
            "GET {path} HTTP/1.1\r\nHost: {addr}\r\n{token_header}Connection: close\r\n\r\n"
        )
        .expect("write request");
        let mut response = String::new();
        stream.read_to_string(&mut response).expect("read response");
        response
    }

    fn raw_request(
        addr: SocketAddr,
        method: &str,
        path: &str,
        headers: &[(&str, &str)],
        body: &str,
    ) -> String {
        let mut stream = TcpStream::connect(addr).expect("connect embedded server");
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .expect("read timeout");
        write!(stream, "{method} {path} HTTP/1.1\r\nHost: {addr}\r\n").expect("write request line");
        for (name, value) in headers {
            write!(stream, "{name}: {value}\r\n").expect("write header");
        }
        write!(
            stream,
            "Content-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body
        )
        .expect("write body");
        let mut response = String::new();
        stream.read_to_string(&mut response).expect("read response");
        response
    }

    fn response_header<'a>(response: &'a str, expected_name: &str) -> Option<&'a str> {
        response
            .split("\r\n\r\n")
            .next()
            .unwrap_or_default()
            .lines()
            .filter_map(|line| line.split_once(':'))
            .find(|(name, _)| name.eq_ignore_ascii_case(expected_name))
            .map(|(_, value)| value.trim())
    }

    fn response_body(response: &str) -> serde_json::Value {
        serde_json::from_str(
            response
                .split_once("\r\n\r\n")
                .map(|(_, body)| body)
                .expect("HTTP body separator"),
        )
        .expect("JSON response body")
    }

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
        let configured = max_workers(false);
        env.set("ORCHESTRATOR_MAX_WORKERS", "0");
        let zero = max_workers(false);
        env.set("ORCHESTRATOR_MAX_WORKERS", "not-a-number");
        let invalid = max_workers(false);
        assert_eq!(configured, 8);
        assert_eq!(zero, DEFAULT_MAX_WORKERS);
        assert_eq!(invalid, DEFAULT_MAX_WORKERS);

        env.set("ORCHESTRATOR_MAX_WORKERS", "127");
        assert_eq!(max_workers(true), DEFAULT_PRODUCTION_MAX_WORKERS);
        env.set("ORCHESTRATOR_MAX_WORKERS", "128");
        assert_eq!(max_workers(true), 128);
    }

    #[test]
    fn blank_topology_provider_origins_are_unconfigured_but_one_sided_values_fail_closed() {
        let mut env = TestEnv::lock();
        env.set("ORCHESTRATOR_GATEWAY_ADMIN_ORIGIN", "  \t");
        env.set("ORCHESTRATOR_AUTH_ADMIN_ORIGIN", "\r\n");
        assert!(configured_topology_provider().unwrap().is_none());

        env.set(
            "ORCHESTRATOR_GATEWAY_ADMIN_ORIGIN",
            "https://gateway.example.test",
        );
        let missing_auth = match configured_topology_provider() {
            Err(error) => error,
            Ok(_) => panic!("one-sided Gateway origin must fail closed"),
        };
        assert!(
            missing_auth
                .to_string()
                .contains("must be configured together")
        );

        env.set("ORCHESTRATOR_GATEWAY_ADMIN_ORIGIN", " ");
        env.set(
            "ORCHESTRATOR_AUTH_ADMIN_ORIGIN",
            "https://auth.example.test",
        );
        let missing_gateway = match configured_topology_provider() {
            Err(error) => error,
            Ok(_) => panic!("one-sided Auth origin must fail closed"),
        };
        assert!(
            missing_gateway
                .to_string()
                .contains("must be configured together")
        );
    }

    #[test]
    fn desktop_startup_does_not_fabricate_agent_observation_or_overwrite_lifecycle_state() {
        let mut env = TestEnv::lock();
        // Compose development overrides commonly inherit these as empty
        // strings. A blank pair is equivalent to an unconfigured Desktop
        // catalog and must not prevent the local trust bootstrap path.
        env.set("ORCHESTRATOR_CATALOG_TRUST_KEYS", " \t ");
        env.set("ORCHESTRATOR_CATALOG_SOURCES", "\r\n");
        for name in [
            "ORCHESTRATOR_GATEWAY_ADMIN_ORIGIN",
            "ORCHESTRATOR_AUTH_ADMIN_ORIGIN",
        ] {
            env.remove(name);
        }
        env.set("ORCHESTRATOR_MAX_WORKERS", "2");
        let root = repo_root();
        let directory = tempfile::tempdir().unwrap();
        let web = tempfile::tempdir().unwrap();
        fs::write(web.path().join("index.html"), "desktop").unwrap();
        let database_path = directory.path().join("orchestrator.db");
        let options = || EmbeddedServerOptions {
            repo_root: root.clone(),
            web_root: web.path().to_path_buf(),
            artifact_root: directory.path().join("artifacts"),
            bind_addr: "127.0.0.1:0".parse().unwrap(),
            internal_token: None,
            desktop_bootstrap_secret: Some("desktop-web-bootstrap".into()),
            desktop_agent_secret: Some("desktop-agent-bootstrap".into()),
            storage: EmbeddedStorage::Sqlite {
                database_path: database_path.clone(),
            },
        };

        let first = start_embedded_server(options()).unwrap();
        let exchange = raw_request(
            first.local_addr(),
            "POST",
            "/api/v1/auth/desktop/exchange",
            &[
                ("Content-Type", "application/json"),
                (DESKTOP_BOOTSTRAP_HEADER, "desktop-web-bootstrap"),
            ],
            "{}",
        );
        assert!(exchange.starts_with("HTTP/1.1 200"), "{exchange}");
        let cookie = response_header(&exchange, "Set-Cookie")
            .unwrap()
            .split(';')
            .next()
            .unwrap()
            .to_string();
        let capabilities = raw_request(
            first.local_addr(),
            "GET",
            "/api/v1/capabilities",
            &[("Cookie", &cookie)],
            "",
        );
        assert!(capabilities.starts_with("HTTP/1.1 200"), "{capabilities}");
        let readiness = raw_request(first.local_addr(), "GET", "/api/v1/healthz/ready", &[], "");
        assert!(readiness.starts_with("HTTP/1.1 200"), "{readiness}");
        let readiness = response_body(&readiness);
        let build = &readiness["data"]["build"];
        assert_eq!(build["version"], env!("CARGO_PKG_VERSION"));
        assert_eq!(build["profile"], "desktop");
        assert_eq!(build["target"], env!("OJOS_BUILD_TARGET"));
        let commit = build["commit_sha"].as_str().expect("build commit");
        assert!(
            commit == "development"
                || (commit.len() == 40
                    && commit
                        .bytes()
                        .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())),
            "unexpected canonical build commit: {commit}"
        );
        let bootstrap_is_not_an_internal_token = raw_request(
            first.local_addr(),
            "GET",
            "/api/v1/capabilities",
            &[(ORCHESTRATOR_INTERNAL_TOKEN_HEADER, "desktop-web-bootstrap")],
            "",
        );
        assert!(
            bootstrap_is_not_an_internal_token.starts_with("HTTP/1.1 401"),
            "{bootstrap_is_not_an_internal_token}"
        );
        let web_secret_is_not_an_agent_credential = raw_request(
            first.local_addr(),
            "POST",
            "/api/v1/agent/enroll",
            &[
                ("Content-Type", "application/json"),
                (DESKTOP_AGENT_BOOTSTRAP_HEADER, "desktop-web-bootstrap"),
            ],
            "{}",
        );
        assert!(
            web_secret_is_not_an_agent_credential.starts_with("HTTP/1.1 401"),
            "{web_secret_is_not_an_agent_credential}"
        );
        let agent_secret_reaches_the_authenticated_agent_protocol = raw_request(
            first.local_addr(),
            "POST",
            "/api/v1/agent/enroll",
            &[
                ("Content-Type", "application/json"),
                (DESKTOP_AGENT_BOOTSTRAP_HEADER, "desktop-agent-bootstrap"),
            ],
            "{}",
        );
        assert!(
            agent_secret_reaches_the_authenticated_agent_protocol.starts_with("HTTP/1.1 403"),
            "{agent_secret_reaches_the_authenticated_agent_protocol}"
        );
        assert!(
            agent_secret_reaches_the_authenticated_agent_protocol
                .contains("AGENT_ENROLLMENT_TLS_REQUIRED")
        );
        let agent_missing_content_type = raw_request(
            first.local_addr(),
            "POST",
            "/api/v1/agent/enroll",
            &[(DESKTOP_AGENT_BOOTSTRAP_HEADER, "desktop-agent-bootstrap")],
            "{}",
        );
        assert!(
            agent_missing_content_type.starts_with("HTTP/1.1 415"),
            "{agent_missing_content_type}"
        );
        assert!(
            agent_missing_content_type
                .contains("Content-Type: application/problem+json; charset=utf-8")
        );
        let agent_missing_content_type = response_body(&agent_missing_content_type);
        assert_eq!(agent_missing_content_type["status"], 415);
        assert_eq!(
            agent_missing_content_type["code"],
            "AGENT_CONTENT_TYPE_REQUIRED"
        );
        let body = response_body(&capabilities);
        let actions = body["data"]["actions"].as_array().expect("actions");
        let publishes = |expected: &str| {
            actions
                .iter()
                .any(|action| action["action"].as_str() == Some(expected))
        };
        assert!(publishes("catalog.list"));
        assert!(publishes("catalog.register"));
        assert!(!publishes("catalog.remove"));
        assert!(!publishes("catalog.search"));
        assert!(!publishes("release.install"));
        first.shutdown().unwrap();
        first.join().unwrap();
        let mut store = SqliteOrchestratorStore::open(&database_path).unwrap();
        let mut node = store.get_node("desktop-local").unwrap().unwrap();
        assert_eq!(node.updated_at, "unix-ms:0");
        node.status = "DRAINING".into();
        node.updated_at = "unix-ms:123".into();
        store.upsert_node(node).unwrap();
        drop(store);

        let restarted = start_embedded_server(options()).unwrap();
        restarted.shutdown().unwrap();
        restarted.join().unwrap();
        let store = SqliteOrchestratorStore::open(&database_path).unwrap();
        let node = store.get_node("desktop-local").unwrap().unwrap();
        assert_eq!(node.status, "DRAINING");
        assert_eq!(node.updated_at, "unix-ms:123");
    }

    #[test]
    fn metrics_fail_closed_when_the_bounded_job_projection_is_incomplete() {
        let mut env = TestEnv::lock();
        env.set("ORCHESTRATOR_MAX_WORKERS", "2");
        for name in [
            "ORCHESTRATOR_CATALOG_TRUST_KEYS",
            "ORCHESTRATOR_CATALOG_SOURCES",
            "ORCHESTRATOR_GATEWAY_ADMIN_ORIGIN",
            "ORCHESTRATOR_AUTH_ADMIN_ORIGIN",
        ] {
            env.remove(name);
        }
        let root = repo_root();
        let directory = tempfile::tempdir().unwrap();
        let web = tempfile::tempdir().unwrap();
        fs::write(web.path().join("index.html"), "desktop").unwrap();
        let database_path = directory.path().join("orchestrator.db");
        let server = start_embedded_server(EmbeddedServerOptions {
            repo_root: root,
            web_root: web.path().to_path_buf(),
            artifact_root: directory.path().join("artifacts"),
            bind_addr: "127.0.0.1:0".parse().unwrap(),
            internal_token: None,
            desktop_bootstrap_secret: None,
            desktop_agent_secret: None,
            storage: EmbeddedStorage::Sqlite {
                database_path: database_path.clone(),
            },
        })
        .unwrap();
        let healthy = raw_request(server.local_addr(), "GET", "/metrics", &[], "");
        assert!(healthy.starts_with("HTTP/1.1 200"), "{healthy}");

        rusqlite::Connection::open(&database_path)
            .unwrap()
            .execute(
                "DELETE FROM orchestrator_job_status_counts WHERE status = 'FAILED'",
                [],
            )
            .unwrap();
        let failed = raw_request(server.local_addr(), "GET", "/metrics", &[], "");
        assert!(failed.starts_with("HTTP/1.1 503"), "{failed}");
        assert!(
            failed.contains("ojos_orchestrator_job_metrics_collection_error 1"),
            "{failed}"
        );
    }

    #[test]
    fn tls_listener_extracts_verified_spiffe_identity_from_client_certificate() {
        use rcgen::{
            BasicConstraints, CertificateParams, DistinguishedName, DnType,
            ExtendedKeyUsagePurpose, IsCa, Issuer, KeyPair, KeyUsagePurpose,
        };
        use rustls::pki_types::ServerName;
        use std::io::{BufReader, Cursor};

        let mut ca_params = CertificateParams::new(Vec::<String>::new()).unwrap();
        ca_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        ca_params.key_usages = vec![
            KeyUsagePurpose::DigitalSignature,
            KeyUsagePurpose::KeyCertSign,
            KeyUsagePurpose::CrlSign,
        ];
        ca_params
            .distinguished_name
            .push(DnType::CommonName, "OJOS TLS test CA");
        let ca_key = KeyPair::generate().unwrap();
        let ca_certificate = ca_params.self_signed(&ca_key).unwrap();
        let issuer = Issuer::from_params(&ca_params, &ca_key);
        let mut server_params = CertificateParams::new(vec!["localhost".to_string()]).unwrap();
        server_params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
        let server_key = KeyPair::generate().unwrap();
        let server_certificate = server_params.signed_by(&server_key, &issuer).unwrap();
        let node_key = KeyPair::generate().unwrap();
        let mut csr_params = CertificateParams::default();
        csr_params.distinguished_name = DistinguishedName::new();
        let csr = csr_params
            .serialize_request(&node_key)
            .unwrap()
            .pem()
            .unwrap();
        let service = NodeIdentityService::from_pem(
            server_certificate.pem().as_bytes(),
            server_key.serialize_pem().as_bytes(),
            &ca_certificate.pem(),
            &ca_key.serialize_pem(),
        )
        .unwrap();
        let issued = service.issue("node-1", &csr, current_time_ms()).unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server_config = service.server_config();
        let server = std::thread::spawn(move || {
            let (tcp, _) = listener.accept().unwrap();
            let mut stream = ConnectionStream::accept(tcp, Some(server_config)).unwrap();
            let request = read_http_request(&mut stream).unwrap();
            assert_eq!(request.path, "/api/v1/agent/nodes/node-1/jobs:claim");
            stream.node_peer_identity().unwrap().unwrap()
        });

        let ca_der = rustls_pemfile::certs(&mut BufReader::new(Cursor::new(
            ca_certificate.pem().into_bytes(),
        )))
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
        let mut roots = rustls::RootCertStore::empty();
        for certificate in ca_der {
            roots.add(certificate).unwrap();
        }
        let client_certificate = rustls_pemfile::certs(&mut BufReader::new(Cursor::new(
            issued.certificate_pem.as_bytes(),
        )))
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
        let client_key = rustls_pemfile::private_key(&mut BufReader::new(Cursor::new(
            node_key.serialize_pem().into_bytes(),
        )))
        .unwrap()
        .unwrap();
        let client_config = rustls::ClientConfig::builder()
            .with_root_certificates(roots)
            .with_client_auth_cert(client_certificate, client_key)
            .unwrap();
        let connection = rustls::ClientConnection::new(
            Arc::new(client_config),
            ServerName::try_from("localhost").unwrap().to_owned(),
        )
        .unwrap();
        let tcp = TcpStream::connect(address).unwrap();
        let mut client = rustls::StreamOwned::new(connection, tcp);
        client
            .write_all(b"POST /api/v1/agent/nodes/node-1/jobs:claim HTTP/1.1\r\nHost: localhost\r\nContent-Length: 0\r\n\r\n")
            .unwrap();
        client.flush().unwrap();
        let peer = server.join().unwrap();
        assert_eq!(peer.node_id, "node-1");
        assert_eq!(peer.spiffe_id, "spiffe://ojos.local/node/node-1");
        assert_eq!(peer.serial_hex, issued.serial_hex);
    }

    #[test]
    fn postgres_mode_refuses_to_bind_without_required_production_identity() {
        let mut env = TestEnv::lock();
        env.remove("ORCHESTRATOR_OIDC_ISSUER");
        env.remove("ORCHESTRATOR_OIDC_AUDIENCE");
        let root = repo_root();
        let console = OrchestratorActionConsole::load_with_database_url(root.clone(), None)
            .expect("test console");
        let web = tempfile::tempdir().expect("web root");
        let result = start_embedded_server_with_console(
            EmbeddedServerOptions {
                repo_root: root,
                web_root: web.path().to_path_buf(),
                artifact_root: web.path().join("artifacts"),
                bind_addr: "127.0.0.1:0".parse().expect("loopback address"),
                internal_token: None,
                desktop_bootstrap_secret: None,
                desktop_agent_secret: None,
                storage: EmbeddedStorage::Postgres {
                    database_url: "postgresql://unused-in-this-test".to_string(),
                },
            },
            console,
        );
        let error = match result {
            Err(error) => error,
            Ok(server) => {
                let _ = server.shutdown();
                panic!("PostgreSQL mode must fail before binding without production identity")
            }
        };
        let expected = if env!("OJOS_BUILD_COMMIT") == "development" {
            "requires a build injected with a 40-character Git commit"
        } else {
            "configure production OIDC verifier"
        };
        assert!(
            error.to_string().contains(expected),
            "unexpected startup error: {error}"
        );
    }

    #[test]
    fn non_loopback_listener_rejects_non_production_storage_before_binding() {
        let root = repo_root();
        let web = tempfile::tempdir().expect("web root");
        let remote = "0.0.0.0:0".parse().expect("wildcard address");
        for storage in [
            EmbeddedStorage::Ephemeral,
            EmbeddedStorage::Sqlite {
                database_path: web.path().join("desktop.db"),
            },
        ] {
            let console = OrchestratorActionConsole::load_with_database_url(root.clone(), None)
                .expect("test console");
            let result = start_embedded_server_with_console(
                EmbeddedServerOptions {
                    repo_root: root.clone(),
                    web_root: web.path().to_path_buf(),
                    artifact_root: web.path().join("artifacts"),
                    bind_addr: remote,
                    internal_token: None,
                    desktop_bootstrap_secret: None,
                    desktop_agent_secret: None,
                    storage,
                },
                console,
            );
            let error = match result {
                Err(error) => error,
                Ok(server) => {
                    let _ = server.shutdown();
                    panic!("non-production storage must never bind a non-loopback listener")
                }
            };
            assert!(
                error.to_string().contains(
                    "non-loopback control-plane listeners require PostgreSQL production mode"
                ),
                "unexpected startup error: {error}"
            );
        }
    }

    #[test]
    fn embedded_server_serves_web_authenticates_and_releases_its_port() {
        let mut env = TestEnv::lock();
        env.remove("ORCHESTRATOR_DATABASE_URL");
        env.set("ORCHESTRATOR_MAX_WORKERS", "2");
        let root = repo_root();
        let console = OrchestratorActionConsole::load_with_database_url(root.clone(), None)
            .expect("test console");
        let web = tempfile::tempdir().expect("web root");
        fs::write(web.path().join("index.html"), "embedded-web-sentinel")
            .expect("write web fixture");

        let server = start_embedded_server_with_console(
            EmbeddedServerOptions {
                repo_root: root,
                web_root: web.path().to_path_buf(),
                artifact_root: web.path().join("artifacts"),
                bind_addr: "127.0.0.1:0".parse().expect("loopback address"),
                internal_token: Some("desktop-session-token".to_string()),
                desktop_bootstrap_secret: None,
                desktop_agent_secret: None,
                storage: EmbeddedStorage::Ephemeral,
            },
            console,
        )
        .expect("start embedded server");
        let addr = server.local_addr();

        let page = request(addr, "/", None);
        assert!(page.starts_with("HTTP/1.1 200"), "{page}");
        assert!(page.contains("embedded-web-sentinel"));
        let health = request(addr, "/health", None);
        assert!(health.starts_with("HTTP/1.1 200"), "{health}");
        let unauthorized = request(addr, "/services", None);
        assert!(unauthorized.starts_with("HTTP/1.1 401"), "{unauthorized}");
        let authorized = request(addr, "/services", Some("desktop-session-token"));
        assert!(authorized.starts_with("HTTP/1.1 200"), "{authorized}");

        server.shutdown().expect("shutdown embedded server");
        server.join().expect("join embedded server");
        let rebound = TcpListener::bind(addr).expect("released listener can be rebound");
        drop(rebound);
    }

    #[test]
    fn desktop_bootstrap_is_single_use_and_session_requires_csrf() {
        let mut env = TestEnv::lock();
        env.remove("ORCHESTRATOR_DATABASE_URL");
        env.set("ORCHESTRATOR_MAX_WORKERS", "2");
        let root = repo_root();
        let console = OrchestratorActionConsole::load_with_database_url(root.clone(), None)
            .expect("test console");
        let web = tempfile::tempdir().expect("web root");
        fs::write(web.path().join("index.html"), "desktop-session-test").expect("write web");
        let server = start_embedded_server_with_console(
            EmbeddedServerOptions {
                repo_root: root,
                web_root: web.path().to_path_buf(),
                artifact_root: web.path().join("artifacts"),
                bind_addr: "127.0.0.1:0".parse().unwrap(),
                internal_token: None,
                desktop_bootstrap_secret: Some("one-time-bootstrap".to_string()),
                desktop_agent_secret: Some("agent-only-bootstrap".to_string()),
                storage: EmbeddedStorage::Ephemeral,
            },
            console,
        )
        .expect("start server");
        let addr = server.local_addr();

        let static_page_with_stale_cookie = raw_request(
            addr,
            "GET",
            "/",
            &[("Cookie", "ojos_session=left-by-an-older-random-port")],
            "",
        );
        assert!(
            static_page_with_stale_cookie.starts_with("HTTP/1.1 200"),
            "{static_page_with_stale_cookie}"
        );
        assert!(static_page_with_stale_cookie.contains("desktop-session-test"));

        let unauthorized = raw_request(addr, "GET", "/api/v1/capabilities", &[], "");
        assert!(unauthorized.starts_with("HTTP/1.1 401"), "{unauthorized}");
        assert!(unauthorized.contains("application/problem+json"));

        let exchange = raw_request(
            addr,
            "POST",
            "/api/v1/auth/desktop/exchange",
            &[
                ("Content-Type", "application/json"),
                (DESKTOP_BOOTSTRAP_HEADER, "one-time-bootstrap"),
            ],
            "{}",
        );
        assert!(exchange.starts_with("HTTP/1.1 200"), "{exchange}");
        let cookie = response_header(&exchange, "Set-Cookie")
            .unwrap()
            .split(';')
            .next()
            .unwrap()
            .to_string();
        let csrf = response_body(&exchange)["csrf_token"]
            .as_str()
            .unwrap()
            .to_string();
        assert!(!exchange.contains("one-time-bootstrap"));

        let replay = raw_request(
            addr,
            "POST",
            "/api/v1/auth/desktop/exchange",
            &[
                ("Content-Type", "application/json"),
                (DESKTOP_BOOTSTRAP_HEADER, "one-time-bootstrap"),
            ],
            "{}",
        );
        assert!(replay.starts_with("HTTP/1.1 401"), "{replay}");

        let authorized = raw_request(
            addr,
            "GET",
            "/api/v1/capabilities",
            &[("Cookie", &cookie)],
            "",
        );
        assert!(authorized.starts_with("HTTP/1.1 200"), "{authorized}");

        let missing_csrf = raw_request(
            addr,
            "POST",
            "/api/v1/topologies",
            &[
                ("Cookie", &cookie),
                ("Content-Type", "application/json"),
                ("Idempotency-Key", "desktop-create-1"),
            ],
            "{}",
        );
        assert!(missing_csrf.starts_with("HTTP/1.1 403"), "{missing_csrf}");
        let accepted_csrf = raw_request(
            addr,
            "POST",
            "/api/v1/topologies",
            &[
                ("Cookie", &cookie),
                (DESKTOP_CSRF_HEADER, &csrf),
                ("Content-Type", "application/json"),
                ("Idempotency-Key", "desktop-create-2"),
            ],
            "{}",
        );
        assert!(accepted_csrf.starts_with("HTTP/1.1 503"), "{accepted_csrf}");

        server.shutdown().unwrap();
        server.join().unwrap();
    }

    #[test]
    fn topology_revision_api_is_durable_and_rejects_stale_etags() {
        let mut env = TestEnv::lock();
        env.remove("ORCHESTRATOR_DATABASE_URL");
        env.set("ORCHESTRATOR_MAX_WORKERS", "2");
        let root = repo_root();
        let data = tempfile::tempdir().expect("data directory");
        let web = tempfile::tempdir().expect("web root");
        fs::write(web.path().join("index.html"), "topology-test").expect("write web");
        let database = data.path().join("orchestrator.db");
        let topology = |note: &str| {
            serde_json::json!({
                "api_version": "v1",
                "topology_id": "primary",
                "root_endpoint": "127.0.0.1:8080:gateway",
                "authority": {
                    "root_endpoint": "127.0.0.1:8080:gateway",
                    "exposure_policy": "root-only"
                },
                "endpoints": [
                    {
                        "endpoint": "127.0.0.1:8080:gateway",
                        "service_id": "gateway",
                        "protocol": "http",
                        "health_path": "/health",
                        "display_name": "Gateway",
                        "note": note,
                        "config": {}
                    },
                    {
                        "endpoint": "127.0.0.1:8083:problem-service",
                        "service_id": "problem-service",
                        "protocol": "http",
                        "health_path": "/health",
                        "display_name": "Problem",
                        "note": "",
                        "config": {}
                    }
                ],
                "links": [{
                    "source_endpoint": "127.0.0.1:8080:gateway",
                    "target_endpoint": "127.0.0.1:8083:problem-service",
                    "protocol": "http",
                    "auth_mode": "internal",
                    "scope": "api",
                    "enabled": false,
                    "config_ref": "",
                    "secret_ref": "",
                    "policy": {}
                }]
            })
            .to_string()
        };
        let options = || EmbeddedServerOptions {
            repo_root: root.clone(),
            web_root: web.path().to_path_buf(),
            artifact_root: data.path().join("artifacts"),
            bind_addr: "127.0.0.1:0".parse().unwrap(),
            internal_token: Some("internal-token".to_string()),
            desktop_bootstrap_secret: None,
            desktop_agent_secret: None,
            storage: EmbeddedStorage::Sqlite {
                database_path: database.clone(),
            },
        };
        let server = start_embedded_server(options()).expect("start sqlite server");
        let addr = server.local_addr();
        let create_body = topology("first");
        let create = raw_request(
            addr,
            "POST",
            "/api/v1/topologies",
            &[
                ("Content-Type", "application/json"),
                (ORCHESTRATOR_INTERNAL_TOKEN_HEADER, "internal-token"),
                ("Idempotency-Key", "topology-create-1"),
            ],
            &create_body,
        );
        assert!(create.starts_with("HTTP/1.1 201"), "{create}");
        let first_etag = response_header(&create, "ETag").unwrap().to_string();

        let replay = raw_request(
            addr,
            "POST",
            "/api/v1/topologies",
            &[
                ("Content-Type", "application/json"),
                (ORCHESTRATOR_INTERNAL_TOKEN_HEADER, "internal-token"),
                ("Idempotency-Key", "topology-create-1"),
            ],
            &create_body,
        );
        assert!(replay.starts_with("HTTP/1.1 201"), "{replay}");
        assert_eq!(
            response_header(&replay, "Idempotency-Replayed"),
            Some("true")
        );
        assert_eq!(response_header(&replay, "ETag"), Some(first_etag.as_str()));
        let conflicting_key = raw_request(
            addr,
            "POST",
            "/api/v1/topologies",
            &[
                ("Content-Type", "application/json"),
                (ORCHESTRATOR_INTERNAL_TOKEN_HEADER, "internal-token"),
                ("Idempotency-Key", "topology-create-1"),
            ],
            &topology("different-payload"),
        );
        assert!(
            conflicting_key.starts_with("HTTP/1.1 409"),
            "{conflicting_key}"
        );
        assert!(conflicting_key.contains("IDEMPOTENCY_CONFLICT"));

        let second_body = topology("second");
        let second = raw_request(
            addr,
            "POST",
            "/api/v1/topologies/primary/revisions",
            &[
                ("Content-Type", "application/json"),
                (ORCHESTRATOR_INTERNAL_TOKEN_HEADER, "internal-token"),
                ("Idempotency-Key", "topology-update-1"),
                ("If-Match", &first_etag),
            ],
            &second_body,
        );
        assert!(second.starts_with("HTTP/1.1 201"), "{second}");
        let second_etag = response_header(&second, "ETag").unwrap().to_string();
        assert_ne!(first_etag, second_etag);

        let stale = raw_request(
            addr,
            "POST",
            "/api/v1/topologies/primary/revisions",
            &[
                ("Content-Type", "application/json"),
                (ORCHESTRATOR_INTERNAL_TOKEN_HEADER, "internal-token"),
                ("Idempotency-Key", "topology-update-stale"),
                ("If-Match", &first_etag),
            ],
            &topology("stale"),
        );
        assert!(stale.starts_with("HTTP/1.1 409"), "{stale}");

        let apply = raw_request(
            addr,
            "POST",
            "/api/v1/topologies/primary:apply",
            &[
                ("Content-Type", "application/json"),
                (ORCHESTRATOR_INTERNAL_TOKEN_HEADER, "internal-token"),
                ("Idempotency-Key", "topology-apply-1"),
                ("If-Match", &second_etag),
            ],
            "{}",
        );
        assert!(apply.starts_with("HTTP/1.1 422"), "{apply}");
        assert!(apply.contains("TOPOLOGY_PROVIDER_MISSING"));

        server.shutdown().unwrap();
        server.join().unwrap();

        let restarted = start_embedded_server(options()).expect("restart sqlite server");
        let persisted = raw_request(
            restarted.local_addr(),
            "GET",
            "/api/v1/topologies/primary",
            &[(ORCHESTRATOR_INTERNAL_TOKEN_HEADER, "internal-token")],
            "",
        );
        assert!(persisted.starts_with("HTTP/1.1 200"), "{persisted}");
        assert_eq!(
            response_header(&persisted, "ETag"),
            Some(second_etag.as_str())
        );
        assert_eq!(
            response_body(&persisted)["data"]["heads"]["applied_revision_id"],
            serde_json::Value::Null
        );
        restarted.shutdown().unwrap();
        restarted.join().unwrap();
    }
}
