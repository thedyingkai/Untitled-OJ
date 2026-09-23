use anyhow::{Context, Result, anyhow};
use opentelemetry::propagation::TextMapPropagator;
use opentelemetry::trace::TraceContextExt;
use reqwest::{Client, Method, RequestBuilder, StatusCode};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::env;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tokio::fs;
use tokio::io::AsyncWriteExt;
use tokio::sync::Semaphore;
use tracing::{Instrument, error, info, info_span, warn};
use tracing_opentelemetry::OpenTelemetrySpanExt;
use uuid::Uuid;

use crate::cgroup::CgroupRun;
use crate::config::{LanguageConfig, LanguagesConfig};
use crate::health::HealthState;
use crate::judge::judge_artifacts;
use crate::result::ResultFile;
use crate::sandbox::nsjail_available;
use crate::service_context::ServiceContext;

#[derive(Debug)]
struct JudgeApiResponseError {
    status: StatusCode,
    body: String,
}

impl std::fmt::Display for JudgeApiResponseError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "judge-api returned {}: {}",
            self.status, self.body
        )
    }
}

impl std::error::Error for JudgeApiResponseError {}

#[derive(Debug)]
struct TaskReportRejected(String);

impl std::fmt::Display for TaskReportRejected {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for TaskReportRejected {}

#[derive(Debug, Clone)]
pub struct WorkerLinkConfig {
    pub worker_id: String,
    pub worker_name: String,
    pub judge_api_url: String,
    pub worker_token: String,
    pub max_concurrency: usize,
    pub work_dir: PathBuf,
    pub artifact_cache_dir: PathBuf,
    pub supported_languages: Vec<String>,
    pub heartbeat_interval: Duration,
    pub task_lease_ttl: Duration,
    pub redis_url: Option<String>,
    pub redis_task_stream: String,
    pub redis_consumer_group: String,
    pub internal_gateway_url: Option<String>,
    pub storage_api_get: String,
    pub storage_api_put: String,
    pub service_token: Option<String>,
    pub caller_node_id: Option<String>,
    pub runner_mode: String,
    pub service_context: Option<ServiceContext>,
}

impl WorkerLinkConfig {
    pub fn from_env(languages: &LanguagesConfig) -> Result<Self> {
        let service_context = ServiceContext::load_optional()?;
        validate_deployment_mode(
            service_context.is_some(),
            &std::env::var("OJOS_ENVIRONMENT").unwrap_or_default(),
        )?;
        if let Some(context) = service_context.as_ref() {
            context.require_service("judge-worker")?;
        }
        // In managed mode the authenticated Deployment identity is the Worker
        // identity. A release-provided environment variable must not be able to
        // register an arbitrary logical Worker under the same JWT.
        let worker_id = match service_context.as_ref() {
            Some(context) => context.deployment.id.clone(),
            None => env_or("OJOS_WORKER_ID", || {
                std::env::var("HOSTNAME").unwrap_or_else(|_| format!("worker-{}", Uuid::new_v4()))
            }),
        };
        let worker_name = env_or("OJOS_WORKER_NAME", || {
            service_context
                .as_ref()
                .map(|context| {
                    format!("{}@{}", context.deployment.service, context.deployment.node)
                })
                .unwrap_or_else(|| worker_id.clone())
        });
        let judge_api_url = match service_context.as_ref() {
            Some(context) => context.binding_url("judge_control", "")?,
            None => required_env("OJOS_JUDGE_API_URL")?
                .trim_end_matches('/')
                .to_string(),
        };
        let worker_token = match service_context.as_ref() {
            Some(_) => String::new(),
            None => required_env("OJOS_WORKER_TOKEN")?,
        };
        let max_concurrency = env_parse("OJOS_MAX_CONCURRENCY", 1usize)?;
        let work_dir = PathBuf::from(env_or("OJOS_WORK_DIR", || {
            "/tmp/ojos-worker/work".to_string()
        }));
        let artifact_cache_dir = PathBuf::from(env_or("OJOS_ARTIFACT_CACHE_DIR", || {
            "/tmp/ojos-worker/cache".to_string()
        }));
        let supported_languages = match std::env::var("OJOS_SUPPORTED_LANGUAGES") {
            Ok(raw) if !raw.trim().is_empty() => raw
                .split(',')
                .map(|item| item.trim().to_string())
                .filter(|item| !item.is_empty())
                .collect(),
            _ => languages.languages.keys().cloned().collect(),
        };
        let heartbeat_interval = Duration::from_secs(env_parse("OJOS_HEARTBEAT_INTERVAL", 10u64)?);
        let task_lease_ttl = Duration::from_secs(env_parse("OJOS_TASK_LEASE_TTL", 60u64)?);
        let redis_url = if service_context.is_some() {
            None
        } else {
            std::env::var("OJOS_REDIS_URL")
                .ok()
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty())
        };
        let redis_task_stream = env_or("OJOS_JUDGE_TASK_STREAM", || "ojos:judge:task".to_string());
        let redis_consumer_group =
            env_or("OJOS_JUDGE_CONSUMER_GROUP", || "judge-worker".to_string());
        let internal_gateway_url = service_context
            .as_ref()
            .map(|context| context.gateway.origin.trim_end_matches('/').to_string())
            .or_else(|| {
                std::env::var("OJOS_INTERNAL_GATEWAY_URL")
                    .ok()
                    .map(|value| value.trim().trim_end_matches('/').to_string())
                    .filter(|value| !value.is_empty())
            });
        let storage_api_get = match service_context.as_ref() {
            Some(context) => context.binding("storage_get")?.api_id.clone(),
            None => env_or("OJOS_STORAGE_OBJECT_GET_API_ID", || {
                "storage.object.get".to_string()
            }),
        };
        let storage_api_put = match service_context.as_ref() {
            Some(context) => context
                .bindings
                .get("storage_put")
                .map(|binding| binding.api_id.clone())
                .unwrap_or_else(|| "storage.object.put".to_string()),
            None => env_or("OJOS_STORAGE_OBJECT_PUT_API_ID", || {
                "storage.object.put".to_string()
            }),
        };
        let service_token = if service_context.is_some() {
            None
        } else {
            std::env::var("OJOS_SERVICE_TOKEN")
                .ok()
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty())
        };
        let caller_node_id = service_context
            .as_ref()
            .map(|context| context.deployment.node.clone())
            .or_else(|| {
                std::env::var("OJOS_CALLER_NODE_ID")
                    .ok()
                    .map(|value| value.trim().to_string())
                    .filter(|value| !value.is_empty())
            });
        let runner_mode =
            normalize_runner_mode(&env_or("OJOS_RUNNER_MODE", || "nsjail".to_string()))?;

        Ok(Self {
            worker_id,
            worker_name,
            judge_api_url,
            worker_token,
            max_concurrency: max_concurrency.max(1),
            work_dir,
            artifact_cache_dir,
            supported_languages,
            heartbeat_interval,
            task_lease_ttl,
            redis_url,
            redis_task_stream,
            redis_consumer_group,
            internal_gateway_url,
            storage_api_get,
            storage_api_put,
            service_token,
            caller_node_id,
            runner_mode,
            service_context,
        })
    }
}

fn validate_deployment_mode(managed: bool, environment: &str) -> Result<()> {
    if managed || environment.trim().eq_ignore_ascii_case("development") {
        return Ok(());
    }
    Err(anyhow!(
        "an unmanaged Judge Worker is a development-only compatibility path; production requires OJOS_SERVICE_CONTEXT_FILE, while legacy Compose must explicitly set OJOS_ENVIRONMENT=development"
    ))
}

pub async fn run_worker_link(
    languages: Arc<LanguagesConfig>,
    health: Arc<HealthState>,
) -> Result<()> {
    let config = Arc::new(WorkerLinkConfig::from_env(&languages)?);
    validate_runtime_preflight(&config, &languages).await?;
    health.mark_preflight_ok(config.heartbeat_interval);
    info!(
        runner_mode = %config.runner_mode,
        supported_languages = ?config.supported_languages,
        context_generation = config.service_context.as_ref().map(|context| context.generation),
        "worker runtime preflight passed"
    );
    if let Some(gateway_url) = &config.internal_gateway_url {
        info!(
            gateway_url = %gateway_url,
            storage_api_get = %config.storage_api_get,
            storage_api_put = %config.storage_api_put,
            "worker storage ancestor api resolver configured"
        );
    }
    fs::create_dir_all(&config.work_dir).await?;
    fs::create_dir_all(&config.artifact_cache_dir).await?;

    let client = match config.service_context.as_ref() {
        Some(context) => context.client()?,
        None => Client::builder()
            .timeout(Duration::from_secs(60))
            .no_proxy()
            .build()
            .context("create worker http client failed")?,
    };

    register_until_available(&client, &config, &health).await;
    let mut stream_wakeup = RedisTaskWakeup::from_config(&config).await;

    let semaphore = Arc::new(Semaphore::new(config.max_concurrency));
    {
        let client = client.clone();
        let config = config.clone();
        let semaphore = semaphore.clone();
        let health = health.clone();
        tokio::spawn(async move {
            loop {
                let running = config
                    .max_concurrency
                    .saturating_sub(semaphore.available_permits());
                if let Err(err) = heartbeat_worker(&client, &config, running).await {
                    health.mark_disconnected();
                    warn!(error = %err, "worker heartbeat failed");
                } else {
                    health.mark_registered();
                }
                tokio::time::sleep(config.heartbeat_interval).await;
            }
        });
    }

    let mut pending_task_events = Vec::new();
    loop {
        let available = semaphore.available_permits();
        if available == 0 {
            tokio::time::sleep(Duration::from_millis(300)).await;
            continue;
        }

        let claimer = JudgeApiTaskClaimer {
            client: &client,
            config: &config,
        };
        let tasks = match claim_task_cycle(
            &mut stream_wakeup,
            &claimer,
            available,
            &mut pending_task_events,
        )
        .await
        {
            Ok(tasks) => tasks,
            Err(error) => {
                health.mark_disconnected();
                warn!(%error, "worker claim failed; re-registering before retry");
                register_until_available(&client, &config, &health).await;
                continue;
            }
        };
        if tasks.is_empty() {
            continue;
        }

        for task in tasks {
            let permit = semaphore
                .clone()
                .acquire_owned()
                .await
                .context("acquire worker slot failed")?;

            let client = client.clone();
            let config = config.clone();
            let languages = languages.clone();
            tokio::spawn(async move {
                let _permit = permit;
                if let Err(err) = execute_task(client, config, languages, task).await {
                    let error_chain = format_error_chain(&err);
                    error!(error = %error_chain, "worker task failed");
                }
            });
        }
    }
}

async fn register_until_available(
    client: &Client,
    config: &WorkerLinkConfig,
    health: &HealthState,
) {
    let retry_delays = [
        Duration::from_secs(1),
        Duration::from_secs(5),
        Duration::from_secs(30),
    ];
    let mut attempt = 0_usize;
    loop {
        match register_worker(client, config).await {
            Ok(()) => {
                health.mark_registered();
                return;
            }
            Err(error) => {
                health.mark_disconnected();
                let delay = retry_delays[attempt.min(retry_delays.len() - 1)];
                warn!(%error, retry_after_seconds = delay.as_secs(), "worker registration failed");
                tokio::time::sleep(delay).await;
                attempt = attempt.saturating_add(1);
            }
        }
    }
}

async fn execute_task(
    client: Client,
    config: Arc<WorkerLinkConfig>,
    languages: Arc<LanguagesConfig>,
    task: WorkerTaskLease,
) -> Result<()> {
    let span = info_span!(
        "judge_worker.execute_task",
        otel.name = "judge-worker execute task",
        otel.kind = "consumer",
        task_id = %task.task_id,
        submission_id = task.submission_id,
        language = %task.language,
        traceparent_present = task.traceparent.is_some()
    );
    if let Some(parent_context) = task
        .traceparent
        .as_deref()
        .and_then(trace_context_from_traceparent)
    {
        let _ = span.set_parent(parent_context);
    }
    execute_task_inner(client, config, languages, task)
        .instrument(span)
        .await
}

async fn execute_task_inner(
    client: Client,
    config: Arc<WorkerLinkConfig>,
    languages: Arc<LanguagesConfig>,
    task: WorkerTaskLease,
) -> Result<()> {
    info!(
        task_id = %task.task_id,
        submission_id = task.submission_id,
        language = %task.language,
        traceparent_present = task.traceparent.is_some(),
        "claimed worker task"
    );

    // The lease belongs to this execution from the instant claim returns.  Start
    // refreshing it before any local filesystem or artifact work, and keep the
    // heartbeat alive until Judge API has acknowledged the terminal report.
    let (heartbeat_stop, heartbeat_rx) = tokio::sync::watch::channel(false);
    let heartbeat_client = client.clone();
    let heartbeat_config = config.clone();
    let heartbeat_task = task.clone();
    let mut heartbeat_handle = tokio::spawn(async move {
        lease_heartbeat_loop(
            heartbeat_client,
            heartbeat_config,
            heartbeat_task,
            heartbeat_rx,
        )
        .await
    });

    let paths = ClaimedTaskPaths::new(&config, &languages, &task);

    let execution = tokio::select! {
        biased;
        heartbeat = &mut heartbeat_handle => {
            let error = heartbeat_termination_error(heartbeat);
            let _ = fs::remove_dir_all(&paths.task_dir).await;
            return Err(error);
        }
        result = execute_claimed_task(
            &client,
            &config,
            languages,
            &task,
            &paths,
        ) => result,
    };

    let (report, terminal_acknowledged) = match execution {
        Ok(result) => {
            let report = tokio::select! {
                biased;
                heartbeat = &mut heartbeat_handle => {
                    let error = heartbeat_termination_error(heartbeat);
                    let _ = fs::remove_dir_all(&paths.task_dir).await;
                    return Err(error);
                }
                result = submit_result(&client, &config, &task, &result) => result,
            };
            let acknowledged = report.is_ok();
            (report, acknowledged)
        }
        Err(failure) => {
            let failure_message = failure.error.to_string();
            let report = tokio::select! {
                biased;
                heartbeat = &mut heartbeat_handle => {
                    let error = heartbeat_termination_error(heartbeat);
                    let _ = fs::remove_dir_all(&paths.task_dir).await;
                    return Err(error);
                }
                result = fail_task_with_retry(
                    &client,
                    &config,
                    &task,
                    failure.retryable,
                    failure.error_type,
                    &failure_message,
                ) => result,
            };
            match report {
                Ok(()) => (Err(failure.error), true),
                Err(report_error) => (
                    Err(anyhow!(
                        "{}; reporting task failure failed: {report_error}",
                        failure.error
                    )),
                    false,
                ),
            }
        }
    };

    let heartbeat_stop_result = stop_lease_heartbeat(
        heartbeat_stop,
        &mut heartbeat_handle,
        terminal_acknowledged,
        &task.task_id,
    )
    .await;
    let _ = fs::remove_dir_all(&paths.task_dir).await;

    report?;
    heartbeat_stop_result
}

#[derive(Debug)]
struct ClaimedTaskPaths {
    task_dir: PathBuf,
    source_path: PathBuf,
    package_zip: PathBuf,
    package_dir: PathBuf,
    result_dir: PathBuf,
}

impl ClaimedTaskPaths {
    fn new(config: &WorkerLinkConfig, languages: &LanguagesConfig, task: &WorkerTaskLease) -> Self {
        let task_dir = config
            .work_dir
            .join(format!("{}-{}", task.submission_id, task.lease_version));
        Self {
            source_path: task_dir
                .join("source")
                .join(source_file_name(languages, &task.language)),
            package_zip: task_dir.join("problem.zip"),
            package_dir: task_dir.join("problem"),
            result_dir: task_dir.join("result"),
            task_dir,
        }
    }
}

#[derive(Debug)]
struct ReportableTaskFailure {
    error: anyhow::Error,
    retryable: bool,
    error_type: &'static str,
}

impl ReportableTaskFailure {
    fn retryable(error_type: &'static str, error: anyhow::Error) -> Self {
        Self {
            error,
            retryable: true,
            error_type,
        }
    }

    fn terminal(error_type: &'static str, error: anyhow::Error) -> Self {
        Self {
            error,
            retryable: false,
            error_type,
        }
    }
}

async fn execute_claimed_task(
    client: &Client,
    config: &WorkerLinkConfig,
    languages: Arc<LanguagesConfig>,
    task: &WorkerTaskLease,
    paths: &ClaimedTaskPaths,
) -> std::result::Result<ResultFile, ReportableTaskFailure> {
    if paths.task_dir.exists() {
        fs::remove_dir_all(&paths.task_dir).await.map_err(|error| {
            ReportableTaskFailure::retryable(
                "WORKSPACE_PREPARATION",
                anyhow!(error).context("remove stale task workspace failed"),
            )
        })?;
    }
    fs::create_dir_all(paths.source_path.parent().unwrap_or(&paths.task_dir))
        .await
        .map_err(|error| {
            ReportableTaskFailure::retryable(
                "WORKSPACE_PREPARATION",
                anyhow!(error).context("create task source directory failed"),
            )
        })?;
    fs::create_dir_all(&paths.package_dir)
        .await
        .map_err(|error| {
            ReportableTaskFailure::retryable(
                "WORKSPACE_PREPARATION",
                anyhow!(error).context("create task package directory failed"),
            )
        })?;
    fs::create_dir_all(&paths.result_dir)
        .await
        .map_err(|error| {
            ReportableTaskFailure::retryable(
                "WORKSPACE_PREPARATION",
                anyhow!(error).context("create task result directory failed"),
            )
        })?;

    validate_artifact_ref(config, &task.source).map_err(|error| {
        ReportableTaskFailure::terminal(
            "INVALID_TASK",
            error.context("submission source reference is invalid"),
        )
    })?;
    download_artifact(
        client,
        config,
        &task.source,
        &paths.source_path,
        task.traceparent.as_deref(),
    )
    .await
    .map_err(|error| {
        ReportableTaskFailure::retryable(
            "ARTIFACT_DOWNLOAD",
            error.context("download submission source failed"),
        )
    })?;
    validate_artifact_ref(config, &task.problem_package).map_err(|error| {
        ReportableTaskFailure::terminal(
            "INVALID_TASK",
            error.context("problem package reference is invalid"),
        )
    })?;
    download_artifact(
        client,
        config,
        &task.problem_package,
        &paths.package_zip,
        task.traceparent.as_deref(),
    )
    .await
    .map_err(|error| {
        ReportableTaskFailure::retryable(
            "ARTIFACT_DOWNLOAD",
            error.context("download problem package failed"),
        )
    })?;
    unzip_safe(&paths.package_zip, &paths.package_dir).map_err(|error| {
        ReportableTaskFailure::terminal(
            "INVALID_PROBLEM_PACKAGE",
            error.context("extract problem package failed"),
        )
    })?;

    judge_artifacts(
        languages,
        task.submission_id,
        &task.language,
        &paths.source_path,
        &paths.package_dir,
        &paths.result_dir,
    )
    .await
    .map_err(|error| ReportableTaskFailure::terminal("SYSTEM", error))
}

fn heartbeat_termination_error(
    outcome: std::result::Result<Result<()>, tokio::task::JoinError>,
) -> anyhow::Error {
    match outcome {
        Ok(Ok(())) => anyhow!("task lease heartbeat stopped before terminal report"),
        Ok(Err(error)) => error,
        Err(error) => anyhow!("task lease heartbeat task failed: {error}"),
    }
}

async fn stop_lease_heartbeat(
    stop: tokio::sync::watch::Sender<bool>,
    handle: &mut tokio::task::JoinHandle<Result<()>>,
    terminal_acknowledged: bool,
    task_id: &str,
) -> Result<()> {
    let _ = stop.send(true);
    let result = match handle.await {
        Ok(result) => result,
        Err(error) => Err(anyhow!("join task lease heartbeat failed: {error}")),
    };
    if terminal_acknowledged {
        if let Err(error) = result {
            // A heartbeat already in flight can observe the newly terminal task
            // and be rejected as stale.  The terminal ACK is the authoritative
            // outcome and must not turn a successfully reported task into a
            // local execution failure.
            warn!(task_id = %task_id, error = %error, "ignoring heartbeat shutdown error after terminal acknowledgement");
        }
        return Ok(());
    }
    result
}

fn trace_context_from_traceparent(traceparent: &str) -> Option<opentelemetry::Context> {
    let mut carrier = std::collections::HashMap::new();
    carrier.insert("traceparent".to_string(), traceparent.trim().to_string());
    let propagator = opentelemetry_sdk::propagation::TraceContextPropagator::new();
    let context = propagator.extract(&carrier);
    context.span().span_context().is_valid().then_some(context)
}

async fn register_worker(client: &Client, config: &WorkerLinkConfig) -> Result<()> {
    let req = WorkerRegisterReq {
        worker_id: config.worker_id.clone(),
        worker_name: config.worker_name.clone(),
        hostname: std::env::var("HOSTNAME").unwrap_or_default(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        capabilities: worker_capabilities(),
        supported_languages: config.supported_languages.clone(),
        max_concurrency: config.max_concurrency as i32,
    };
    let resp: WorkerRegisterResp =
        post_json(client, config, "/judge/worker/register", &req).await?;
    info!(
        worker_id = %resp.worker_id,
        status = %resp.status,
        lease_ttl_seconds = resp.lease_ttl_seconds,
        "worker registered"
    );
    Ok(())
}

async fn heartbeat_worker(
    client: &Client,
    config: &WorkerLinkConfig,
    running_count: usize,
) -> Result<()> {
    let req = WorkerHeartbeatReq {
        worker_id: config.worker_id.clone(),
        running_tasks: vec![],
        running_count: running_count as i32,
        available_slots: config.max_concurrency.saturating_sub(running_count) as i32,
    };
    let _: WorkerHeartbeatResp = post_json(client, config, "/judge/worker/heartbeat", &req).await?;
    Ok(())
}

enum RedisTaskWakeup {
    Stream {
        connection: redis::aio::ConnectionManager,
        stream: String,
        group: String,
        consumer: String,
    },
    Sleep(Duration),
}

trait TaskEventWakeup {
    fn database_claim_uses_long_poll(&self) -> bool;

    async fn wait_for_task_event(&mut self) -> Vec<RedisTaskEvent>;

    async fn ack_task_events(&mut self, entry_ids: &[String]) -> bool;

    async fn backoff_after_ack_failure(&mut self);
}

trait TaskClaimer {
    async fn claim_tasks(
        &self,
        available_slots: usize,
        pending_task_events: &[RedisTaskEvent],
        long_poll: bool,
    ) -> Result<Vec<WorkerTaskLease>>;
}

struct JudgeApiTaskClaimer<'a> {
    client: &'a Client,
    config: &'a WorkerLinkConfig,
}

impl TaskClaimer for JudgeApiTaskClaimer<'_> {
    async fn claim_tasks(
        &self,
        available_slots: usize,
        pending_task_events: &[RedisTaskEvent],
        long_poll: bool,
    ) -> Result<Vec<WorkerTaskLease>> {
        claim_tasks(
            self.client,
            self.config,
            available_slots,
            pending_task_events,
            long_poll,
        )
        .await
    }
}

async fn claim_task_cycle<W, C>(
    wakeup: &mut W,
    claimer: &C,
    available_slots: usize,
    pending_task_events: &mut Vec<RedisTaskEvent>,
) -> Result<Vec<WorkerTaskLease>>
where
    W: TaskEventWakeup,
    C: TaskClaimer,
{
    let long_poll = wakeup.database_claim_uses_long_poll();
    if !long_poll && pending_task_events.is_empty() {
        pending_task_events.extend(wakeup.wait_for_task_event().await);
    }

    let mut tasks = claimer
        .claim_tasks(available_slots, pending_task_events, long_poll)
        .await?;
    let traceparents = traceparents_by_task_id(pending_task_events);
    for task in &mut tasks {
        if task.traceparent.is_none() {
            task.traceparent = traceparents.get(&task.task_id).cloned();
        }
    }
    acknowledge_pending_task_events(wakeup, pending_task_events).await;
    Ok(tasks)
}

async fn acknowledge_pending_task_events<W>(
    wakeup: &mut W,
    pending_task_events: &mut Vec<RedisTaskEvent>,
) -> bool
where
    W: TaskEventWakeup,
{
    if pending_task_events.is_empty() {
        return true;
    }
    if !wakeup
        .ack_task_events(&stream_event_ids(pending_task_events))
        .await
    {
        wakeup.backoff_after_ack_failure().await;
        return false;
    }
    pending_task_events.clear();
    true
}

impl RedisTaskWakeup {
    async fn from_config(config: &WorkerLinkConfig) -> Self {
        let Some(redis_url) = config.redis_url.as_ref() else {
            return Self::Sleep(Duration::from_secs(1));
        };
        match Self::connect(
            redis_url,
            &config.redis_task_stream,
            &config.redis_consumer_group,
            &config.worker_id,
        )
        .await
        {
            Ok(wakeup) => wakeup,
            Err(err) => {
                warn!(error = %err, "redis task stream wakeup disabled");
                Self::Sleep(Duration::from_secs(1))
            }
        }
    }

    async fn connect(redis_url: &str, stream: &str, group: &str, consumer: &str) -> Result<Self> {
        let client = redis::Client::open(redis_url)
            .with_context(|| "open redis client for judge task stream failed")?;
        let mut connection = client
            .get_connection_manager()
            .await
            .context("connect redis task stream failed")?;
        let group_result: redis::RedisResult<()> = redis::cmd("XGROUP")
            .arg("CREATE")
            .arg(stream)
            .arg(group)
            .arg("$")
            .arg("MKSTREAM")
            .query_async(&mut connection)
            .await;
        if let Err(err) = group_result
            && !err.to_string().contains("BUSYGROUP")
        {
            return Err(err).context("create redis task stream consumer group failed");
        }
        let created: i64 = redis::cmd("XGROUP")
            .arg("CREATECONSUMER")
            .arg(stream)
            .arg(group)
            .arg(consumer)
            .query_async(&mut connection)
            .await
            .context("create redis task stream consumer failed")?;
        info!(
            stream = %stream,
            group = %group,
            consumer = %consumer,
            created,
            "redis task stream wakeup enabled"
        );
        Ok(Self::Stream {
            connection,
            stream: stream.to_string(),
            group: group.to_string(),
            consumer: consumer.to_string(),
        })
    }
}

impl TaskEventWakeup for RedisTaskWakeup {
    fn database_claim_uses_long_poll(&self) -> bool {
        matches!(self, Self::Sleep(_))
    }

    async fn wait_for_task_event(&mut self) -> Vec<RedisTaskEvent> {
        match self {
            Self::Sleep(duration) => {
                tokio::time::sleep(*duration).await;
                Vec::new()
            }
            Self::Stream {
                connection,
                stream,
                group,
                consumer,
            } => {
                let result: redis::RedisResult<redis::Value> = redis::cmd("XREADGROUP")
                    .arg("GROUP")
                    .arg(group.as_str())
                    .arg(consumer.as_str())
                    .arg("COUNT")
                    .arg(1)
                    .arg("BLOCK")
                    .arg(1000)
                    .arg("STREAMS")
                    .arg(stream.as_str())
                    .arg(">")
                    .query_async(connection)
                    .await;
                match result {
                    Ok(value) if value != redis::Value::Nil => {
                        let events = redis_stream_task_events(&value);
                        info!(
                            stream = %stream,
                            event_count = events.len(),
                            "redis task stream event received"
                        );
                        events
                    }
                    Ok(_) => Vec::new(),
                    Err(err) => {
                        warn!(error = %err, "redis task stream read failed");
                        tokio::time::sleep(Duration::from_secs(1)).await;
                        Vec::new()
                    }
                }
            }
        }
    }

    async fn ack_task_events(&mut self, entry_ids: &[String]) -> bool {
        if entry_ids.is_empty() {
            return true;
        }
        match self {
            Self::Sleep(_) => true,
            Self::Stream {
                connection,
                stream,
                group,
                ..
            } => {
                let mut cmd = redis::cmd("XACK");
                cmd.arg(stream.as_str()).arg(group.as_str());
                for entry_id in entry_ids {
                    cmd.arg(entry_id.as_str());
                }
                let result: redis::RedisResult<i64> = cmd.query_async(connection).await;
                match result {
                    Ok(acked) => {
                        info!(
                            stream = %stream,
                            group = %group,
                            requested = entry_ids.len(),
                            acked,
                            "redis task stream events acknowledged"
                        );
                        true
                    }
                    Err(err) => {
                        warn!(
                            stream = %stream,
                            group = %group,
                            error = %err,
                            "redis task stream ack failed"
                        );
                        false
                    }
                }
            }
        }
    }

    async fn backoff_after_ack_failure(&mut self) {
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RedisTaskEvent {
    entry_id: String,
    task_id: Option<String>,
    submission_id: Option<i64>,
    traceparent: Option<String>,
}

fn stream_event_ids(events: &[RedisTaskEvent]) -> Vec<String> {
    events.iter().map(|event| event.entry_id.clone()).collect()
}

fn stream_task_ids(events: &[RedisTaskEvent]) -> Vec<String> {
    events
        .iter()
        .filter_map(|event| event.task_id.clone())
        .collect()
}

fn traceparents_by_task_id(events: &[RedisTaskEvent]) -> std::collections::HashMap<String, String> {
    events
        .iter()
        .filter_map(|event| {
            let task_id = event.task_id.as_ref()?;
            let traceparent = event.traceparent.as_ref()?;
            Some((task_id.clone(), traceparent.clone()))
        })
        .collect()
}

fn redis_stream_task_events(value: &redis::Value) -> Vec<RedisTaskEvent> {
    let redis::Value::Array(streams) = value else {
        return Vec::new();
    };
    streams
        .iter()
        .flat_map(redis_stream_task_events_from_stream)
        .collect()
}

fn redis_stream_task_events_from_stream(value: &redis::Value) -> Vec<RedisTaskEvent> {
    let redis::Value::Array(parts) = value else {
        return Vec::new();
    };
    let Some(redis::Value::Array(entries)) = parts.get(1) else {
        return Vec::new();
    };
    entries.iter().filter_map(redis_stream_task_event).collect()
}

fn redis_stream_task_event(value: &redis::Value) -> Option<RedisTaskEvent> {
    let redis::Value::Array(parts) = value else {
        return None;
    };
    let entry_id = parts.first().and_then(redis_value_to_string)?;
    let values = parts.get(1).map(redis_stream_fields).unwrap_or_default();
    Some(RedisTaskEvent {
        entry_id,
        task_id: values
            .iter()
            .find(|(key, _)| key == "task_id")
            .map(|(_, value)| value.clone()),
        submission_id: values
            .iter()
            .find(|(key, _)| key == "submission_id")
            .and_then(|(_, value)| value.parse::<i64>().ok()),
        traceparent: values
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case("traceparent"))
            .map(|(_, value)| value.clone()),
    })
}

fn redis_stream_fields(value: &redis::Value) -> Vec<(String, String)> {
    let redis::Value::Array(parts) = value else {
        return Vec::new();
    };
    parts
        .chunks(2)
        .filter_map(|chunk| {
            let key = chunk.first().and_then(redis_value_to_string)?;
            let value = chunk.get(1).and_then(redis_value_to_string)?;
            Some((key, value))
        })
        .collect()
}

fn redis_value_to_string(value: &redis::Value) -> Option<String> {
    match value {
        redis::Value::BulkString(bytes) => String::from_utf8(bytes.clone()).ok(),
        redis::Value::SimpleString(value) => Some(value.clone()),
        redis::Value::Int(value) => Some(value.to_string()),
        _ => None,
    }
}

async fn claim_tasks(
    client: &Client,
    config: &WorkerLinkConfig,
    available_slots: usize,
    pending_task_events: &[RedisTaskEvent],
    long_poll: bool,
) -> Result<Vec<WorkerTaskLease>> {
    let req = WorkerClaimTasksReq {
        worker_id: config.worker_id.clone(),
        capabilities: worker_capabilities(),
        supported_languages: config.supported_languages.clone(),
        available_slots: available_slots as i32,
        task_ids: stream_task_ids(pending_task_events),
    };
    let resp: WorkerClaimTasksResp = post_json_with_trace_and_idempotency(
        client,
        config,
        "/judge/worker/tasks/claim",
        &req,
        None,
        None,
        long_poll,
    )
    .await?;
    Ok(resp.tasks)
}

fn worker_capabilities() -> Vec<String> {
    vec!["nsjail".to_string(), "cgroup-v2".to_string()]
}

async fn validate_runtime_preflight(
    config: &WorkerLinkConfig,
    languages: &LanguagesConfig,
) -> Result<()> {
    validate_runner_policy(config)?;
    validate_supported_languages(config, languages)?;
    ensure_writable_dir(&config.work_dir, "OJOS_WORK_DIR").await?;
    ensure_writable_dir(&config.artifact_cache_dir, "OJOS_ARTIFACT_CACHE_DIR").await?;

    validate_language_toolchain(config, languages)?;

    if env_bool("OJOS_ALLOW_CGROUP_FALLBACK") {
        return Err(anyhow!(
            "OJOS_ALLOW_CGROUP_FALLBACK must be false for nsjail production workers"
        ));
    }
    if !nsjail_available() {
        return Err(anyhow!(
            "nsjail binary is required for OJOS_RUNNER_MODE=nsjail and was not found on PATH"
        ));
    }

    let _probe =
        CgroupRun::create(64, 64).context("cgroup v2 preflight failed for nsjail runner")?;
    Ok(())
}

fn validate_runner_policy(config: &WorkerLinkConfig) -> Result<()> {
    match config.runner_mode.as_str() {
        "nsjail" => Ok(()),
        other => Err(anyhow!("unsupported runner mode: {other}")),
    }
}

fn validate_supported_languages(
    config: &WorkerLinkConfig,
    languages: &LanguagesConfig,
) -> Result<()> {
    if config.supported_languages.is_empty() {
        return Err(anyhow!("OJOS_SUPPORTED_LANGUAGES resolved to an empty set"));
    }
    for language in &config.supported_languages {
        if !languages.languages.contains_key(language) {
            return Err(anyhow!(
                "supported language {language:?} is not present in languages config"
            ));
        }
    }
    Ok(())
}

fn validate_language_toolchain(
    config: &WorkerLinkConfig,
    languages: &LanguagesConfig,
) -> Result<()> {
    for language in &config.supported_languages {
        let lang = languages
            .get(language)
            .ok_or_else(|| anyhow!("language {language:?} is not present in languages config"))?;
        if lang.compile.enabled {
            ensure_command_available(language, "compile", &lang.compile.command)?;
        }
        ensure_runtime_command_available(language, lang)?;
    }
    Ok(())
}

fn ensure_runtime_command_available(language: &str, lang: &LanguageConfig) -> Result<()> {
    let command = lang.run.command.trim();
    if generated_runtime_command(command) {
        return Ok(());
    }
    ensure_command_available(language, "run", command)
}

fn ensure_command_available(language: &str, phase: &str, command: &str) -> Result<()> {
    let command = command.trim();
    if command.is_empty() {
        return Err(anyhow!("{language} {phase} command must not be empty"));
    }
    if generated_runtime_command(command) {
        return Ok(());
    }
    if command_available_on_path(command) {
        return Ok(());
    }
    Err(anyhow!(
        "{language} {phase} command {command:?} is not available on PATH"
    ))
}

fn generated_runtime_command(command: &str) -> bool {
    command.contains("{exe}") || command.contains("{source}") || command.contains("{workdir}")
}

fn command_available_on_path(command: &str) -> bool {
    if command.contains('/') || command.contains('\\') {
        return is_executable_file(Path::new(command));
    }
    let Some(paths) = env::var_os("PATH") else {
        return false;
    };
    for dir in env::split_paths(&paths) {
        let candidate = dir.join(command);
        if is_executable_file(&candidate) {
            return true;
        }
        #[cfg(windows)]
        {
            let pathext = env::var("PATHEXT").unwrap_or_else(|_| {
                ".COM;.EXE;.BAT;.CMD;.VBS;.VBE;.JS;.JSE;.WSF;.WSH;.MSC".to_string()
            });
            for ext in pathext.split(';').filter(|ext| !ext.is_empty()) {
                if is_executable_file(&dir.join(format!("{command}{ext}"))) {
                    return true;
                }
            }
        }
    }
    false
}

#[cfg(unix)]
fn is_executable_file(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;

    path.metadata()
        .map(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

#[cfg(not(unix))]
fn is_executable_file(path: &Path) -> bool {
    path.is_file()
}

async fn ensure_writable_dir(path: &Path, label: &str) -> Result<()> {
    fs::create_dir_all(path)
        .await
        .with_context(|| format!("create {label} failed: {}", path.display()))?;
    let probe = path.join(format!(
        ".preflight-write-{}-{}",
        std::process::id(),
        Uuid::new_v4()
    ));
    fs::write(&probe, b"ok")
        .await
        .with_context(|| format!("write {label} preflight probe failed: {}", path.display()))?;
    fs::remove_file(&probe)
        .await
        .with_context(|| format!("remove {label} preflight probe failed: {}", probe.display()))?;
    Ok(())
}

async fn lease_heartbeat_loop(
    client: Client,
    config: Arc<WorkerLinkConfig>,
    task: WorkerTaskLease,
    mut stop: tokio::sync::watch::Receiver<bool>,
) -> Result<()> {
    let mut lease_expires_at = task.lease_expires_at.clone();
    let mut first = true;
    loop {
        let delay = if first {
            Duration::ZERO
        } else {
            lease_heartbeat_delay(config.task_lease_ttl, lease_expires_at.as_deref())
        };
        tokio::select! {
            biased;
            changed = stop.changed() => {
                match changed {
                    Ok(()) if *stop.borrow() => return Ok(()),
                    Ok(()) => {}
                    Err(_) => return Err(anyhow!("task lease heartbeat stop channel closed")),
                }
            }
            _ = tokio::time::sleep(delay) => {
                let req = WorkerTaskHeartbeatReq {
                    worker_id: config.worker_id.clone(),
                    lease_version: task.lease_version,
                };
                let path = format!("/judge/worker/tasks/{}/heartbeat", task.task_id);
                let response = post_json_with_trace::<_, WorkerTaskHeartbeatResp>(
                    &client,
                    &config,
                    &path,
                    &req,
                    task.traceparent.as_deref(),
                )
                .await;
                match response {
                    Ok(response) => {
                        first = false;
                        if response.lease_expires_at.is_some() {
                            lease_expires_at = response.lease_expires_at;
                        }
                    }
                    Err(err) => {
                        warn!(task_id = %task.task_id, error = %err, "task heartbeat failed");
                        return Err(err.context("task lease heartbeat failed"));
                    }
                }
            }
        }
    }
}

fn lease_heartbeat_delay(configured_ttl: Duration, lease_expires_at: Option<&str>) -> Duration {
    let configured = configured_ttl / 3;
    let server_remaining = lease_expires_at
        .and_then(|value| chrono::DateTime::parse_from_rfc3339(value).ok())
        .and_then(|expires_at| {
            let remaining = expires_at.with_timezone(&chrono::Utc) - chrono::Utc::now();
            remaining.to_std().ok()
        })
        .map(|remaining| remaining / 3);
    configured
        .min(server_remaining.unwrap_or(configured))
        .max(Duration::from_millis(10))
}

async fn submit_result(
    client: &Client,
    config: &WorkerLinkConfig,
    task: &WorkerTaskLease,
    result: &ResultFile,
) -> Result<()> {
    let cases = result
        .cases
        .iter()
        .map(|case| WorkerResultCase {
            case_no: case.case_no,
            status: case.status.clone(),
            score: case.score,
            time_ms: case.time_ms,
            memory_kb: case.memory_kb,
            message: case.message.clone(),
            stdout: read_text_limited(&case.stdout_path, 64 * 1024).unwrap_or_default(),
            stderr: read_text_limited(&case.stderr_path, 64 * 1024).unwrap_or_default(),
            checker_log: read_text_limited(&case.checker_log_path, 64 * 1024).unwrap_or_default(),
        })
        .collect();

    let req = WorkerSubmitResultReq {
        worker_id: config.worker_id.clone(),
        lease_version: task.lease_version,
        status: result.status.clone(),
        score: result.score,
        time_ms: result.time_ms,
        memory_kb: result.memory_kb,
        message: result.message.clone(),
        cases,
    };
    let idempotency_key = task_report_idempotency_key(task, "result", &req)?;
    let path = format!("/judge/worker/tasks/{}/result", task.task_id);
    let mut attempt = 0_usize;
    loop {
        let response: Result<WorkerSubmitResultResp> = post_json_with_trace_and_idempotency(
            client,
            config,
            &path,
            &req,
            task.traceparent.as_deref(),
            Some(&idempotency_key),
            false,
        )
        .await;
        let error = match response {
            Ok(resp) if resp.accepted => return Ok(()),
            Ok(resp) => {
                TaskReportRejected(format!("judge-api rejected task result: {}", resp.status))
                    .into()
            }
            Err(error) => error,
        };
        warn!(
            task_id = %task.task_id,
            lease_version = task.lease_version,
            attempt = attempt + 1,
            error = %error,
            "reporting task result failed"
        );
        if !terminal_report_error_retryable(&error) {
            return Err(error);
        }
        tokio::time::sleep(terminal_report_retry_delay(attempt)).await;
        attempt = attempt.saturating_add(1);
    }
}

async fn fail_task_once(
    client: &Client,
    config: &WorkerLinkConfig,
    task: &WorkerTaskLease,
    retryable: bool,
    error_type: &str,
    message: &str,
    idempotency_key: &str,
) -> Result<()> {
    let req = WorkerFailTaskReq {
        worker_id: config.worker_id.clone(),
        lease_version: task.lease_version,
        error_type: error_type.to_string(),
        message: message.to_string(),
        retryable,
    };
    let path = format!("/judge/worker/tasks/{}/fail", task.task_id);
    let resp: WorkerFailTaskResp = post_json_with_trace_and_idempotency(
        client,
        config,
        &path,
        &req,
        task.traceparent.as_deref(),
        Some(idempotency_key),
        false,
    )
    .await?;
    if !resp.accepted {
        return Err(TaskReportRejected(format!(
            "judge-api rejected task failure: {}",
            resp.status
        ))
        .into());
    }
    Ok(())
}

async fn fail_task_with_retry(
    client: &Client,
    config: &WorkerLinkConfig,
    task: &WorkerTaskLease,
    retryable: bool,
    error_type: &str,
    message: &str,
) -> Result<()> {
    let idempotency_payload = WorkerFailTaskReq {
        worker_id: config.worker_id.clone(),
        lease_version: task.lease_version,
        error_type: error_type.to_string(),
        message: message.to_string(),
        retryable,
    };
    let idempotency_key = task_report_idempotency_key(task, "fail", &idempotency_payload)?;

    let mut attempt = 0_usize;
    loop {
        match fail_task_once(
            client,
            config,
            task,
            retryable,
            error_type,
            message,
            &idempotency_key,
        )
        .await
        {
            Ok(()) => return Ok(()),
            Err(error) => {
                warn!(
                    task_id = %task.task_id,
                    lease_version = task.lease_version,
                    attempt = attempt + 1,
                    error = %error,
                    "reporting task failure failed"
                );
                if !terminal_report_error_retryable(&error) {
                    return Err(error);
                }
                tokio::time::sleep(terminal_report_retry_delay(attempt)).await;
                attempt = attempt.saturating_add(1);
            }
        }
    }
}

fn terminal_report_error_retryable(error: &anyhow::Error) -> bool {
    if error.downcast_ref::<TaskReportRejected>().is_some() {
        return false;
    }
    if let Some(response) = error.downcast_ref::<JudgeApiResponseError>() {
        return response.status == StatusCode::TOO_MANY_REQUESTS
            || response.status.is_server_error();
    }
    // Transport interruption, response truncation, and decode failures are
    // ambiguous. The lease heartbeat is the retry deadline, so keep replaying
    // the stable receipt request until ACK or an explicit rejection.
    true
}

fn terminal_report_retry_delay(attempt: usize) -> Duration {
    match attempt {
        0 => Duration::from_millis(100),
        1 => Duration::from_secs(1),
        2 => Duration::from_secs(5),
        _ => Duration::from_secs(30),
    }
}

fn task_report_idempotency_key<T>(
    task: &WorkerTaskLease,
    report_kind: &str,
    payload: &T,
) -> Result<String>
where
    T: Serialize + ?Sized,
{
    let mut hasher = Sha256::new();
    hasher.update(task.task_id.as_bytes());
    hasher.update([0]);
    hasher.update(task.lease_version.to_be_bytes());
    hasher.update([0]);
    hasher.update(report_kind.as_bytes());
    hasher.update([0]);
    hasher
        .update(serde_json::to_vec(payload).context("serialize task report idempotency payload")?);
    Ok(format!("judge-{report_kind}-{:x}", hasher.finalize()))
}

async fn download_artifact(
    client: &Client,
    config: &WorkerLinkConfig,
    artifact: &WorkerArtifactRef,
    target: &Path,
    traceparent: Option<&str>,
) -> Result<()> {
    validate_artifact_ref(config, artifact)?;
    if artifact.size_bytes <= 0 {
        return Err(anyhow!("artifact size must be positive"));
    }
    if let Some(context) = config.service_context.as_ref() {
        let binding = artifact
            .binding
            .as_deref()
            .ok_or_else(|| anyhow!("managed artifact binding is required"))?;
        let api_id = artifact
            .api_id
            .as_deref()
            .ok_or_else(|| anyhow!("managed artifact api_id is required"))?;
        let relative_path = artifact
            .relative_path
            .as_deref()
            .ok_or_else(|| anyhow!("managed artifact relative_path is required"))?;
        let declared = context.binding(binding)?;
        if api_id != declared.api_id {
            return Err(anyhow!(
                "artifact API does not match the named service binding"
            ));
        }
        // The shared SDK performs an atomic streaming download, reads the
        // current rotated credential, enforces the binding timeout, and verifies
        // both digest and size before publishing the destination file.
        return context
            .download_to(
                client,
                binding,
                relative_path,
                &artifact.sha256,
                artifact.size_bytes as u64,
                target,
            )
            .await;
    }
    let url = artifact_url(config, artifact)?;
    let request = with_traceparent(client.get(url), traceparent);
    let request = authorize_request(config, request, artifact.uses_internal_api()).await?;
    let mut resp = request.send().await?.error_for_status()?;

    let mut file = fs::File::create(target).await?;
    let mut hasher = Sha256::new();
    let mut written: u64 = 0;
    while let Some(chunk) = resp.chunk().await? {
        written += chunk.len() as u64;
        if written > artifact.size_bytes as u64 {
            return Err(anyhow!("artifact exceeded advertised size"));
        }
        hasher.update(&chunk);
        file.write_all(&chunk).await?;
    }
    file.flush().await?;

    if written != artifact.size_bytes as u64 {
        return Err(anyhow!(
            "artifact size mismatch: expected {}, received {}",
            artifact.size_bytes,
            written
        ));
    }
    let digest = format!("{:x}", hasher.finalize());
    if digest != artifact.sha256.trim_start_matches("sha256:") {
        return Err(anyhow!("artifact digest mismatch"));
    }
    Ok(())
}

fn validate_artifact_ref(config: &WorkerLinkConfig, artifact: &WorkerArtifactRef) -> Result<()> {
    if artifact.size_bytes <= 0 {
        return Err(anyhow!("artifact size must be positive"));
    }
    let digest = artifact.sha256.trim().trim_start_matches("sha256:");
    if digest.len() != 64 || !digest.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(anyhow!("artifact sha256 is invalid"));
    }
    if config.service_context.is_none() {
        if artifact.url.trim().is_empty() {
            return Err(anyhow!("legacy artifact URL is required"));
        }
        return Ok(());
    }
    if !artifact.url.trim().is_empty() {
        return Err(anyhow!(
            "managed artifact references must not contain a legacy URL"
        ));
    }
    let binding = artifact
        .binding
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| anyhow!("managed artifact binding is required"))?;
    let api_id = artifact
        .api_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| anyhow!("managed artifact api_id is required"))?;
    let relative_path = artifact
        .relative_path
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| anyhow!("managed artifact relative_path is required"))?;
    if binding != "storage_get" || api_id != "storage.object.get" {
        return Err(anyhow!(
            "managed judge artifacts require binding storage_get with API storage.object.get"
        ));
    }
    validate_artifact_relative_path(relative_path)?;

    let context = config
        .service_context
        .as_ref()
        .ok_or_else(|| anyhow!("managed service context is missing"))?;
    let declared = context.binding(binding)?;
    if api_id != declared.api_id {
        return Err(anyhow!(
            "artifact API {api_id} does not match binding {binding} API {}",
            declared.api_id
        ));
    }
    Ok(())
}

fn validate_artifact_relative_path(path: &str) -> Result<()> {
    let lower = path.to_ascii_lowercase();
    if !path.starts_with('/')
        || path.starts_with("//")
        || path.contains('\\')
        || path.contains(['?', '#'])
        || path.chars().any(char::is_control)
        || lower.contains("%2e")
        || lower.contains("%2f")
        || lower.contains("%5c")
        || path
            .split('/')
            .any(|segment| segment == "." || segment == "..")
    {
        return Err(anyhow!("managed artifact relative_path is unsafe"));
    }
    Ok(())
}

async fn post_json<T, R>(
    client: &Client,
    config: &WorkerLinkConfig,
    path: &str,
    body: &T,
) -> Result<R>
where
    T: Serialize + ?Sized,
    R: for<'de> Deserialize<'de>,
{
    post_json_with_trace(client, config, path, body, None).await
}

async fn post_json_with_trace<T, R>(
    client: &Client,
    config: &WorkerLinkConfig,
    path: &str,
    body: &T,
    traceparent: Option<&str>,
) -> Result<R>
where
    T: Serialize + ?Sized,
    R: for<'de> Deserialize<'de>,
{
    post_json_with_trace_and_idempotency(client, config, path, body, traceparent, None, false).await
}

async fn post_json_with_trace_and_idempotency<T, R>(
    client: &Client,
    config: &WorkerLinkConfig,
    path: &str,
    body: &T,
    traceparent: Option<&str>,
    idempotency_key: Option<&str>,
    prefer_wait: bool,
) -> Result<R>
where
    T: Serialize + ?Sized,
    R: for<'de> Deserialize<'de>,
{
    let managed = config.service_context.as_ref();
    let request = match managed {
        Some(context) => {
            let relative_path = path.strip_prefix("/judge/worker").unwrap_or(path);
            context
                .request(client, "judge_control", Method::POST, relative_path)
                .await?
                .json(body)
        }
        None => client.post(absolute_url(config, path)).json(body),
    };
    let mut request = with_traceparent(request, traceparent);
    if prefer_wait {
        request = request.header("Prefer", "wait=25");
    }
    if let Some(idempotency_key) = idempotency_key {
        request = request.header("Idempotency-Key", idempotency_key);
    }
    let request = if managed.is_some() {
        request
    } else {
        authorize_request(config, request, false).await?
    };
    let resp = request.send().await?;
    let status = resp.status();
    let text = resp.text().await?;
    if !status.is_success() {
        return Err(JudgeApiResponseError { status, body: text }.into());
    }
    serde_json::from_str(&text).with_context(|| format!("decode judge-api response: {}", text))
}

async fn authorize_request(
    config: &WorkerLinkConfig,
    mut request: RequestBuilder,
    legacy_internal_api: bool,
) -> Result<RequestBuilder> {
    if let Some(context) = config.service_context.as_ref() {
        return context.authorize(request).await;
    }
    request = request.header("X-OJOS-Worker-Token", &config.worker_token);
    if legacy_internal_api {
        request = request.header("X-OJOS-Caller-Service", "judge-worker");
        if let Some(node_id) = &config.caller_node_id {
            request = request
                .header("X-OJOS-Node-Id", node_id)
                .header("X-OJOS-Caller-Node-Id", node_id);
        }
        if let Some(token) = &config.service_token {
            request = request.bearer_auth(token);
        }
    }
    Ok(request)
}

fn artifact_url(config: &WorkerLinkConfig, artifact: &WorkerArtifactRef) -> Result<String> {
    if let (Some(context), Some(binding), Some(relative_path)) = (
        config.service_context.as_ref(),
        artifact.binding.as_deref(),
        artifact.relative_path.as_deref(),
    ) {
        let declared = context.binding(binding)?;
        if let Some(api_id) = artifact.api_id.as_deref()
            && api_id != declared.api_id
        {
            return Err(anyhow!(
                "artifact API {} does not match binding {} API {}",
                api_id,
                binding,
                declared.api_id
            ));
        }
        return context.binding_url(binding, relative_path);
    }
    if artifact.url.trim().is_empty() {
        return Err(anyhow!(
            "artifact must contain a binding/relative_path reference or legacy url"
        ));
    }
    Ok(absolute_url(config, &artifact.url))
}

fn with_traceparent(request: RequestBuilder, traceparent: Option<&str>) -> RequestBuilder {
    match traceparent.map(str::trim).filter(|value| !value.is_empty()) {
        Some(value) => request.header("traceparent", value),
        None => request,
    }
}

fn absolute_url(config: &WorkerLinkConfig, path: &str) -> String {
    if path.starts_with("http://") || path.starts_with("https://") {
        return path.to_string();
    }
    if path.starts_with("/internal/apis/")
        && let Some(gateway_url) = &config.internal_gateway_url
    {
        return format!(
            "{}/{}",
            gateway_url.trim_end_matches('/'),
            path.trim_start_matches('/')
        );
    }
    let path = if config.service_context.is_some() {
        path.strip_prefix("/judge/worker").unwrap_or(path)
    } else {
        path
    };
    format!(
        "{}/{}",
        config.judge_api_url.trim_end_matches('/'),
        path.trim_start_matches('/')
    )
}

fn unzip_safe(zip_path: &Path, target_dir: &Path) -> Result<()> {
    let file = std::fs::File::open(zip_path)?;
    let mut archive = zip::ZipArchive::new(file)?;

    for index in 0..archive.len() {
        let mut file = archive.by_index(index)?;
        let Some(enclosed) = file.enclosed_name().map(|p| p.to_path_buf()) else {
            return Err(anyhow!("zip entry escapes package root"));
        };
        let out_path = target_dir.join(enclosed);

        if file.is_dir() {
            std::fs::create_dir_all(&out_path)?;
            continue;
        }
        if let Some(parent) = out_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut out = std::fs::File::create(&out_path)?;
        std::io::copy(&mut file, &mut out)?;
    }
    Ok(())
}

fn read_text_limited(path: &str, max_bytes: usize) -> Result<String> {
    if path.is_empty() {
        return Ok(String::new());
    }
    let data = std::fs::read(path)?;
    let data = if data.len() > max_bytes {
        &data[..max_bytes]
    } else {
        &data
    };
    Ok(String::from_utf8_lossy(data).to_string())
}

fn source_file_name(languages: &LanguagesConfig, language: &str) -> String {
    languages
        .get(language)
        .map(|lang| lang.source_file.clone())
        .unwrap_or_else(|| "source.txt".to_string())
}

fn env_or<F>(key: &str, fallback: F) -> String
where
    F: FnOnce() -> String,
{
    std::env::var(key).unwrap_or_else(|_| fallback())
}

fn normalize_runner_mode(raw: &str) -> Result<String> {
    let mode = raw.trim().to_ascii_lowercase();
    match mode.as_str() {
        "nsjail" => Ok(mode),
        "" => Err(anyhow!("OJOS_RUNNER_MODE must not be empty")),
        other => Err(anyhow!(
            "unsupported OJOS_RUNNER_MODE {other:?}; supported value is nsjail"
        )),
    }
}

fn format_error_chain(err: &anyhow::Error) -> String {
    err.chain()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(": ")
}

fn required_env(key: &str) -> Result<String> {
    let value = std::env::var(key).with_context(|| format!("{} is required", key))?;
    if value.trim().is_empty() {
        return Err(anyhow!("{} is required", key));
    }
    Ok(value)
}

fn env_parse<T>(key: &str, default: T) -> Result<T>
where
    T: std::str::FromStr,
    T::Err: std::fmt::Display,
{
    match std::env::var(key) {
        Ok(raw) if !raw.trim().is_empty() => raw
            .parse::<T>()
            .map_err(|err| anyhow!("parse {} failed: {}", key, err)),
        _ => Ok(default),
    }
}

fn env_bool(key: &str) -> bool {
    match std::env::var(key) {
        Ok(value) => matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "1" | "true" | "yes" | "on"
        ),
        Err(_) => false,
    }
}

#[derive(Debug, Serialize)]
struct WorkerRegisterReq {
    worker_id: String,
    worker_name: String,
    hostname: String,
    version: String,
    capabilities: Vec<String>,
    supported_languages: Vec<String>,
    max_concurrency: i32,
}

#[derive(Debug, Deserialize)]
struct WorkerRegisterResp {
    worker_id: String,
    lease_ttl_seconds: i64,
    status: String,
}

#[derive(Debug, Serialize)]
struct WorkerHeartbeatReq {
    worker_id: String,
    running_tasks: Vec<String>,
    running_count: i32,
    available_slots: i32,
}

#[derive(Debug, Deserialize)]
struct WorkerHeartbeatResp {}

#[derive(Debug, Serialize)]
struct WorkerClaimTasksReq {
    worker_id: String,
    capabilities: Vec<String>,
    supported_languages: Vec<String>,
    available_slots: i32,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    task_ids: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct WorkerClaimTasksResp {
    tasks: Vec<WorkerTaskLease>,
}

#[derive(Debug, Clone, Deserialize)]
struct WorkerTaskLease {
    task_id: String,
    submission_id: i64,
    language: String,
    lease_version: i32,
    #[serde(default)]
    lease_expires_at: Option<String>,
    source: WorkerArtifactRef,
    problem_package: WorkerArtifactRef,
    #[serde(default)]
    traceparent: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct WorkerArtifactRef {
    #[serde(default)]
    url: String,
    #[serde(default)]
    binding: Option<String>,
    #[serde(default)]
    api_id: Option<String>,
    #[serde(default)]
    relative_path: Option<String>,
    sha256: String,
    size_bytes: i64,
}

impl WorkerArtifactRef {
    fn uses_internal_api(&self) -> bool {
        self.binding.is_some() || self.url.starts_with("/internal/apis/")
    }
}

#[derive(Debug, Serialize)]
struct WorkerTaskHeartbeatReq {
    worker_id: String,
    lease_version: i32,
}

#[derive(Debug, Deserialize)]
struct WorkerTaskHeartbeatResp {
    #[serde(default)]
    lease_expires_at: Option<String>,
}

#[derive(Debug, Serialize)]
struct WorkerSubmitResultReq {
    worker_id: String,
    lease_version: i32,
    status: String,
    score: i32,
    time_ms: i32,
    memory_kb: i32,
    message: String,
    cases: Vec<WorkerResultCase>,
}

#[derive(Debug, Serialize)]
struct WorkerResultCase {
    case_no: i32,
    status: String,
    score: i32,
    time_ms: i32,
    memory_kb: i32,
    message: String,
    stdout: String,
    stderr: String,
    checker_log: String,
}

#[derive(Debug, Deserialize)]
struct WorkerSubmitResultResp {
    accepted: bool,
    status: String,
}

#[derive(Debug, Serialize)]
struct WorkerFailTaskReq {
    worker_id: String,
    lease_version: i32,
    error_type: String,
    message: String,
    retryable: bool,
}

#[derive(Debug, Deserialize)]
struct WorkerFailTaskResp {
    accepted: bool,
    status: String,
}
