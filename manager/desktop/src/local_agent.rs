use anyhow::{Context, Result};
use orchestrator_agent::{
    AgentLedger, AgentWorker, BuiltInReleasePipelineProvider, JobExecutor, LoopbackHttpTransport,
    WorkerConfig,
};
use orchestrator_runtime::{DockerEngineRuntime, RuntimeError};
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;
use tokio::sync::watch;
use url::Url;

pub const DESKTOP_NODE_ID: &str = "desktop-local";
const DEFAULT_RETRY_DELAY: Duration = Duration::from_secs(2);

#[derive(Debug, Clone)]
pub struct DesktopAgentOptions {
    pub control_plane: Url,
    pub ledger_path: PathBuf,
    pub provider_state_path: PathBuf,
    pub registry_credentials_path: Option<PathBuf>,
    pub retry_delay: Duration,
    pub bootstrap_secret: String,
}

impl DesktopAgentOptions {
    pub fn embedded(control_plane: Url, data_dir: PathBuf, bootstrap_secret: String) -> Self {
        Self {
            control_plane,
            ledger_path: data_dir.join("agent-ledger.db"),
            provider_state_path: data_dir.join("provider-state.sqlite3"),
            registry_credentials_path: None,
            retry_delay: DEFAULT_RETRY_DELAY,
            bootstrap_secret,
        }
    }

    /// Configures the same strict, bounded registry credential document used
    /// by the standalone Agent. The document is read by the native runtime and
    /// is never exposed to the embedded Web UI.
    pub fn with_registry_credentials_file(mut self, path: PathBuf) -> Self {
        self.registry_credentials_path = Some(path);
        self
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DesktopAgentPhase {
    Starting,
    Running,
    Degraded,
    Stopping,
    Stopped,
    StopTimedOut,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesktopAgentStatus {
    pub phase: DesktopAgentPhase,
    pub detail: String,
    pub retry_count: u64,
}

impl DesktopAgentStatus {
    fn starting() -> Self {
        Self {
            phase: DesktopAgentPhase::Starting,
            detail: "local agent is starting".to_string(),
            retry_count: 0,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesktopAgentShutdown {
    pub graceful: bool,
    pub detail: String,
}

pub struct DesktopAgentHandle {
    shutdown: watch::Sender<bool>,
    completed: Receiver<std::result::Result<(), String>>,
    thread: Option<JoinHandle<()>>,
    status: Arc<Mutex<DesktopAgentStatus>>,
}

impl DesktopAgentHandle {
    pub fn status(&self) -> DesktopAgentStatus {
        self.status
            .lock()
            .map(|status| status.clone())
            .unwrap_or(DesktopAgentStatus {
                phase: DesktopAgentPhase::Degraded,
                detail: "local agent status lock is poisoned".to_string(),
                retry_count: 0,
            })
    }

    /// Requests a cooperative stop and waits at most `timeout`. A timed-out
    /// runtime call is detached so Desktop can continue shutting down the
    /// embedded backend and process.
    pub fn shutdown_and_join(mut self, timeout: Duration) -> DesktopAgentShutdown {
        update_status(
            &self.status,
            DesktopAgentPhase::Stopping,
            "waiting for local agent to drain".to_string(),
            None,
        );
        let _ = self.shutdown.send(true);
        match self.completed.recv_timeout(timeout) {
            Ok(result) => {
                let join_result = self
                    .thread
                    .take()
                    .expect("desktop agent thread is present")
                    .join();
                let (graceful, detail) = match (result, join_result) {
                    (Ok(()), Ok(())) => (true, "local agent stopped".to_string()),
                    (Err(error), Ok(())) => {
                        (false, format!("local agent stopped after error: {error}"))
                    }
                    (_, Err(_)) => (false, "local agent thread panicked".to_string()),
                };
                update_status(
                    &self.status,
                    DesktopAgentPhase::Stopped,
                    detail.clone(),
                    None,
                );
                DesktopAgentShutdown { graceful, detail }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                let detail = format!(
                    "local agent did not drain within {} ms; continuing Desktop shutdown",
                    timeout.as_millis()
                );
                update_status(
                    &self.status,
                    DesktopAgentPhase::StopTimedOut,
                    detail.clone(),
                    None,
                );
                // Dropping JoinHandle detaches the bounded-overrun worker. The
                // process is already exiting, and the embedded server is closed
                // only after this bounded drain attempt.
                self.thread.take();
                DesktopAgentShutdown {
                    graceful: false,
                    detail,
                }
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                let detail = "local agent completion channel disconnected".to_string();
                let join_result = self.thread.take().map(JoinHandle::join);
                update_status(
                    &self.status,
                    DesktopAgentPhase::Stopped,
                    detail.clone(),
                    None,
                );
                DesktopAgentShutdown {
                    graceful: join_result.is_some_and(|result| result.is_ok()),
                    detail,
                }
            }
        }
    }
}

impl Drop for DesktopAgentHandle {
    fn drop(&mut self) {
        let _ = self.shutdown.send(true);
    }
}

pub fn start_desktop_agent(options: DesktopAgentOptions) -> Result<DesktopAgentHandle> {
    let transport = LoopbackHttpTransport::new_with_bootstrap(
        options.control_plane.as_str(),
        options.bootstrap_secret.clone(),
    )
    .context("configure Desktop loopback agent transport")?;
    if let Some(path) = options.registry_credentials_path.as_deref() {
        DockerEngineRuntime::connect_local()?
            .with_registry_credentials_file(path)
            .context("validate Desktop Agent registry credentials")?;
    }
    spawn_agent_thread(move |shutdown, status| {
        run_agent_thread(options, transport, shutdown, status)
    })
}

fn spawn_agent_thread<F>(runner: F) -> Result<DesktopAgentHandle>
where
    F: FnOnce(
            watch::Receiver<bool>,
            Arc<Mutex<DesktopAgentStatus>>,
        ) -> std::result::Result<(), String>
        + Send
        + 'static,
{
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let (completed_tx, completed_rx) = mpsc::sync_channel(1);
    let status = Arc::new(Mutex::new(DesktopAgentStatus::starting()));
    let thread_status = Arc::clone(&status);
    let thread = thread::Builder::new()
        .name("orchestrator-desktop-agent".to_string())
        .spawn(move || {
            let result = runner(shutdown_rx, Arc::clone(&thread_status));
            match &result {
                Ok(()) => update_status(
                    &thread_status,
                    DesktopAgentPhase::Stopped,
                    "local agent stopped".to_string(),
                    None,
                ),
                Err(error) => update_status(
                    &thread_status,
                    DesktopAgentPhase::Degraded,
                    format!("local agent thread stopped after error: {error}"),
                    None,
                ),
            }
            let _ = completed_tx.send(result);
        })
        .context("spawn Desktop loopback agent thread")?;
    Ok(DesktopAgentHandle {
        shutdown: shutdown_tx,
        completed: completed_rx,
        thread: Some(thread),
        status,
    })
}

fn run_agent_thread(
    options: DesktopAgentOptions,
    transport: LoopbackHttpTransport,
    shutdown: watch::Receiver<bool>,
    status: Arc<Mutex<DesktopAgentStatus>>,
) -> std::result::Result<(), String> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .map_err(|error| format!("create Desktop agent Tokio runtime: {error}"))?;
    runtime.block_on(run_agent_loop(options, transport, shutdown, status))
}

async fn run_agent_loop(
    options: DesktopAgentOptions,
    transport: LoopbackHttpTransport,
    mut shutdown: watch::Receiver<bool>,
    status: Arc<Mutex<DesktopAgentStatus>>,
) -> std::result::Result<(), String> {
    let mut retry_count = 0_u64;
    loop {
        if *shutdown.borrow() {
            return Ok(());
        }
        let ledger = match AgentLedger::open(&options.ledger_path) {
            Ok(ledger) => ledger,
            Err(error) => {
                retry_count = retry_count.saturating_add(1);
                degraded(
                    &status,
                    retry_count,
                    format!("open local agent ledger: {error}"),
                    options.retry_delay,
                );
                if wait_or_shutdown(&mut shutdown, options.retry_delay).await {
                    return Ok(());
                }
                continue;
            }
        };

        // Desktop is a loopback deployment of the same Agent, not a reduced
        // runtime. Provider state therefore lives beside the execution ledger
        // and uses exactly the same typed provider implementation as a remote
        // Node. Missing provider configuration is rejected by the pipeline at
        // plan/execution time instead of being reported as a successful local
        // install.
        let pipeline_provider = match BuiltInReleasePipelineProvider::from_env_with_state_database(
            &options.provider_state_path,
        ) {
            Ok(provider) => provider,
            Err(error) => {
                retry_count = retry_count.saturating_add(1);
                degraded(
                    &status,
                    retry_count,
                    format!("configure local release providers: {error}"),
                    options.retry_delay,
                );
                if wait_or_shutdown(&mut shutdown, options.retry_delay).await {
                    return Ok(());
                }
                continue;
            }
        };

        let docker = match configured_docker_runtime(&options) {
            Ok(docker) => docker,
            Err(error) => {
                retry_count = retry_count.saturating_add(1);
                degraded(
                    &status,
                    retry_count,
                    format!("connect local Docker Engine: {error}"),
                    options.retry_delay,
                );
                if wait_or_shutdown(&mut shutdown, options.retry_delay).await {
                    return Ok(());
                }
                continue;
            }
        };
        if let Err(error) = docker.ping().await {
            retry_count = retry_count.saturating_add(1);
            degraded(
                &status,
                retry_count,
                format!("local Docker Engine is unavailable: {error}"),
                options.retry_delay,
            );
            if wait_or_shutdown(&mut shutdown, options.retry_delay).await {
                return Ok(());
            }
            continue;
        }

        let artifact_fetcher = transport
            .artifact_fetcher(DESKTOP_NODE_ID)
            .map_err(|error| format!("configure Desktop artifact fetcher: {error}"))?;

        let worker = AgentWorker::new(
            WorkerConfig {
                node_id: DESKTOP_NODE_ID.to_string(),
                instance_id: desktop_instance_id(),
                heartbeat_ms: 10_000,
                lease_ms: 30_000,
                transport_retry_ms: 1_000,
            },
            transport.clone(),
            JobExecutor::new(docker)
                .with_pipeline_provider(Arc::new(pipeline_provider))
                .with_artifact_fetcher(Arc::new(artifact_fetcher)),
            ledger,
        );
        let mut worker = match worker {
            Ok(worker) => worker,
            Err(error) => return Err(format!("configure Desktop agent worker: {error}")),
        };
        update_status(
            &status,
            DesktopAgentPhase::Running,
            "local agent is connected to the embedded control plane and Docker Engine".to_string(),
            Some(retry_count),
        );
        match worker.run_until_shutdown(shutdown.clone()).await {
            Ok(()) if *shutdown.borrow() => return Ok(()),
            Ok(()) => {
                retry_count = retry_count.saturating_add(1);
                degraded(
                    &status,
                    retry_count,
                    "local agent worker stopped unexpectedly".to_string(),
                    options.retry_delay,
                );
            }
            Err(error) => {
                retry_count = retry_count.saturating_add(1);
                degraded(
                    &status,
                    retry_count,
                    format!("local agent worker failed: {error}"),
                    options.retry_delay,
                );
            }
        }
        if wait_or_shutdown(&mut shutdown, options.retry_delay).await {
            return Ok(());
        }
    }
}

fn configured_docker_runtime(
    options: &DesktopAgentOptions,
) -> std::result::Result<DockerEngineRuntime, RuntimeError> {
    let runtime = DockerEngineRuntime::connect_local()?;
    match options.registry_credentials_path.as_deref() {
        Some(path) => runtime.with_registry_credentials_file(path),
        None => Ok(runtime),
    }
}

fn degraded(
    status: &Arc<Mutex<DesktopAgentStatus>>,
    retry_count: u64,
    error: String,
    retry_delay: Duration,
) {
    let detail = format!(
        "{error}; local agent is degraded and will retry in {} ms",
        retry_delay.as_millis()
    );
    eprintln!("OJOS Desktop agent degraded: {detail}");
    update_status(
        status,
        DesktopAgentPhase::Degraded,
        detail,
        Some(retry_count),
    );
}

fn update_status(
    status: &Arc<Mutex<DesktopAgentStatus>>,
    phase: DesktopAgentPhase,
    detail: String,
    retry_count: Option<u64>,
) {
    if let Ok(mut status) = status.lock() {
        status.phase = phase;
        status.detail = detail;
        if let Some(retry_count) = retry_count {
            status.retry_count = retry_count;
        }
    }
}

async fn wait_or_shutdown(shutdown: &mut watch::Receiver<bool>, delay: Duration) -> bool {
    if *shutdown.borrow() {
        return true;
    }
    tokio::select! {
        _ = tokio::time::sleep(delay) => false,
        changed = shutdown.changed() => changed.is_err() || *shutdown.borrow(),
    }
}

fn desktop_instance_id() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    format!("desktop-{}-{millis}", std::process::id())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Instant;

    #[test]
    fn embedded_agent_persists_execution_and_provider_state_beside_desktop_data() {
        let data = tempfile::tempdir().unwrap();
        let options = DesktopAgentOptions::embedded(
            Url::parse("http://127.0.0.1:38123/").unwrap(),
            data.path().to_path_buf(),
            "bootstrap".to_string(),
        );

        assert_eq!(options.ledger_path, data.path().join("agent-ledger.db"));
        assert_eq!(
            options.provider_state_path,
            data.path().join("provider-state.sqlite3")
        );
        assert_ne!(options.ledger_path, options.provider_state_path);
        assert!(options.registry_credentials_path.is_none());
    }

    #[test]
    fn embedded_agent_uses_the_runtime_strict_registry_credential_loader() {
        let data = tempfile::tempdir().unwrap();
        let credentials = data.path().join("registry-credentials.json");
        std::fs::write(
            &credentials,
            r#"{"schema_version":1,"registries":[{"server_address":"registry.example","username":"desktop","password":"do-not-log-this","unexpected":true}]}"#,
        )
        .unwrap();
        let options = DesktopAgentOptions::embedded(
            Url::parse("http://127.0.0.1:38123/").unwrap(),
            data.path().to_path_buf(),
            "bootstrap".to_string(),
        )
        .with_registry_credentials_file(credentials.clone());

        assert_eq!(
            options.registry_credentials_path.as_deref(),
            Some(credentials.as_path())
        );
        let error = match configured_docker_runtime(&options) {
            Ok(_) => panic!("unknown credential fields must be rejected"),
            Err(error) => error.to_string(),
        };
        assert!(error.contains("strict schema-version 1 JSON"));
        assert!(!error.contains("do-not-log-this"));
    }

    #[test]
    fn graceful_shutdown_notifies_and_joins_agent_thread() {
        let observed = Arc::new(AtomicBool::new(false));
        let thread_observed = Arc::clone(&observed);
        let handle = spawn_agent_thread(move |mut shutdown, status| {
            update_status(
                &status,
                DesktopAgentPhase::Running,
                "test agent running".to_string(),
                None,
            );
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            runtime.block_on(async move {
                while !*shutdown.borrow() {
                    if shutdown.changed().await.is_err() {
                        break;
                    }
                }
            });
            thread_observed.store(true, Ordering::Release);
            Ok(())
        })
        .unwrap();

        let result = handle.shutdown_and_join(Duration::from_secs(1));
        assert!(result.graceful, "{}", result.detail);
        assert!(observed.load(Ordering::Acquire));
    }

    #[test]
    fn shutdown_wait_is_bounded_when_runtime_call_does_not_drain() {
        let handle = spawn_agent_thread(move |_shutdown, status| {
            update_status(
                &status,
                DesktopAgentPhase::Running,
                "blocked runtime call".to_string(),
                None,
            );
            thread::sleep(Duration::from_millis(150));
            Ok(())
        })
        .unwrap();
        let started = Instant::now();
        let result = handle.shutdown_and_join(Duration::from_millis(10));
        assert!(!result.graceful);
        assert!(started.elapsed() < Duration::from_millis(100));
        assert!(result.detail.contains("did not drain"));
    }

    #[test]
    fn degraded_status_is_explicit_and_tracks_retries() {
        let handle = spawn_agent_thread(move |mut shutdown, status| {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            runtime.block_on(async move {
                degraded(
                    &status,
                    1,
                    "Docker is unavailable".to_string(),
                    Duration::from_millis(5),
                );
                let _ = shutdown.changed().await;
            });
            Ok(())
        })
        .unwrap();
        for _ in 0..20 {
            if handle.status().phase == DesktopAgentPhase::Degraded {
                break;
            }
            thread::sleep(Duration::from_millis(5));
        }
        let status = handle.status();
        assert_eq!(status.phase, DesktopAgentPhase::Degraded);
        assert_eq!(status.retry_count, 1);
        assert!(status.detail.contains("will retry"));
        assert!(handle.shutdown_and_join(Duration::from_secs(1)).graceful);
    }

    #[test]
    fn agent_thread_failure_is_not_reported_as_running_or_graceful() {
        let handle = spawn_agent_thread(move |_shutdown, _status| {
            Err("Tokio runtime initialization failed".to_string())
        })
        .unwrap();
        for _ in 0..20 {
            if handle.status().phase == DesktopAgentPhase::Degraded {
                break;
            }
            thread::sleep(Duration::from_millis(5));
        }
        let status = handle.status();
        assert_eq!(status.phase, DesktopAgentPhase::Degraded);
        assert!(status.detail.contains("initialization failed"));
        let shutdown = handle.shutdown_and_join(Duration::from_secs(1));
        assert!(!shutdown.graceful);
    }
}
