use anyhow::{Context, Result, anyhow};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tokio::fs;
use tokio::io::AsyncWriteExt;
use tokio::sync::Semaphore;
use tracing::{error, info, warn};
use uuid::Uuid;

use crate::config::LanguagesConfig;
use crate::judge::judge_artifacts;
use crate::result::ResultFile;

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
}

impl WorkerLinkConfig {
    pub fn from_env(languages: &LanguagesConfig) -> Result<Self> {
        let worker_id = env_or("OJOS_WORKER_ID", || {
            std::env::var("HOSTNAME").unwrap_or_else(|_| format!("worker-{}", Uuid::new_v4()))
        });
        let worker_name = env_or("OJOS_WORKER_NAME", || worker_id.clone());
        let judge_api_url = required_env("OJOS_JUDGE_API_URL")?
            .trim_end_matches('/')
            .to_string();
        let worker_token = required_env("OJOS_WORKER_TOKEN")?;
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
        let redis_url = std::env::var("OJOS_REDIS_URL")
            .ok()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty());
        let redis_task_stream = env_or("OJOS_JUDGE_TASK_STREAM", || {
            "ojos:judge:submissions".to_string()
        });
        let redis_consumer_group =
            env_or("OJOS_JUDGE_CONSUMER_GROUP", || "judge-workers".to_string());
        let internal_gateway_url = std::env::var("OJOS_INTERNAL_GATEWAY_URL")
            .ok()
            .map(|value| value.trim().trim_end_matches('/').to_string())
            .filter(|value| !value.is_empty());
        let storage_api_get = env_or("OJOS_STORAGE_OBJECT_GET_API_ID", || {
            "storage.object.get".to_string()
        });
        let storage_api_put = env_or("OJOS_STORAGE_OBJECT_PUT_API_ID", || {
            "storage.object.put".to_string()
        });

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
        })
    }
}

fn internal_api_url(gateway_url: &str, api_id: &str, path: &str) -> String {
    format!(
        "{}/internal/apis/{}/{}",
        gateway_url.trim_end_matches('/'),
        api_id.trim_matches('/'),
        path.trim_start_matches('/')
    )
}

pub async fn run_worker_link(languages: Arc<LanguagesConfig>) -> Result<()> {
    let config = Arc::new(WorkerLinkConfig::from_env(&languages)?);
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

    let client = Client::builder()
        .timeout(Duration::from_secs(60))
        .build()
        .context("create worker http client failed")?;

    register_worker(&client, &config).await?;
    let mut stream_wakeup = RedisTaskWakeup::from_config(&config).await;

    let semaphore = Arc::new(Semaphore::new(config.max_concurrency));
    {
        let client = client.clone();
        let config = config.clone();
        let semaphore = semaphore.clone();
        tokio::spawn(async move {
            loop {
                let running = config
                    .max_concurrency
                    .saturating_sub(semaphore.available_permits());
                if let Err(err) = heartbeat_worker(&client, &config, running).await {
                    warn!(error = %err, "worker heartbeat failed");
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

        let tasks = claim_tasks(&client, &config, available, &pending_task_events).await?;
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

async fn execute_task(
    client: Client,
    config: Arc<WorkerLinkConfig>,
    languages: Arc<LanguagesConfig>,
    task: WorkerTaskLease,
) -> Result<()> {
    info!(
        task_id = %task.task_id,
        submission_id = task.submission_id,
        language = %task.language,
        "claimed worker task"
    );

    let task_dir = config
        .work_dir
        .join(format!("{}-{}", task.submission_id, task.lease_version));
    let source_path = task_dir
        .join("source")
        .join(source_file_name(&languages, &task.language));
    let package_zip = task_dir.join("problem.zip");
    let package_dir = task_dir.join("problem");
    let result_dir = task_dir.join("result");

    if task_dir.exists() {
        let _ = fs::remove_dir_all(&task_dir).await;
    }
    fs::create_dir_all(source_path.parent().unwrap_or(&task_dir)).await?;
    fs::create_dir_all(&package_dir).await?;
    fs::create_dir_all(&result_dir).await?;

    download_artifact(&client, &config, &task.source, &source_path).await?;
    download_artifact(&client, &config, &task.problem_package, &package_zip).await?;
    unzip_safe(&package_zip, &package_dir)?;

    let heartbeat_stop = tokio::sync::watch::channel(false);
    let heartbeat_rx = heartbeat_stop.1;
    let heartbeat_client = client.clone();
    let heartbeat_config = config.clone();
    let heartbeat_task = task.clone();
    let heartbeat_handle = tokio::spawn(async move {
        lease_heartbeat_loop(
            heartbeat_client,
            heartbeat_config,
            heartbeat_task,
            heartbeat_rx,
        )
        .await;
    });

    let result = judge_artifacts(
        languages,
        task.submission_id,
        &task.language,
        &source_path,
        &package_dir,
        &result_dir,
    )
    .await;

    let _ = heartbeat_stop.0.send(true);
    let _ = heartbeat_handle.await;

    match result {
        Ok(result) => submit_result(&client, &config, &task, &result).await?,
        Err(err) => {
            fail_task(&client, &config, &task, false, "SYSTEM", &err.to_string()).await?;
            return Err(err);
        }
    }

    let _ = fs::remove_dir_all(&task_dir).await;
    Ok(())
}

async fn register_worker(client: &Client, config: &WorkerLinkConfig) -> Result<()> {
    let req = WorkerRegisterReq {
        worker_id: config.worker_id.clone(),
        worker_name: config.worker_name.clone(),
        hostname: std::env::var("HOSTNAME").unwrap_or_default(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        capabilities: vec!["nsjail".to_string(), "cgroup-v2".to_string()],
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
        if let Err(err) = group_result {
            if !err.to_string().contains("BUSYGROUP") {
                return Err(err).context("create redis task stream consumer group failed");
            }
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
        capabilities: vec!["nsjail".to_string(), "cgroup-v2".to_string()],
        supported_languages: config.supported_languages.clone(),
        available_slots: available_slots as i32,
        task_ids: stream_task_ids(pending_task_events),
    };
    let resp: WorkerClaimTasksResp =
        post_json(client, config, "/judge/worker/tasks/claim", &req).await?;
    Ok(resp.tasks)
}

async fn lease_heartbeat_loop(
    client: Client,
    config: Arc<WorkerLinkConfig>,
    task: WorkerTaskLease,
    mut stop: tokio::sync::watch::Receiver<bool>,
) {
    let every = (config.task_lease_ttl / 3).max(Duration::from_secs(5));
    loop {
        tokio::select! {
            _ = stop.changed() => {
                if *stop.borrow() {
                    return;
                }
            }
            _ = tokio::time::sleep(every) => {
                let req = WorkerTaskHeartbeatReq {
                    worker_id: config.worker_id.clone(),
                    lease_version: task.lease_version,
                };
                let path = format!("/judge/worker/tasks/{}/heartbeat", task.task_id);
                if let Err(err) = post_json::<_, WorkerTaskHeartbeatResp>(&client, &config, &path, &req).await {
                    warn!(task_id = %task.task_id, error = %err, "task heartbeat failed");
                    return;
                }
            }
        }
    }
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
    let path = format!("/judge/worker/tasks/{}/result", task.task_id);
    let resp: WorkerSubmitResultResp = post_json(client, config, &path, &req).await?;
    if !resp.accepted {
        return Err(anyhow!("judge-api rejected task result: {}", resp.status));
    }
    Ok(())
}

async fn fail_task(
    client: &Client,
    config: &WorkerLinkConfig,
    task: &WorkerTaskLease,
    retryable: bool,
    error_type: &str,
    message: &str,
) -> Result<()> {
    let req = WorkerFailTaskReq {
        worker_id: config.worker_id.clone(),
        lease_version: task.lease_version,
        error_type: error_type.to_string(),
        message: message.to_string(),
        retryable,
    };
    let path = format!("/judge/worker/tasks/{}/fail", task.task_id);
    let _: WorkerFailTaskResp = post_json(client, config, &path, &req).await?;
    Ok(())
}

async fn download_artifact(
    client: &Client,
    config: &WorkerLinkConfig,
    artifact: &WorkerArtifactRef,
    target: &Path,
) -> Result<()> {
    let url = absolute_url(config, &artifact.url);
    let mut resp = client
        .get(url)
        .header("X-OJOS-Worker-Token", &config.worker_token)
        .send()
        .await?
        .error_for_status()?;

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

    let digest = format!("{:x}", hasher.finalize());
    if digest != artifact.sha256 {
        return Err(anyhow!("artifact digest mismatch"));
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
    let resp = client
        .post(absolute_url(config, path))
        .header("X-OJOS-Worker-Token", &config.worker_token)
        .json(body)
        .send()
        .await?;
    let status = resp.status();
    let text = resp.text().await?;
    if !status.is_success() {
        return Err(anyhow!("judge-api returned {}: {}", status, text));
    }
    serde_json::from_str(&text).with_context(|| format!("decode judge-api response: {}", text))
}

fn absolute_url(config: &WorkerLinkConfig, path: &str) -> String {
    if path.starts_with("http://") || path.starts_with("https://") {
        return path.to_string();
    }
    format!(
        "{}/{}",
        config.judge_api_url.trim_end_matches('/'),
        path.trim_start_matches('/')
    )
}

fn unzip_safe(zip_path: &Path, target_dir: &Path) -> Result<()> {
    let file = std::fs::File::open(zip_path)?;
    let mut archive = zip::ZipArchive::new(file)?;
    let target_root = target_dir
        .canonicalize()
        .unwrap_or_else(|_| target_dir.to_path_buf());

    for index in 0..archive.len() {
        let mut file = archive.by_index(index)?;
        let Some(enclosed) = file.enclosed_name().map(|p| p.to_path_buf()) else {
            return Err(anyhow!("zip entry escapes package root"));
        };
        let out_path = target_dir.join(enclosed);
        if !out_path.starts_with(&target_root) && target_root.exists() {
            return Err(anyhow!("zip entry escapes package root"));
        }

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
        .unwrap_or_else(|| match language {
            "c11" => "main.c".to_string(),
            "python3" => "main.py".to_string(),
            "java17" => "Main.java".to_string(),
            _ => "main.cpp".to_string(),
        })
}

fn env_or<F>(key: &str, fallback: F) -> String
where
    F: FnOnce() -> String,
{
    std::env::var(key).unwrap_or_else(|_| fallback())
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
    source: WorkerArtifactRef,
    problem_package: WorkerArtifactRef,
}

#[derive(Debug, Clone, Deserialize)]
struct WorkerArtifactRef {
    url: String,
    sha256: String,
    size_bytes: i64,
}

#[derive(Debug, Serialize)]
struct WorkerTaskHeartbeatReq {
    worker_id: String,
    lease_version: i32,
}

#[derive(Debug, Deserialize)]
struct WorkerTaskHeartbeatResp {}

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
struct WorkerFailTaskResp {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::result::{ResultCase, ResultFile};
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::{Arc, Mutex};

    #[test]
    fn redis_stream_task_events_extracts_ids_and_task_metadata_from_xreadgroup_response() {
        let value = redis::Value::Array(vec![redis::Value::Array(vec![
            redis::Value::BulkString(b"ojos:judge:submissions".to_vec()),
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
            redis_task_stream: "ojos:judge:submissions".to_string(),
            redis_consumer_group: "judge-workers".to_string(),
            internal_gateway_url: None,
            storage_api_get: "storage.object.get".to_string(),
            storage_api_put: "storage.object.put".to_string(),
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
                sha256: artifact_sha256,
                size_bytes: artifact_body.len() as i64,
            },
            &artifact_target,
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
            source: WorkerArtifactRef {
                url: "/unused/source".to_string(),
                sha256: String::new(),
                size_bytes: 0,
            },
            problem_package: WorkerArtifactRef {
                url: "/unused/package".to_string(),
                sha256: String::new(),
                size_bytes: 0,
            },
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
        fail_task(&client, &config, &task, false, "SYSTEM", "nsjail failed")
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

        let _ = fs::remove_dir_all(&config.work_dir).await;
        let _ = fs::remove_dir_all(&config.artifact_cache_dir).await;
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
            "/judge/worker/tasks/sub-42/fail" => r#"{}"#,
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
