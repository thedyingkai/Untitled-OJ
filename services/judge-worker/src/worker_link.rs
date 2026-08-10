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
    pub smoke_once: bool,
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
        let smoke_once = env_bool("OJOS_WORKER_SMOKE_ONCE");

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
            smoke_once,
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

#[cfg(test)]
fn internal_api_url(gateway_url: &str, api_id: &str, path: &str) -> String {
    format!(
        "{}/internal/apis/{}/{}",
        gateway_url.trim_end_matches('/'),
        api_id.trim_matches('/'),
        path.trim_start_matches('/')
    )
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

        let mut tasks = match claim_tasks(&client, &config, available, &pending_task_events).await {
            Ok(tasks) => tasks,
            Err(error) => {
                health.mark_disconnected();
                warn!(%error, "worker claim failed; re-registering before retry");
                register_until_available(&client, &config, &health).await;
                continue;
            }
        };
        let traceparents = traceparents_by_task_id(&pending_task_events);
        for task in &mut tasks {
            if task.traceparent.is_none() {
                task.traceparent = traceparents.get(&task.task_id).cloned();
            }
        }
        if !pending_task_events.is_empty()
            && stream_wakeup
                .ack_task_events(&stream_event_ids(&pending_task_events))
                .await
        {
            pending_task_events.clear();
        }
        if tasks.is_empty() {
            pending_task_events.extend(stream_wakeup.wait_for_task_event().await);
            continue;
        }

        for task in tasks {
            let permit = semaphore
                .clone()
                .acquire_owned()
                .await
                .context("acquire worker slot failed")?;
            if config.smoke_once {
                let _permit = permit;
                execute_task(client.clone(), config.clone(), languages.clone(), task).await?;
                if !pending_task_events.is_empty()
                    && !stream_wakeup
                        .ack_task_events(&stream_event_ids(&pending_task_events))
                        .await
                {
                    warn!("worker smoke-once task completed but redis ack failed");
                }
                info!("worker smoke-once task completed");
                return Ok(());
            }

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
        info!(
            stream = %stream,
            group = %group,
            consumer = %consumer,
            "redis task stream wakeup enabled"
        );
        Ok(Self::Stream {
            connection,
            stream: stream.to_string(),
            group: group.to_string(),
            consumer: consumer.to_string(),
        })
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
) -> Result<Vec<WorkerTaskLease>> {
    let req = WorkerClaimTasksReq {
        worker_id: config.worker_id.clone(),
        capabilities: worker_capabilities(),
        supported_languages: config.supported_languages.clone(),
        available_slots: available_slots as i32,
        task_ids: stream_task_ids(pending_task_events),
    };
    let resp: WorkerClaimTasksResp =
        post_json(client, config, "/judge/worker/tasks/claim", &req).await?;
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
    post_json_with_trace_and_idempotency(client, config, path, body, traceparent, None).await
}

async fn post_json_with_trace_and_idempotency<T, R>(
    client: &Client,
    config: &WorkerLinkConfig,
    path: &str,
    body: &T,
    traceparent: Option<&str>,
    idempotency_key: Option<&str>,
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
    if path.ends_with("/tasks/claim") {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{CompileConfig, LanguageConfig, RunConfig};
    use crate::result::{ResultCase, ResultFile};
    use std::collections::HashMap;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    fn test_worker_config(runner_mode: &str) -> WorkerLinkConfig {
        WorkerLinkConfig {
            worker_id: "worker-a".to_string(),
            worker_name: "Worker A".to_string(),
            judge_api_url: "http://judge-api:8082".to_string(),
            worker_token: "worker-token".to_string(),
            max_concurrency: 1,
            work_dir: std::env::temp_dir().join(format!("ojos-worker-test-{}", Uuid::new_v4())),
            artifact_cache_dir: std::env::temp_dir()
                .join(format!("ojos-worker-cache-test-{}", Uuid::new_v4())),
            supported_languages: vec!["cpp17".to_string()],
            heartbeat_interval: Duration::from_secs(10),
            task_lease_ttl: Duration::from_secs(45),
            redis_url: None,
            redis_task_stream: "ojos:judge:task".to_string(),
            redis_consumer_group: "judge-worker".to_string(),
            internal_gateway_url: None,
            storage_api_get: "storage.object.get".to_string(),
            storage_api_put: "storage.object.put".to_string(),
            service_token: None,
            caller_node_id: None,
            runner_mode: runner_mode.to_string(),
            smoke_once: false,
            service_context: None,
        }
    }

    fn test_languages_config() -> LanguagesConfig {
        LanguagesConfig {
            languages: HashMap::from([(
                "cpp17".to_string(),
                LanguageConfig {
                    source_file: "main.cpp".to_string(),
                    exe_file: "main".to_string(),
                    compile: CompileConfig {
                        enabled: true,
                        command: "definitely-not-a-real-compiler".to_string(),
                        args: vec![],
                        timeout_ms: 1000,
                        memory_mb: Some(256),
                    },
                    run: RunConfig {
                        command: "{exe}".to_string(),
                        args: vec![],
                    },
                },
            )]),
        }
    }

    #[test]
    fn redis_stream_task_events_extracts_ids_and_task_metadata_from_xreadgroup_response() {
        let value = redis::Value::Array(vec![redis::Value::Array(vec![
            redis::Value::BulkString(b"ojos:judge:task".to_vec()),
            redis::Value::Array(vec![
                redis::Value::Array(vec![
                    redis::Value::BulkString(b"1720000000000-0".to_vec()),
                    redis::Value::Array(vec![
                        redis::Value::BulkString(b"type".to_vec()),
                        redis::Value::BulkString(b"submission.created".to_vec()),
                        redis::Value::BulkString(b"task_id".to_vec()),
                        redis::Value::BulkString(b"sub-41".to_vec()),
                        redis::Value::BulkString(b"submission_id".to_vec()),
                        redis::Value::BulkString(b"41".to_vec()),
                        redis::Value::BulkString(b"traceparent".to_vec()),
                        redis::Value::BulkString(
                            b"00-0102030405060708090a0b0c0d0e0f10-1112131415161718-01".to_vec(),
                        ),
                    ]),
                ]),
                redis::Value::Array(vec![
                    redis::Value::BulkString(b"1720000000001-0".to_vec()),
                    redis::Value::Array(vec![
                        redis::Value::BulkString(b"task_id".to_vec()),
                        redis::Value::BulkString(b"sub-42".to_vec()),
                        redis::Value::BulkString(b"submission_id".to_vec()),
                        redis::Value::BulkString(b"42".to_vec()),
                    ]),
                ]),
            ]),
        ])]);

        let events = redis_stream_task_events(&value);

        assert_eq!(
            stream_event_ids(&events),
            vec!["1720000000000-0".to_string(), "1720000000001-0".to_string()]
        );
        assert_eq!(
            stream_task_ids(&events),
            vec!["sub-41".to_string(), "sub-42".to_string()]
        );
        assert_eq!(events[0].submission_id, Some(41));
        assert_eq!(events[1].submission_id, Some(42));
        assert_eq!(
            events[0].traceparent.as_deref(),
            Some("00-0102030405060708090a0b0c0d0e0f10-1112131415161718-01")
        );
        assert_eq!(
            traceparents_by_task_id(&events)
                .get("sub-41")
                .map(String::as_str),
            Some("00-0102030405060708090a0b0c0d0e0f10-1112131415161718-01")
        );
    }

    #[test]
    fn redis_stream_task_events_ignores_nil_or_malformed_values() {
        assert!(redis_stream_task_events(&redis::Value::Nil).is_empty());
        assert!(
            redis_stream_task_events(&redis::Value::Array(vec![redis::Value::BulkString(
                b"unexpected".to_vec()
            )]))
            .is_empty()
        );
    }

    #[test]
    fn worker_claim_request_serializes_stream_task_ids() {
        let req = WorkerClaimTasksReq {
            worker_id: "worker-a".to_string(),
            capabilities: vec!["nsjail".to_string()],
            supported_languages: vec!["rust".to_string()],
            available_slots: 1,
            task_ids: vec!["sub-42".to_string()],
        };

        let payload = serde_json::to_string(&req).expect("serialize claim request");

        assert!(
            payload.contains("\"task_ids\":[\"sub-42\"]"),
            "claim request should include stream task ids: {payload}"
        );
    }

    #[test]
    fn runner_policy_rejects_non_nsjail_mode() {
        let config = test_worker_config("process");

        let err = validate_runner_policy(&config).expect_err("non-nsjail runner must be refused");

        assert!(err.to_string().contains("unsupported runner mode"));
    }

    #[test]
    fn production_or_implicit_unmanaged_worker_cannot_fall_back_to_shared_credentials() {
        assert!(validate_deployment_mode(false, "production").is_err());
        assert!(validate_deployment_mode(false, "").is_err());
        assert!(validate_deployment_mode(false, "development").is_ok());
        assert!(validate_deployment_mode(true, "production").is_ok());
    }

    #[test]
    fn nsjail_runner_claims_sandbox_capabilities() {
        let _config = test_worker_config("nsjail");

        assert_eq!(
            worker_capabilities(),
            vec!["nsjail".to_string(), "cgroup-v2".to_string()]
        );
    }

    #[test]
    fn supported_languages_must_exist_in_languages_config() {
        let mut config = test_worker_config("nsjail");
        config.supported_languages = vec!["missing".to_string()];

        let err = validate_supported_languages(&config, &test_languages_config())
            .expect_err("unknown worker language must fail preflight");

        assert!(err.to_string().contains("not present in languages config"));
    }

    #[test]
    fn worker_storage_access_uses_api_id_resolver_url() {
        let get = internal_api_url(
            "http://gateway:8080/",
            "storage.object.get",
            "/submissions/42-source-main.cpp",
        );
        let put = internal_api_url(
            "http://gateway:8080",
            "storage.object.put",
            "submissions/42-result.json",
        );

        assert_eq!(
            get,
            "http://gateway:8080/internal/apis/storage.object.get/submissions/42-source-main.cpp"
        );
        assert_eq!(
            put,
            "http://gateway:8080/internal/apis/storage.object.put/submissions/42-result.json"
        );
    }

    #[tokio::test]
    async fn worker_http_client_uses_worker_token_and_runtime_routes() {
        let artifact_body = b"int main() { return 0; }\n".to_vec();
        let artifact_sha256 = format!("{:x}", Sha256::digest(&artifact_body));
        let captured = Arc::new(Mutex::new(Vec::<CapturedRequest>::new()));
        let endpoint = start_worker_http_contract_server(captured.clone(), artifact_body.clone());

        let config = WorkerLinkConfig {
            worker_id: "worker-a".to_string(),
            worker_name: "Worker A".to_string(),
            judge_api_url: endpoint,
            worker_token: "secret-worker-token".to_string(),
            max_concurrency: 2,
            work_dir: std::env::temp_dir().join(format!("ojos-worker-test-{}", Uuid::new_v4())),
            artifact_cache_dir: std::env::temp_dir()
                .join(format!("ojos-worker-cache-test-{}", Uuid::new_v4())),
            supported_languages: vec!["cpp17".to_string()],
            heartbeat_interval: Duration::from_secs(10),
            task_lease_ttl: Duration::from_secs(45),
            redis_url: None,
            redis_task_stream: "ojos:judge:task".to_string(),
            redis_consumer_group: "judge-worker".to_string(),
            internal_gateway_url: None,
            storage_api_get: "storage.object.get".to_string(),
            storage_api_put: "storage.object.put".to_string(),
            service_token: None,
            caller_node_id: None,
            runner_mode: "nsjail".to_string(),
            smoke_once: false,
            service_context: None,
        };
        let client = Client::builder()
            .timeout(Duration::from_secs(5))
            .no_proxy()
            .build()
            .expect("worker http client");

        register_worker(&client, &config)
            .await
            .expect("register worker");
        heartbeat_worker(&client, &config, 1)
            .await
            .expect("heartbeat worker");
        let tasks = claim_tasks(
            &client,
            &config,
            1,
            &[RedisTaskEvent {
                entry_id: "1720000000000-0".to_string(),
                task_id: Some("sub-42".to_string()),
                submission_id: Some(42),
                traceparent: Some(
                    "00-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-bbbbbbbbbbbbbbbb-01".to_string(),
                ),
            }],
        )
        .await
        .expect("claim tasks");
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].task_id, "sub-42");

        let artifact_target = config.work_dir.join("source.cpp");
        if let Some(parent) = artifact_target.parent() {
            fs::create_dir_all(parent).await.expect("artifact parent");
        }
        download_artifact(
            &client,
            &config,
            &WorkerArtifactRef {
                url: "/judge/worker/artifacts/submissions/42/source?task_id=sub-42&worker_id=worker-a&lease_version=7".to_string(),
                binding: None,
                api_id: None,
                relative_path: None,
                sha256: artifact_sha256,
                size_bytes: artifact_body.len() as i64,
            },
            &artifact_target,
            Some("00-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-bbbbbbbbbbbbbbbb-01"),
        )
        .await
        .expect("download artifact");
        let downloaded = fs::read(&artifact_target).await.expect("read artifact");
        assert_eq!(downloaded, artifact_body);

        let result_dir = config.work_dir.join("result");
        fs::create_dir_all(&result_dir).await.expect("result dir");
        let stdout_path = result_dir.join("stdout.txt");
        let stderr_path = result_dir.join("stderr.txt");
        let checker_path = result_dir.join("checker.log");
        fs::write(&stdout_path, "ok\n").await.expect("stdout");
        fs::write(&stderr_path, "").await.expect("stderr");
        fs::write(&checker_path, "matched\n")
            .await
            .expect("checker");

        let task = WorkerTaskLease {
            task_id: "sub-42".to_string(),
            submission_id: 42,
            language: "cpp17".to_string(),
            lease_version: 7,
            lease_expires_at: None,
            source: WorkerArtifactRef {
                url: "/unused/source".to_string(),
                binding: None,
                api_id: None,
                relative_path: None,
                sha256: String::new(),
                size_bytes: 0,
            },
            problem_package: WorkerArtifactRef {
                url: "/unused/package".to_string(),
                binding: None,
                api_id: None,
                relative_path: None,
                sha256: String::new(),
                size_bytes: 0,
            },
            traceparent: Some(
                "00-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-bbbbbbbbbbbbbbbb-01".to_string(),
            ),
        };
        submit_result(
            &client,
            &config,
            &task,
            &ResultFile {
                submission_id: 42,
                status: "ACCEPTED".to_string(),
                score: 100,
                time_ms: 12,
                memory_kb: 2048,
                message: "accepted".to_string(),
                cases: vec![ResultCase {
                    case_no: 1,
                    status: "ACCEPTED".to_string(),
                    score: 100,
                    time_ms: 12,
                    memory_kb: 2048,
                    stdout_path: stdout_path.to_string_lossy().to_string(),
                    stderr_path: stderr_path.to_string_lossy().to_string(),
                    checker_log_path: checker_path.to_string_lossy().to_string(),
                    message: "accepted".to_string(),
                }],
            },
        )
        .await
        .expect("submit result");
        fail_task_with_retry(&client, &config, &task, false, "SYSTEM", "nsjail failed")
            .await
            .expect("fail task");

        let requests = captured.lock().expect("captured requests").clone();
        let paths: Vec<_> = requests
            .iter()
            .map(|request| request.path.as_str())
            .collect();
        assert_eq!(
            paths,
            vec![
                "/judge/worker/register",
                "/judge/worker/heartbeat",
                "/judge/worker/tasks/claim",
                "/judge/worker/artifacts/submissions/42/source?task_id=sub-42&worker_id=worker-a&lease_version=7",
                "/judge/worker/tasks/sub-42/result",
                "/judge/worker/tasks/sub-42/fail",
            ]
        );
        for request in &requests {
            assert_eq!(
                request.header("x-ojos-worker-token"),
                Some("secret-worker-token"),
                "worker request {} must carry worker token",
                request.path
            );
        }
        assert!(requests[0].body.contains("\"worker_id\":\"worker-a\""));
        assert!(requests[2].body.contains("\"task_ids\":[\"sub-42\"]"));
        assert!(requests[4].body.contains("\"stdout\":\"ok\\n\""));
        assert!(requests[4].body.contains("\"checker_log\":\"matched\\n\""));
        assert!(requests[5].body.contains("\"message\":\"nsjail failed\""));
        assert_eq!(
            requests[3].header("traceparent"),
            Some("00-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-bbbbbbbbbbbbbbbb-01")
        );
        assert_eq!(
            requests[4].header("traceparent"),
            Some("00-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-bbbbbbbbbbbbbbbb-01")
        );
        assert_eq!(
            requests[5].header("traceparent"),
            Some("00-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-bbbbbbbbbbbbbbbb-01")
        );

        let _ = fs::remove_dir_all(&config.work_dir).await;
        let _ = fs::remove_dir_all(&config.artifact_cache_dir).await;
    }

    #[tokio::test]
    async fn worker_downloads_storage_source_through_internal_gateway_identity() {
        let artifact_body = b"int main() { return 0; }\n".to_vec();
        let artifact_sha256 = format!("{:x}", Sha256::digest(&artifact_body));
        let captured = Arc::new(Mutex::new(Vec::<CapturedRequest>::new()));
        let endpoint = start_gateway_artifact_server(captured.clone(), artifact_body.clone());
        let config = WorkerLinkConfig {
            worker_id: "127.0.0.2_19000_judge-worker".to_string(),
            worker_name: "worker".to_string(),
            judge_api_url: "http://judge-api:8082".to_string(),
            worker_token: "worker-token".to_string(),
            max_concurrency: 1,
            work_dir: std::env::temp_dir().join(format!("ojos-worker-test-{}", Uuid::new_v4())),
            artifact_cache_dir: std::env::temp_dir()
                .join(format!("ojos-worker-cache-test-{}", Uuid::new_v4())),
            supported_languages: vec!["cpp17".to_string()],
            heartbeat_interval: Duration::from_secs(10),
            task_lease_ttl: Duration::from_secs(45),
            redis_url: None,
            redis_task_stream: "ojos:judge:task".to_string(),
            redis_consumer_group: "judge-worker".to_string(),
            internal_gateway_url: Some(endpoint),
            storage_api_get: "storage.object.get".to_string(),
            storage_api_put: "storage.object.put".to_string(),
            service_token: Some("internal-token".to_string()),
            caller_node_id: Some("child-node".to_string()),
            runner_mode: "nsjail".to_string(),
            smoke_once: false,
            service_context: None,
        };
        let client = Client::builder()
            .timeout(Duration::from_secs(5))
            .no_proxy()
            .build()
            .expect("worker http client");
        let target = config.work_dir.join("source.cpp");
        fs::create_dir_all(target.parent().unwrap())
            .await
            .expect("target parent");

        download_artifact(
            &client,
            &config,
            &WorkerArtifactRef {
                url: "/internal/apis/storage.object.get/submissions/42-source-main.cpp".to_string(),
                binding: None,
                api_id: None,
                relative_path: None,
                sha256: artifact_sha256,
                size_bytes: artifact_body.len() as i64,
            },
            &target,
            Some("00-cccccccccccccccccccccccccccccccc-dddddddddddddddd-01"),
        )
        .await
        .expect("download via gateway");

        let requests = captured.lock().expect("captured requests").clone();
        assert_eq!(requests.len(), 1);
        assert_eq!(
            requests[0].path,
            "/internal/apis/storage.object.get/submissions/42-source-main.cpp"
        );
        assert_eq!(
            requests[0].header("x-ojos-caller-service"),
            Some("judge-worker")
        );
        assert_eq!(requests[0].header("x-ojos-node-id"), Some("child-node"));
        assert_eq!(
            requests[0].header("authorization"),
            Some("Bearer internal-token")
        );
        assert_eq!(
            requests[0].header("x-ojos-worker-token"),
            Some("worker-token")
        );
        assert_eq!(
            requests[0].header("traceparent"),
            Some("00-cccccccccccccccccccccccccccccccc-dddddddddddddddd-01")
        );

        let downloaded = fs::read(&target).await.expect("read downloaded source");
        assert_eq!(downloaded, artifact_body);
        let _ = fs::remove_dir_all(&config.work_dir).await;
        let _ = fs::remove_dir_all(&config.artifact_cache_dir).await;
    }

    #[tokio::test]
    async fn managed_worker_uses_named_bindings_rotated_token_and_sdk_resource_download() {
        let artifact_body = b"managed source\n".to_vec();
        let artifact_sha256 = format!("sha256:{:x}", Sha256::digest(&artifact_body));
        let captured = Arc::new(Mutex::new(Vec::new()));
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind managed gateway");
        let origin = format!(
            "http://{}",
            listener.local_addr().expect("managed gateway addr")
        );
        let server_captured = captured.clone();
        let server_artifact = artifact_body.clone();
        std::thread::spawn(move || {
            for _ in 0..2 {
                let (mut stream, _) = listener.accept().expect("accept managed request");
                let request = read_http_request(&mut stream);
                let response = if request.path
                    == "/internal/apis/storage.object.get/submissions/42-source-main.cpp"
                {
                    http_response("200 OK", "text/plain", &server_artifact)
                } else {
                    http_response("200 OK", "application/json", b"{}")
                };
                server_captured
                    .lock()
                    .expect("managed captured requests")
                    .push(request);
                stream.write_all(&response).expect("managed response");
            }
        });

        let root = std::env::temp_dir().join(format!("ojos-worker-managed-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&root).expect("managed root");
        let token_path = root.join("token");
        std::fs::write(&token_path, "rotated-deployment-token").expect("managed token");
        let context: ServiceContext = serde_json::from_value(serde_json::json!({
            "schema_version": 1,
            "deployment": {"id": "worker-b", "service": "judge-worker", "node": "node-b"},
            "gateway": {"origin": origin},
            "bindings": {
                "judge_control": {"binding_id": "control", "api_id": "judge.worker.control", "base_path": "/internal/apis/judge.worker.control", "timeout_ms": 35000},
                "storage_get": {"binding_id": "get", "api_id": "storage.object.get", "base_path": "/internal/apis/storage.object.get", "timeout_ms": 300000}
            },
            "credential_file": token_path,
            "generation": 3
        }))
        .expect("managed context");
        context.validate().expect("valid managed context");
        let mut config = test_worker_config("nsjail");
        config.judge_api_url = context
            .binding_url("judge_control", "")
            .expect("control binding URL");
        config.worker_token.clear();
        config.service_context = Some(context.clone());
        let client = context.client().expect("managed client");

        for forbidden_url in [
            "https://attacker.invalid/source.cpp",
            "/internal/apis/storage.object.get/submissions/42-source-main.cpp",
        ] {
            let error = download_artifact(
                &client,
                &config,
                &WorkerArtifactRef {
                    url: forbidden_url.to_string(),
                    binding: Some("storage_get".to_string()),
                    api_id: Some("storage.object.get".to_string()),
                    relative_path: Some("/submissions/42-source-main.cpp".to_string()),
                    sha256: artifact_sha256.clone(),
                    size_bytes: artifact_body.len() as i64,
                },
                &root.join("forbidden-source.cpp"),
                None,
            )
            .await
            .expect_err("managed worker must reject every legacy URL form");
            assert!(error.to_string().contains("must not contain a legacy URL"));
        }
        let wrong_binding_error = download_artifact(
            &client,
            &config,
            &WorkerArtifactRef {
                url: String::new(),
                binding: Some("judge_control".to_string()),
                api_id: Some("judge.worker.control".to_string()),
                relative_path: Some("/source.cpp".to_string()),
                sha256: artifact_sha256.clone(),
                size_bytes: artifact_body.len() as i64,
            },
            &root.join("wrong-binding-source.cpp"),
            None,
        )
        .await
        .expect_err("a valid non-storage binding must not become an artifact source");
        assert!(
            wrong_binding_error
                .to_string()
                .contains("require binding storage_get")
        );

        let _: serde_json::Value = post_json(
            &client,
            &config,
            "/judge/worker/register",
            &serde_json::json!({"worker_id": "worker-b"}),
        )
        .await
        .expect("managed control call");
        let target = root.join("source.cpp");
        download_artifact(
            &client,
            &config,
            &WorkerArtifactRef {
                url: String::new(),
                binding: Some("storage_get".to_string()),
                api_id: Some("storage.object.get".to_string()),
                relative_path: Some("/submissions/42-source-main.cpp".to_string()),
                sha256: artifact_sha256,
                size_bytes: artifact_body.len() as i64,
            },
            &target,
            None,
        )
        .await
        .expect("managed SDK download");

        let requests = captured.lock().expect("managed requests").clone();
        assert_eq!(requests.len(), 2);
        assert_eq!(
            requests[0].path,
            "/internal/apis/judge.worker.control/register"
        );
        assert!(requests[0].header("idempotency-key").is_some());
        assert_eq!(
            requests[1].path,
            "/internal/apis/storage.object.get/submissions/42-source-main.cpp"
        );
        for request in &requests {
            assert_eq!(
                request.header("authorization"),
                Some("Bearer rotated-deployment-token")
            );
            assert_eq!(request.header("x-ojos-caller-service"), None);
            assert_eq!(request.header("x-ojos-node-id"), None);
            assert_eq!(request.header("x-ojos-worker-token"), None);
        }
        assert_eq!(
            fs::read(&target).await.expect("managed artifact"),
            artifact_body
        );
        let _ = fs::remove_dir_all(&root).await;
    }

    #[tokio::test]
    async fn claimed_task_heartbeats_during_download_longer_than_lease_and_reports_failure() {
        let captured = Arc::new(Mutex::new(Vec::<CapturedRequest>::new()));
        let (control_origin, stop_control, control_thread) =
            start_responsive_control_server(captured.clone(), true, Duration::ZERO, 0, 0);
        let source = b"int main() { return 0; }\n".to_vec();
        let source_sha256 = format!("{:x}", Sha256::digest(&source));
        let (source_origin, source_started, source_thread) =
            start_delayed_artifact_server(source.clone(), Duration::from_millis(700));

        let mut config = test_worker_config("nsjail");
        config.judge_api_url = control_origin;
        config.task_lease_ttl = Duration::from_millis(300);
        let work_dir = config.work_dir.clone();
        let task = WorkerTaskLease {
            task_id: "sub-slow-download".to_string(),
            submission_id: 101,
            language: "cpp17".to_string(),
            lease_version: 3,
            lease_expires_at: None,
            source: WorkerArtifactRef {
                url: format!("{source_origin}/source.cpp"),
                binding: None,
                api_id: None,
                relative_path: None,
                sha256: source_sha256,
                size_bytes: source.len() as i64,
            },
            // This deliberately fails after the slow source download.  The
            // failure must be reported explicitly rather than waiting for lease
            // expiry and allowing another worker to execute the same task.
            problem_package: WorkerArtifactRef {
                url: String::new(),
                binding: None,
                api_id: None,
                relative_path: None,
                sha256: String::new(),
                size_bytes: 0,
            },
            traceparent: None,
        };
        let client = Client::builder()
            .timeout(Duration::from_secs(5))
            .no_proxy()
            .build()
            .expect("worker client");

        let error = execute_task_inner(
            client,
            Arc::new(config),
            Arc::new(test_languages_config()),
            task,
        )
        .await
        .expect_err("invalid package reference must fail the claimed task");
        assert!(
            format!("{error:#}").contains("artifact size must be positive"),
            "unexpected execution error: {error:#}"
        );
        assert!(source_started.load(Ordering::SeqCst));

        stop_control.store(true, Ordering::SeqCst);
        control_thread.join().expect("join control server");
        source_thread.join().expect("join source server");
        let requests = captured.lock().expect("captured requests").clone();
        let heartbeat_count = requests
            .iter()
            .filter(|request| request.path.ends_with("/heartbeat"))
            .count();
        assert!(
            heartbeat_count >= 3,
            "slow download should span multiple lease heartbeats, got {heartbeat_count}: {requests:#?}"
        );
        let failure = requests
            .iter()
            .find(|request| request.path.ends_with("/fail"))
            .expect("preparation failure must call the formal fail API");
        assert!(failure.body.contains("\"retryable\":false"));
        assert!(failure.body.contains("\"error_type\":\"INVALID_TASK\""));
        assert!(failure.header("idempotency-key").is_some());
        let _ = fs::remove_dir_all(&work_dir).await;
    }

    #[tokio::test]
    async fn lease_heartbeat_failure_cancels_inflight_download_without_stale_report() {
        let captured = Arc::new(Mutex::new(Vec::<CapturedRequest>::new()));
        let (control_origin, stop_control, control_thread) = start_responsive_control_server(
            captured.clone(),
            false,
            Duration::from_millis(150),
            0,
            0,
        );
        let source = b"slow artifact".to_vec();
        let source_sha256 = format!("{:x}", Sha256::digest(&source));
        let (source_origin, source_started, source_thread) =
            start_delayed_artifact_server(source.clone(), Duration::from_millis(800));

        let mut config = test_worker_config("nsjail");
        config.judge_api_url = control_origin;
        config.task_lease_ttl = Duration::from_millis(300);
        let work_dir = config.work_dir.clone();
        let task = WorkerTaskLease {
            task_id: "sub-lost-lease".to_string(),
            submission_id: 102,
            language: "cpp17".to_string(),
            lease_version: 5,
            lease_expires_at: None,
            source: WorkerArtifactRef {
                url: format!("{source_origin}/source.cpp"),
                binding: None,
                api_id: None,
                relative_path: None,
                sha256: source_sha256,
                size_bytes: source.len() as i64,
            },
            problem_package: WorkerArtifactRef {
                url: String::new(),
                binding: None,
                api_id: None,
                relative_path: None,
                sha256: String::new(),
                size_bytes: 0,
            },
            traceparent: None,
        };
        let client = Client::builder()
            .timeout(Duration::from_secs(5))
            .no_proxy()
            .build()
            .expect("worker client");
        let started_at = std::time::Instant::now();

        let error = execute_task_inner(
            client,
            Arc::new(config),
            Arc::new(test_languages_config()),
            task,
        )
        .await
        .expect_err("rejected heartbeat must cancel task work");
        assert!(error.to_string().contains("task lease heartbeat failed"));
        assert!(
            started_at.elapsed() < Duration::from_millis(600),
            "task continued after the lease was lost"
        );
        assert!(source_started.load(Ordering::SeqCst));

        stop_control.store(true, Ordering::SeqCst);
        control_thread.join().expect("join control server");
        source_thread.join().expect("join source server");
        let requests = captured.lock().expect("captured requests").clone();
        assert!(
            requests
                .iter()
                .all(|request| !request.path.ends_with("/fail")
                    && !request.path.ends_with("/result")),
            "a worker must not report through a rejected lease: {requests:#?}"
        );
        let _ = fs::remove_dir_all(&work_dir).await;
    }

    #[tokio::test]
    async fn heartbeat_stop_cancels_before_any_http_side_effect() {
        let mut config = test_worker_config("nsjail");
        config.judge_api_url = "http://127.0.0.1:9".to_string();
        config.task_lease_ttl = Duration::from_millis(300);
        let task = test_task_lease("sub-cancel-heartbeat", 1);
        let client = Client::builder()
            .timeout(Duration::from_secs(1))
            .no_proxy()
            .build()
            .expect("worker client");
        let (stop, receiver) = tokio::sync::watch::channel(false);
        stop.send(true).expect("send heartbeat stop");

        tokio::time::timeout(
            Duration::from_millis(100),
            lease_heartbeat_loop(client, Arc::new(config), task, receiver),
        )
        .await
        .expect("heartbeat cancellation must be prompt")
        .expect("heartbeat cancellation must be clean");
    }

    #[tokio::test]
    async fn heartbeat_cadence_tracks_server_expiry_when_local_ttl_is_larger() {
        let captured = Arc::new(Mutex::new(Vec::<CapturedRequest>::new()));
        let (origin, stop_server, server_thread) =
            start_responsive_control_server(captured.clone(), true, Duration::ZERO, 0, 0);
        let mut config = test_worker_config("nsjail");
        config.judge_api_url = origin;
        config.task_lease_ttl = Duration::from_secs(30);
        let mut task = test_task_lease("sub-server-short-ttl", 2);
        task.lease_expires_at =
            Some((chrono::Utc::now() + chrono::Duration::seconds(30)).to_rfc3339());
        let client = Client::builder()
            .timeout(Duration::from_secs(1))
            .no_proxy()
            .build()
            .expect("worker client");
        let (stop, receiver) = tokio::sync::watch::channel(false);
        let heartbeat = tokio::spawn(lease_heartbeat_loop(
            client,
            Arc::new(config),
            task,
            receiver,
        ));

        tokio::time::sleep(Duration::from_millis(450)).await;
        stop.send(true).expect("stop heartbeat");
        heartbeat
            .await
            .expect("join heartbeat")
            .expect("heartbeat loop");
        stop_server.store(true, Ordering::SeqCst);
        server_thread.join().expect("join short TTL server");

        let heartbeat_count = captured
            .lock()
            .expect("captured requests")
            .iter()
            .filter(|request| request.path.ends_with("/heartbeat"))
            .count();
        assert!(
            heartbeat_count >= 3,
            "server 300ms lease must override local 30s cadence; got {heartbeat_count}"
        );
    }

    #[tokio::test]
    async fn terminal_ack_wins_over_rejected_inflight_heartbeat() {
        let (stop, _receiver) = tokio::sync::watch::channel(false);
        let mut heartbeat = tokio::spawn(async {
            tokio::time::sleep(Duration::from_millis(10)).await;
            Err(anyhow!(
                "judge-api returned 409 after task entered terminal state"
            ))
        });

        stop_lease_heartbeat(stop, &mut heartbeat, true, "sub-terminal-race")
            .await
            .expect("terminal acknowledgement must be authoritative");
    }

    #[tokio::test]
    async fn fail_report_retries_with_one_stable_idempotency_key() {
        let captured = Arc::new(Mutex::new(Vec::<CapturedRequest>::new()));
        let (origin, stop_server, server_thread) =
            start_responsive_control_server(captured.clone(), true, Duration::ZERO, 3, 0);
        let mut config = test_worker_config("nsjail");
        config.judge_api_url = origin;
        let task = test_task_lease("sub-retry-fail", 8);
        let client = Client::builder()
            .timeout(Duration::from_secs(2))
            .no_proxy()
            .build()
            .expect("worker client");

        fail_task_with_retry(
            &client,
            &config,
            &task,
            true,
            "ARTIFACT_DOWNLOAD",
            "temporary gateway failure",
        )
        .await
        .expect("second fail report attempt should succeed");

        stop_server.store(true, Ordering::SeqCst);
        server_thread.join().expect("join retry server");
        let requests: Vec<_> = captured
            .lock()
            .expect("captured requests")
            .iter()
            .filter(|request| request.path.ends_with("/fail"))
            .cloned()
            .collect();
        assert_eq!(requests.len(), 4);
        let first_key = requests[0]
            .header("idempotency-key")
            .expect("first retry key");
        assert!(first_key.starts_with("judge-fail-"));
        assert!(
            requests
                .iter()
                .all(|request| request.header("idempotency-key") == Some(first_key))
        );
    }

    #[tokio::test]
    async fn result_report_retries_with_payload_bound_idempotency_key() {
        let captured = Arc::new(Mutex::new(Vec::<CapturedRequest>::new()));
        let (origin, stop_server, server_thread) =
            start_responsive_control_server(captured.clone(), true, Duration::ZERO, 0, 3);
        let mut config = test_worker_config("nsjail");
        config.judge_api_url = origin;
        let task = test_task_lease("sub-retry-result", 9);
        let result_dir = config.work_dir.join("result-retry");
        fs::create_dir_all(&result_dir).await.expect("result dir");
        let stdout_path = result_dir.join("stdout.txt");
        let stderr_path = result_dir.join("stderr.txt");
        let checker_path = result_dir.join("checker.log");
        fs::write(&stdout_path, "ok\n").await.expect("stdout");
        fs::write(&stderr_path, "").await.expect("stderr");
        fs::write(&checker_path, "matched\n")
            .await
            .expect("checker");
        let result = ResultFile {
            submission_id: 1,
            status: "ACCEPTED".to_string(),
            score: 100,
            time_ms: 10,
            memory_kb: 1024,
            message: "accepted".to_string(),
            cases: vec![ResultCase {
                case_no: 1,
                status: "ACCEPTED".to_string(),
                score: 100,
                time_ms: 10,
                memory_kb: 1024,
                stdout_path: stdout_path.to_string_lossy().to_string(),
                stderr_path: stderr_path.to_string_lossy().to_string(),
                checker_log_path: checker_path.to_string_lossy().to_string(),
                message: String::new(),
            }],
        };
        let client = Client::builder()
            .timeout(Duration::from_secs(2))
            .no_proxy()
            .build()
            .expect("worker client");

        submit_result(&client, &config, &task, &result)
            .await
            .expect("second result report attempt should replay the receipt");

        stop_server.store(true, Ordering::SeqCst);
        server_thread.join().expect("join result retry server");
        let requests: Vec<_> = captured
            .lock()
            .expect("captured requests")
            .iter()
            .filter(|request| request.path.ends_with("/result"))
            .cloned()
            .collect();
        assert_eq!(requests.len(), 4);
        let first_key = requests[0]
            .header("idempotency-key")
            .expect("first result retry key");
        assert!(first_key.starts_with("judge-result-"));
        assert!(
            requests
                .iter()
                .all(|request| request.header("idempotency-key") == Some(first_key))
        );
        let _ = fs::remove_dir_all(&config.work_dir).await;
    }

    #[test]
    fn terminal_report_retry_uses_bounded_load_backoff() {
        assert_eq!(terminal_report_retry_delay(0), Duration::from_millis(100));
        assert_eq!(terminal_report_retry_delay(1), Duration::from_secs(1));
        assert_eq!(terminal_report_retry_delay(2), Duration::from_secs(5));
        assert_eq!(terminal_report_retry_delay(3), Duration::from_secs(30));
        assert_eq!(terminal_report_retry_delay(20), Duration::from_secs(30));
    }

    fn test_task_lease(task_id: &str, lease_version: i32) -> WorkerTaskLease {
        WorkerTaskLease {
            task_id: task_id.to_string(),
            submission_id: 1,
            language: "cpp17".to_string(),
            lease_version,
            lease_expires_at: None,
            source: WorkerArtifactRef {
                url: String::new(),
                binding: None,
                api_id: None,
                relative_path: None,
                sha256: String::new(),
                size_bytes: 0,
            },
            problem_package: WorkerArtifactRef {
                url: String::new(),
                binding: None,
                api_id: None,
                relative_path: None,
                sha256: String::new(),
                size_bytes: 0,
            },
            traceparent: None,
        }
    }

    #[derive(Clone, Debug)]
    struct CapturedRequest {
        path: String,
        headers: Vec<(String, String)>,
        body: String,
    }

    impl CapturedRequest {
        fn header(&self, name: &str) -> Option<&str> {
            self.headers
                .iter()
                .find(|(key, _)| key.eq_ignore_ascii_case(name))
                .map(|(_, value)| value.as_str())
        }
    }

    fn start_responsive_control_server(
        captured: Arc<Mutex<Vec<CapturedRequest>>>,
        heartbeat_success: bool,
        heartbeat_delay: Duration,
        fail_responses_before_success: usize,
        result_responses_before_success: usize,
    ) -> (String, Arc<AtomicBool>, std::thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind responsive control server");
        listener
            .set_nonblocking(true)
            .expect("set responsive server nonblocking");
        let addr = listener.local_addr().expect("responsive control addr");
        let stop = Arc::new(AtomicBool::new(false));
        let server_stop = stop.clone();
        let fail_attempts = Arc::new(AtomicUsize::new(0));
        let server_fail_attempts = fail_attempts.clone();
        let result_attempts = Arc::new(AtomicUsize::new(0));
        let server_result_attempts = result_attempts.clone();
        let handle = std::thread::spawn(move || {
            while !server_stop.load(Ordering::SeqCst) {
                let (mut stream, _) = match listener.accept() {
                    Ok(connection) => connection,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(2));
                        continue;
                    }
                    Err(error) => panic!("accept responsive control request: {error}"),
                };
                // On Windows an accepted socket may inherit the listener's
                // nonblocking flag. The request helper expects a blocking
                // stream with a finite read timeout.
                stream
                    .set_nonblocking(false)
                    .expect("set responsive stream blocking");
                let request = read_http_request(&mut stream);
                let response = if request.path.ends_with("/heartbeat") {
                    std::thread::sleep(heartbeat_delay);
                    if heartbeat_success {
                        let lease_expires_at =
                            (chrono::Utc::now() + chrono::Duration::milliseconds(300)).to_rfc3339();
                        http_response(
                            "200 OK",
                            "application/json",
                            format!(r#"{{"lease_expires_at":"{lease_expires_at}"}}"#).as_bytes(),
                        )
                    } else {
                        http_response(
                            "409 Conflict",
                            "application/problem+json",
                            br#"{"title":"task lease is invalid"}"#,
                        )
                    }
                } else if request.path.ends_with("/fail") {
                    let attempt = server_fail_attempts.fetch_add(1, Ordering::SeqCst);
                    if attempt < fail_responses_before_success {
                        http_response(
                            "503 Service Unavailable",
                            "application/problem+json",
                            br#"{"title":"temporarily unavailable"}"#,
                        )
                    } else {
                        http_response(
                            "200 OK",
                            "application/json",
                            br#"{"accepted":true,"status":"PENDING"}"#,
                        )
                    }
                } else if request.path.ends_with("/result") {
                    let attempt = server_result_attempts.fetch_add(1, Ordering::SeqCst);
                    if attempt < result_responses_before_success {
                        http_response(
                            "503 Service Unavailable",
                            "application/problem+json",
                            br#"{"title":"temporarily unavailable"}"#,
                        )
                    } else {
                        http_response(
                            "200 OK",
                            "application/json",
                            br#"{"accepted":true,"status":"ACCEPTED"}"#,
                        )
                    }
                } else {
                    http_response(
                        "404 Not Found",
                        "application/problem+json",
                        br#"{"title":"not found"}"#,
                    )
                };
                captured
                    .lock()
                    .expect("responsive captured requests")
                    .push(request);
                let _ = stream.write_all(&response);
            }
        });
        (format!("http://{addr}"), stop, handle)
    }

    fn start_delayed_artifact_server(
        body: Vec<u8>,
        delay: Duration,
    ) -> (String, Arc<AtomicBool>, std::thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind delayed artifact server");
        let addr = listener.local_addr().expect("delayed artifact addr");
        let started = Arc::new(AtomicBool::new(false));
        let server_started = started.clone();
        let handle = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept delayed artifact request");
            server_started.store(true, Ordering::SeqCst);
            let _request = read_http_request(&mut stream);
            std::thread::sleep(delay);
            let _ = stream.write_all(&http_response("200 OK", "application/octet-stream", &body));
        });
        (format!("http://{addr}"), started, handle)
    }

    fn start_worker_http_contract_server(
        captured: Arc<Mutex<Vec<CapturedRequest>>>,
        artifact_body: Vec<u8>,
    ) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind worker contract server");
        let addr = listener.local_addr().expect("worker contract addr");
        std::thread::spawn(move || {
            for _ in 0..6 {
                let (mut stream, _) = listener.accept().expect("accept worker request");
                let request = read_http_request(&mut stream);
                let body = response_for_worker_request(&request.path, &artifact_body);
                captured.lock().expect("captured requests").push(request);
                stream
                    .write_all(&body)
                    .expect("write worker contract response");
            }
        });
        format!("http://{}", addr)
    }

    fn start_gateway_artifact_server(
        captured: Arc<Mutex<Vec<CapturedRequest>>>,
        artifact_body: Vec<u8>,
    ) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind gateway artifact server");
        let addr = listener.local_addr().expect("gateway artifact addr");
        std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept gateway request");
            let request = read_http_request(&mut stream);
            captured.lock().expect("captured requests").push(request);
            stream
                .write_all(&http_response(
                    "200 OK",
                    "text/plain; charset=utf-8",
                    &artifact_body,
                ))
                .expect("write gateway response");
        });
        format!("http://{}", addr)
    }

    fn response_for_worker_request(path: &str, artifact_body: &[u8]) -> Vec<u8> {
        if path.starts_with("/judge/worker/artifacts/submissions/42/source") {
            return http_response("200 OK", "text/plain; charset=utf-8", artifact_body);
        }
        let json = match path {
            "/judge/worker/register" => {
                r#"{"worker_id":"worker-a","heartbeat_every_s":10,"lease_ttl_seconds":45,"status":"ONLINE"}"#
            }
            "/judge/worker/heartbeat" => r#"{}"#,
            "/judge/worker/tasks/claim" => {
                r#"{"tasks":[{"task_id":"sub-42","submission_id":42,"problem_id":7,"language":"cpp17","attempt":1,"lease_version":7,"lease_expires_at":"2026-07-01T00:00:00Z","source":{"url":"/unused/source","sha256":"","size_bytes":0,"content_type":"text/plain"},"problem_package":{"url":"/unused/package","sha256":"","size_bytes":0,"content_type":"application/zip"}}]}"#
            }
            "/judge/worker/tasks/sub-42/result" => r#"{"accepted":true,"status":"ACCEPTED"}"#,
            "/judge/worker/tasks/sub-42/fail" => r#"{"accepted":true,"status":"SYSTEM_ERROR"}"#,
            _ => r#"{"error":"not found"}"#,
        };
        http_response("200 OK", "application/json", json.as_bytes())
    }

    fn http_response(status: &str, content_type: &str, body: &[u8]) -> Vec<u8> {
        let mut response = format!(
            "HTTP/1.1 {status}\r\nContent-Length: {}\r\nContent-Type: {content_type}\r\nConnection: close\r\n\r\n",
            body.len()
        )
        .into_bytes();
        response.extend_from_slice(body);
        response
    }

    fn read_http_request(stream: &mut std::net::TcpStream) -> CapturedRequest {
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .expect("request read timeout");
        let mut bytes = Vec::new();
        let mut buffer = [0_u8; 1024];
        loop {
            let count = stream.read(&mut buffer).expect("read request");
            if count == 0 {
                break;
            }
            bytes.extend_from_slice(&buffer[..count]);
            if http_request_complete(&bytes) {
                break;
            }
        }
        parse_http_request(&bytes)
    }

    fn http_request_complete(bytes: &[u8]) -> bool {
        let Some(header_end) = bytes.windows(4).position(|window| window == b"\r\n\r\n") else {
            return false;
        };
        let header = String::from_utf8_lossy(&bytes[..header_end]);
        let content_length = header
            .lines()
            .find_map(|line| {
                line.to_ascii_lowercase()
                    .strip_prefix("content-length:")?
                    .trim()
                    .parse::<usize>()
                    .ok()
            })
            .unwrap_or(0);
        bytes.len() >= header_end + 4 + content_length
    }

    fn parse_http_request(bytes: &[u8]) -> CapturedRequest {
        let text = String::from_utf8_lossy(bytes);
        let (head, body) = text.split_once("\r\n\r\n").unwrap_or((text.as_ref(), ""));
        let mut lines = head.lines();
        let request_line = lines.next().unwrap_or_default();
        let path = request_line
            .split_whitespace()
            .nth(1)
            .unwrap_or_default()
            .to_string();
        let headers = lines
            .filter_map(|line| {
                let (key, value) = line.split_once(':')?;
                Some((key.trim().to_string(), value.trim().to_string()))
            })
            .collect();
        CapturedRequest {
            path,
            headers,
            body: body.to_string(),
        }
    }
}
