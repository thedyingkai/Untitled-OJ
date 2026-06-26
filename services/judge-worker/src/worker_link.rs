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
    pub control_plane_url: String,
    pub worker_token: String,
    pub max_concurrency: usize,
    pub work_dir: PathBuf,
    pub artifact_cache_dir: PathBuf,
    pub supported_languages: Vec<String>,
    pub heartbeat_interval: Duration,
    pub task_lease_ttl: Duration,
}

impl WorkerLinkConfig {
    pub fn from_env(languages: &LanguagesConfig) -> Result<Self> {
        let worker_id = env_or("OJOS_WORKER_ID", || {
            std::env::var("HOSTNAME").unwrap_or_else(|_| format!("worker-{}", Uuid::new_v4()))
        });
        let worker_name = env_or("OJOS_WORKER_NAME", || worker_id.clone());
        let control_plane_url = required_env("OJOS_CONTROL_PLANE_URL")?
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

        Ok(Self {
            worker_id,
            worker_name,
            control_plane_url,
            worker_token,
            max_concurrency: max_concurrency.max(1),
            work_dir,
            artifact_cache_dir,
            supported_languages,
            heartbeat_interval,
            task_lease_ttl,
        })
    }
}

pub async fn run_worker_link(languages: Arc<LanguagesConfig>) -> Result<()> {
    let config = Arc::new(WorkerLinkConfig::from_env(&languages)?);
    fs::create_dir_all(&config.work_dir).await?;
    fs::create_dir_all(&config.artifact_cache_dir).await?;

    let client = Client::builder()
        .timeout(Duration::from_secs(60))
        .build()
        .context("create worker http client failed")?;

    register_worker(&client, &config).await?;

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

    loop {
        let available = semaphore.available_permits();
        if available == 0 {
            tokio::time::sleep(Duration::from_millis(300)).await;
            continue;
        }

        let tasks = claim_tasks(&client, &config, available).await?;
        if tasks.is_empty() {
            tokio::time::sleep(Duration::from_secs(1)).await;
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
                    error!(error = %err, "worker task failed");
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
            fail_task(&client, &config, &task, true, "SYSTEM", &err.to_string()).await?;
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

async fn claim_tasks(
    client: &Client,
    config: &WorkerLinkConfig,
    available_slots: usize,
) -> Result<Vec<WorkerTaskLease>> {
    let req = WorkerClaimTasksReq {
        worker_id: config.worker_id.clone(),
        capabilities: vec!["nsjail".to_string(), "cgroup-v2".to_string()],
        supported_languages: config.supported_languages.clone(),
        available_slots: available_slots as i32,
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
        return Err(anyhow!(
            "control plane rejected task result: {}",
            resp.status
        ));
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
        return Err(anyhow!("control plane returned {}: {}", status, text));
    }
    serde_json::from_str(&text).with_context(|| format!("decode control plane response: {}", text))
}

fn absolute_url(config: &WorkerLinkConfig, path: &str) -> String {
    if path.starts_with("http://") || path.starts_with("https://") {
        return path.to_string();
    }
    format!(
        "{}/{}",
        config.control_plane_url.trim_end_matches('/'),
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
