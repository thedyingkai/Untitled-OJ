//! Required live gate for the v1 Store -> durable Job -> Agent -> Docker path.
//!
//! Ordinary `cargo test` runs skip this external-engine test. The checked-in
//! gate script always sets `OJOS_REQUIRE_DOCKER_AGENT_E2E=1` and supplies two
//! immutable images from an isolated local registry, so CI cannot silently
//! substitute a mock runtime or a mutable tag.

use anyhow::{Context, Result, anyhow, bail, ensure};
use async_trait::async_trait;
use base64::{Engine as _, engine::general_purpose::STANDARD};
use ed25519_dalek::{Signer, SigningKey};
use orchestrator_agent::{
    AgentClaimRequest, AgentLedger, AgentTransport, AgentWorker, ClaimResponse, HeartbeatAck,
    JobExecutor, LoopbackHttpTransport, PollOutcome, TransportError, WorkerConfig,
};
use orchestrator_backend::{
    EmbeddedServerHandle, EmbeddedServerOptions, EmbeddedStorage, start_embedded_server,
};
use orchestrator_control_plane::{CompleteRequest, HeartbeatRequest};
use orchestrator_manager::catalog_v2::{
    CatalogModuleV2, CatalogReleaseV2, CatalogV2, Ed25519Signature, MetadataPackageV2,
    ReleaseChannel, TargetPlatform,
};
use orchestrator_runtime::{ContainerRuntime, DockerEngineRuntime, OciImageReference};
use reqwest::{Client, StatusCode, header::SET_COOKIE};
use semver::Version;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::ffi::{OsStr, OsString};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

const NODE_ID: &str = "desktop-local";
const CATALOG_KEY_ID: &str = "docker-e2e-key";
const TERMINAL_OPERATION_STATUSES: &[&str] = &[
    "SUCCEEDED",
    "FAILED",
    "CANCELLED",
    "NEEDS_ATTENTION",
    "ROLLED_BACK",
];

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn store_job_agent_docker_lifecycle_and_recovery() {
    if std::env::var("OJOS_REQUIRE_DOCKER_AGENT_E2E").as_deref() != Ok("1") {
        eprintln!(
            "skipping live Docker Agent gate; run deploy/ops/orchestrator-docker-agent-e2e.sh"
        );
        return;
    }
    run_live_gate()
        .await
        .expect("live Docker Agent gate failed");
}

async fn run_live_gate() -> Result<()> {
    let image_v1 = required_image("OJOS_DOCKER_E2E_IMAGE_V1")?;
    let image_v2 = required_image("OJOS_DOCKER_E2E_IMAGE_V2")?;
    ensure!(image_v1 != image_v2, "v1 and v2 OCI digests must differ");
    let service_id = required_service_id()?;
    let _cleanup = DockerCleanup::new(service_id.clone());

    let docker = DockerEngineRuntime::connect_local().context("connect to local Docker Engine")?;
    docker.ping().await.context("ping local Docker Engine")?;

    let repo_root = workspace_root()?;
    let scratch_root = repo_root.join(".tmp");
    fs::create_dir_all(&scratch_root).context("create workspace .tmp directory")?;
    let data = tempfile::Builder::new()
        .prefix("docker-agent-e2e-")
        .tempdir_in(&scratch_root)
        .context("create live gate data directory")?;
    let web_root = data.path().join("web");
    fs::create_dir_all(&web_root)?;
    fs::write(web_root.join("index.html"), "orchestrator docker agent e2e")?;
    let database_path = data.path().join("orchestrator.db");
    let artifact_root = data.path().join("artifacts");
    let ledger_path = data.path().join("agent-ledger.db");

    let (trust_json, sources_json) =
        write_catalog(&repo_root, data.path(), &service_id, &image_v1, &image_v2)?;
    let mut environment = EnvironmentGuard::default();
    environment.set("ORCHESTRATOR_CATALOG_TRUST_KEYS", &trust_json);
    environment.set("ORCHESTRATOR_CATALOG_SOURCES", &sources_json);
    environment.set("ORCHESTRATOR_MAX_WORKERS", "4");
    environment.remove("ORCHESTRATOR_DATABASE_URL");

    let first_bootstrap = format!("docker-e2e-agent-{}-first", std::process::id());
    let first_web_bootstrap = format!("docker-e2e-web-{}-first", std::process::id());
    let first_server = start_server(
        &repo_root,
        &web_root,
        &artifact_root,
        &database_path,
        &first_web_bootstrap,
        &first_bootstrap,
    )?;
    let first_origin = format!("http://{}", first_server.local_addr());
    let first_session = DesktopSession::exchange(&first_origin, &first_web_bootstrap).await?;

    let install_body = json!({
        "service_id": service_id,
        "version": "1.0.0",
        "target_node_id": NODE_ID
    });
    let installed = first_session
        .post_json(
            "/api/v1/store/releases:install",
            "docker-e2e-install-v1",
            &install_body,
            StatusCode::ACCEPTED,
        )
        .await?;
    let install_operation = required_pointer_str(&installed, "/data/operation_id")?;
    let deployment_v1 = required_pointer_str(&installed, "/data/deployment_id")?;
    drive_operation(
        &first_session,
        &first_origin,
        &first_bootstrap,
        &ledger_path,
        &install_operation,
        1,
    )
    .await?;

    let first_projection = only_deployment(&first_session).await?;
    assert_projection(
        &first_projection,
        &deployment_v1,
        &service_id,
        &image_v1,
        "RUNNING",
        "HEALTHY",
    )?;
    ensure!(
        docker_container_count(&service_id)? == 1,
        "install must create exactly one Docker container"
    );

    // Replaying the exact Store mutation must return the persisted response and
    // must not create either a second Operation or a second Docker container.
    let idempotent = first_session
        .post_json(
            "/api/v1/store/releases:install",
            "docker-e2e-install-v1",
            &install_body,
            StatusCode::ACCEPTED,
        )
        .await?;
    ensure!(
        required_pointer_str(&idempotent, "/data/operation_id")? == install_operation,
        "idempotent Store replay changed operation_id"
    );
    ensure!(
        docker_container_count(&service_id)? == 1,
        "idempotent Store replay created a second Docker container"
    );

    // Queue a mutation, terminate the control plane before any Agent claims it,
    // then prove the same SQLite-backed Operation resumes after restart.
    let stopped = first_session
        .post_json(
            &format!("/api/v1/deployments/{deployment_v1}:stop"),
            "docker-e2e-stop",
            &json!({}),
            StatusCode::ACCEPTED,
        )
        .await?;
    let stop_operation = required_pointer_str(&stopped, "/data/operation_id")?;
    ensure!(
        operation_status(&first_session, &stop_operation).await? == "RUNNING",
        "queued stop Operation was not durable before restart"
    );
    shutdown_server(first_server)?;

    let second_bootstrap = format!("docker-e2e-agent-{}-second", std::process::id());
    let second_web_bootstrap = format!("docker-e2e-web-{}-second", std::process::id());
    let second_server = start_server(
        &repo_root,
        &web_root,
        &artifact_root,
        &database_path,
        &second_web_bootstrap,
        &second_bootstrap,
    )?;
    let second_origin = format!("http://{}", second_server.local_addr());
    let second_session = DesktopSession::exchange(&second_origin, &second_web_bootstrap).await?;
    ensure!(
        operation_status(&second_session, &stop_operation).await? == "RUNNING",
        "control-plane restart lost the queued stop Operation"
    );
    drive_operation(
        &second_session,
        &second_origin,
        &second_bootstrap,
        &ledger_path,
        &stop_operation,
        10,
    )
    .await?;
    let stopped_projection = only_deployment(&second_session).await?;
    ensure!(
        stopped_projection.pointer("/instance/desired_state") == Some(&json!("STOPPED")),
        "stop did not project desired_state=STOPPED: {stopped_projection}"
    );
    ensure!(
        stopped_projection.pointer("/instance/observed_state") != Some(&json!("RUNNING")),
        "stop left the actual Docker projection running: {stopped_projection}"
    );

    let started = second_session
        .post_json(
            &format!("/api/v1/deployments/{deployment_v1}:start"),
            "docker-e2e-start",
            &json!({}),
            StatusCode::ACCEPTED,
        )
        .await?;
    let start_operation = required_pointer_str(&started, "/data/operation_id")?;
    drive_operation(
        &second_session,
        &second_origin,
        &second_bootstrap,
        &ledger_path,
        &start_operation,
        20,
    )
    .await?;
    ensure!(
        only_deployment(&second_session)
            .await?
            .pointer("/instance/observed_state")
            == Some(&json!("RUNNING")),
        "start did not restore RUNNING"
    );

    // A lost completion response occurs after the real Docker restart and the
    // Agent has durably recorded success. Expire the lease, restart the Agent,
    // and require a ledger replay. The Docker StartedAt value must not change a
    // second time, proving the runtime mutation was not blindly repeated.
    let restarted = second_session
        .post_json(
            &format!("/api/v1/deployments/{deployment_v1}:restart"),
            "docker-e2e-restart",
            &json!({}),
            StatusCode::ACCEPTED,
        )
        .await?;
    let restart_operation = required_pointer_str(&restarted, "/data/operation_id")?;
    let restart_job_id = operation_job_id(&second_session, &restart_operation).await?;
    let transport =
        LoopbackHttpTransport::new_with_bootstrap(&second_origin, second_bootstrap.clone())?;
    let failed_completion =
        run_worker_once(FailCompleteTransport { inner: transport }, &ledger_path, 30).await;
    ensure!(
        matches!(failed_completion, Err(ref error) if error.to_string().contains("injected completion response loss")),
        "restart did not reach the injected completion-loss point: {failed_completion:?}"
    );
    let recorded = AgentLedger::open(&ledger_path)?
        .get(&restart_job_id)?
        .ok_or_else(|| anyhow!("restart was not written to the Agent ledger"))?;
    ensure!(
        recorded.completion.is_some(),
        "Agent did not durably finish restart before reporting it"
    );
    let container_id = required_pointer_str(
        &only_deployment(&second_session).await?,
        "/instance/container_id",
    )?;
    let started_at_after_first_restart = docker_started_at(&container_id)?;
    expire_job_lease(&database_path, &restart_job_id)?;
    tokio::time::sleep(Duration::from_millis(2_200)).await;
    drive_operation(
        &second_session,
        &second_origin,
        &second_bootstrap,
        &ledger_path,
        &restart_operation,
        31,
    )
    .await?;
    ensure!(
        docker_started_at(&container_id)? == started_at_after_first_restart,
        "Agent ledger replay executed Docker restart twice"
    );

    // Refresh health through the published deployment.health action and a real
    // Node Health job, rather than trusting the earlier Store projection.
    wait_for_actual_health(&docker, &container_id).await?;
    let health_operation = plan_and_apply_health(
        &second_session,
        &deployment_v1,
        &container_id,
        "docker-e2e-health",
    )
    .await?;
    drive_operation(
        &second_session,
        &second_origin,
        &second_bootstrap,
        &ledger_path,
        &health_operation,
        40,
    )
    .await?;
    let health = second_session
        .get_json(
            &format!("/api/v1/deployments/{deployment_v1}/health"),
            StatusCode::OK,
        )
        .await?;
    ensure!(
        health.pointer("/data/health") == Some(&json!("HEALTHY"))
            && health.pointer("/data/observed_state") == Some(&json!("RUNNING")),
        "published deployment health did not reflect Docker: {health}"
    );

    let upgraded = second_session
        .post_json(
            "/api/v1/store/releases:upgrade",
            "docker-e2e-upgrade-v2",
            &json!({"deployment_id": deployment_v1, "version": "2.0.0"}),
            StatusCode::ACCEPTED,
        )
        .await?;
    let upgrade_operation = required_pointer_str(&upgraded, "/data/operation_id")?;
    drive_operation(
        &second_session,
        &second_origin,
        &second_bootstrap,
        &ledger_path,
        &upgrade_operation,
        50,
    )
    .await?;
    let projection_v2 = only_deployment(&second_session).await?;
    let deployment_v2 = required_pointer_str(&projection_v2, "/instance/deployment_id")?;
    assert_projection(
        &projection_v2,
        &deployment_v2,
        &service_id,
        &image_v2,
        "RUNNING",
        "HEALTHY",
    )?;
    ensure!(
        deployment_v2 != deployment_v1,
        "upgrade reused the old deployment id"
    );
    ensure!(
        docker_container_count(&service_id)? == 1,
        "upgrade did not atomically replace the old container"
    );

    let rolled_back = second_session
        .post_json(
            "/api/v1/store/releases:rollback",
            "docker-e2e-rollback-v1",
            &json!({"deployment_id": deployment_v2, "version": "1.0.0"}),
            StatusCode::ACCEPTED,
        )
        .await?;
    let rollback_operation = required_pointer_str(&rolled_back, "/data/operation_id")?;
    drive_operation(
        &second_session,
        &second_origin,
        &second_bootstrap,
        &ledger_path,
        &rollback_operation,
        60,
    )
    .await?;
    let rolled_back_projection = only_deployment(&second_session).await?;
    let rolled_back_deployment =
        required_pointer_str(&rolled_back_projection, "/instance/deployment_id")?;
    assert_projection(
        &rolled_back_projection,
        &rolled_back_deployment,
        &service_id,
        &image_v1,
        "RUNNING",
        "HEALTHY",
    )?;
    ensure!(
        docker_container_count(&service_id)? == 1,
        "rollback did not leave exactly one proven container"
    );

    let uninstalled = second_session
        .post_json(
            &format!("/api/v1/deployments/{rolled_back_deployment}:uninstall"),
            "docker-e2e-uninstall",
            &json!({}),
            StatusCode::ACCEPTED,
        )
        .await?;
    let uninstall_operation = required_pointer_str(&uninstalled, "/data/operation_id")?;
    drive_operation(
        &second_session,
        &second_origin,
        &second_bootstrap,
        &ledger_path,
        &uninstall_operation,
        70,
    )
    .await?;
    ensure!(
        deployment_items(&second_session).await?.is_empty(),
        "uninstall left a runtime projection"
    );
    ensure!(
        docker_container_count(&service_id)? == 0,
        "uninstall left a Docker container"
    );

    shutdown_server(second_server)?;
    drop(data);
    Ok(())
}

fn required_image(name: &str) -> Result<String> {
    let value = std::env::var(name).with_context(|| format!("{name} is required"))?;
    OciImageReference::parse(&value).with_context(|| format!("{name} is not an OCI digest"))?;
    Ok(value)
}

fn required_service_id() -> Result<String> {
    let value = std::env::var("OJOS_DOCKER_E2E_SERVICE_ID")
        .context("OJOS_DOCKER_E2E_SERVICE_ID is required")?;
    ensure!(
        !value.is_empty()
            && value.len() <= 63
            && value
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-'),
        "OJOS_DOCKER_E2E_SERVICE_ID is not a safe service id"
    );
    Ok(value)
}

fn workspace_root() -> Result<PathBuf> {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(3)
        .map(Path::to_path_buf)
        .ok_or_else(|| anyhow!("could not locate workspace root"))
}

fn write_catalog(
    repo_root: &Path,
    directory: &Path,
    service_id: &str,
    image_v1: &str,
    image_v2: &str,
) -> Result<(String, String)> {
    let release = |version: &str, image: &str| -> Result<(String, String)> {
        let filename = format!("{service_id}-{version}.release.yaml");
        let manifest = json!({
            "schema_version": 1,
            "service_name": service_id,
            "version": version,
            "description": "live Docker Agent integration fixture",
            "service_type": "backend-api",
            "source": {
                "kind": "url",
                "url": "https://invalid.example.test/metadata-only",
                "checksum": format!("sha256:{}", "0".repeat(64))
            },
            "runtime": {
                "kind": "image",
                "image": image,
                "command": "",
                "args": [],
                "env": {}
            },
            "backend": {"protocol": "http", "port": 8080, "health_path": "/health"},
            "migrations": [],
            "permissions": [],
            "routes": [],
            "apis": [],
            "redis": [],
            "storage": [],
            "dependencies": [],
            "required_apis": [],
            "config_schema": {},
            "secrets": []
        });
        let yaml = serde_yaml::to_string(&manifest)?;
        fs::write(directory.join(&filename), yaml.as_bytes())?;
        Ok((
            filename,
            format!("sha256:{:x}", Sha256::digest(yaml.as_bytes())),
        ))
    };
    let (metadata_v1, checksum_v1) = release("1.0.0", image_v1)?;
    let (metadata_v2, checksum_v2) = release("2.0.0", image_v2)?;

    let platforms = vec![TargetPlatform::current()];
    let catalog_release = |version: &str,
                           image: &str,
                           metadata: String,
                           checksum: String|
     -> Result<CatalogReleaseV2> {
        Ok(CatalogReleaseV2 {
            version: Version::parse(version)?,
            channel: ReleaseChannel::Stable,
            platforms: platforms.clone(),
            min_orchestrator_version: Version::parse("0.1.0")?,
            dependencies: vec![],
            runtime_capabilities: Vec::new(),
            metadata: MetadataPackageV2 {
                url: metadata,
                sha256: checksum.parse()?,
            },
            oci_image: image.parse()?,
        })
    };
    let signing_key = SigningKey::from_bytes(&[41_u8; 32]);
    let mut catalog = CatalogV2 {
        schema_version: 2,
        id: "docker-agent-e2e".to_string(),
        name: "Docker Agent E2E".to_string(),
        modules: vec![CatalogModuleV2 {
            id: service_id.to_string(),
            name: service_id.to_string(),
            description: "real Docker Engine lifecycle fixture".to_string(),
            kind: "backend-api".to_string(),
            tags: vec!["e2e".to_string()],
            releases: vec![
                catalog_release("1.0.0", image_v1, metadata_v1, checksum_v1)?,
                catalog_release("2.0.0", image_v2, metadata_v2, checksum_v2)?,
            ],
        }],
        signatures: vec![],
    };
    let signature = signing_key.sign(&catalog.signing_payload_jcs()?);
    catalog.signatures.push(Ed25519Signature {
        key_id: CATALOG_KEY_ID.to_string(),
        algorithm: "Ed25519".to_string(),
        signature: STANDARD.encode(signature.to_bytes()),
    });
    catalog
        .validate()
        .context("validate signed Catalog fixture")?;
    fs::write(
        directory.join("catalog.json"),
        serde_json::to_vec_pretty(&catalog)?,
    )?;

    let catalog_relative = directory
        .join("catalog.json")
        .strip_prefix(repo_root)
        .context("catalog fixture must be inside the workspace")?
        .to_str()
        .context("catalog fixture path must be valid UTF-8")?
        .replace('\\', "/");
    let trust_json = serde_json::to_string(&std::collections::BTreeMap::from([(
        CATALOG_KEY_ID,
        STANDARD.encode(signing_key.verifying_key().to_bytes()),
    )]))?;
    let sources_json = serde_json::to_string(&json!([{
        "id": "docker-agent-e2e",
        "url": catalog_relative,
        "required_key_id": CATALOG_KEY_ID,
        "auth_secret_ref": "",
        "enabled": true,
        "offline_oci_layouts": {}
    }]))?;
    Ok((trust_json, sources_json))
}

fn start_server(
    repo_root: &Path,
    web_root: &Path,
    artifact_root: &Path,
    database_path: &Path,
    desktop_bootstrap: &str,
    agent_bootstrap: &str,
) -> Result<EmbeddedServerHandle> {
    start_embedded_server(EmbeddedServerOptions {
        repo_root: repo_root.to_path_buf(),
        web_root: web_root.to_path_buf(),
        artifact_root: artifact_root.to_path_buf(),
        bind_addr: "127.0.0.1:0".parse()?,
        internal_token: None,
        desktop_bootstrap_secret: Some(desktop_bootstrap.to_string()),
        desktop_agent_secret: Some(agent_bootstrap.to_string()),
        storage: EmbeddedStorage::Sqlite {
            database_path: database_path.to_path_buf(),
        },
    })
    .context("start embedded SQLite control plane")
}

fn shutdown_server(server: EmbeddedServerHandle) -> Result<()> {
    server.shutdown()?;
    server
        .join_timeout(Duration::from_secs(10))
        .context("stop embedded control plane")
}

#[derive(Clone)]
struct DesktopSession {
    client: Client,
    origin: String,
    cookie: String,
    csrf: String,
}

impl DesktopSession {
    async fn exchange(origin: &str, bootstrap: &str) -> Result<Self> {
        let client = Client::builder()
            .no_proxy()
            .connect_timeout(Duration::from_secs(2))
            .timeout(Duration::from_secs(30))
            .build()?;
        let response = client
            .post(format!("{origin}/api/v1/auth/desktop/exchange"))
            .header("x-ojos-desktop-bootstrap", bootstrap)
            .json(&json!({}))
            .send()
            .await?;
        ensure!(
            response.status() == StatusCode::OK,
            "desktop exchange returned {}: {}",
            response.status(),
            response.text().await.unwrap_or_default()
        );
        let cookie = response
            .headers()
            .get(SET_COOKIE)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.split(';').next())
            .map(str::to_string)
            .context("desktop exchange omitted its HttpOnly session cookie")?;
        let body: Value = response.json().await?;
        let csrf = required_pointer_str(&body, "/csrf_token")?;
        Ok(Self {
            client,
            origin: origin.to_string(),
            cookie,
            csrf,
        })
    }

    async fn post_json(
        &self,
        path: &str,
        idempotency_key: &str,
        body: &Value,
        expected: StatusCode,
    ) -> Result<Value> {
        let response = self
            .client
            .post(format!("{}{path}", self.origin))
            .header("cookie", &self.cookie)
            .header("x-csrf-token", &self.csrf)
            .header("idempotency-key", idempotency_key)
            .json(body)
            .send()
            .await?;
        json_response(response, expected, path).await
    }

    async fn get_json(&self, path: &str, expected: StatusCode) -> Result<Value> {
        let response = self
            .client
            .get(format!("{}{path}", self.origin))
            .header("cookie", &self.cookie)
            .send()
            .await?;
        json_response(response, expected, path).await
    }
}

async fn json_response(
    response: reqwest::Response,
    expected: StatusCode,
    path: &str,
) -> Result<Value> {
    let status = response.status();
    let bytes = response.bytes().await?;
    let body = match serde_json::from_slice::<Value>(&bytes) {
        Ok(body) => body,
        Err(_) => json!({
            "non_json_body": std::str::from_utf8(&bytes)
                .context("non-JSON response body must be valid UTF-8")?
        }),
    };
    ensure!(
        status == expected,
        "{path} returned {status}, expected {expected}: {body}"
    );
    Ok(body)
}

fn required_pointer_str(value: &Value, pointer: &str) -> Result<String> {
    value
        .pointer(pointer)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .ok_or_else(|| anyhow!("response omitted {pointer}: {value}"))
}

async fn operation_status(session: &DesktopSession, operation_id: &str) -> Result<String> {
    let response = session
        .get_json(
            &format!("/api/v1/operations/{operation_id}"),
            StatusCode::OK,
        )
        .await?;
    required_pointer_str(&response, "/data/operation/status")
}

async fn operation_job_id(session: &DesktopSession, operation_id: &str) -> Result<String> {
    let response = session
        .get_json(
            &format!("/api/v1/operations/{operation_id}"),
            StatusCode::OK,
        )
        .await?;
    required_pointer_str(&response, "/data/operation/job_bindings/0/job_id")
}

async fn drive_operation(
    session: &DesktopSession,
    origin: &str,
    bootstrap: &str,
    ledger_path: &Path,
    operation_id: &str,
    instance_seed: u32,
) -> Result<()> {
    for attempt in 0..30_u32 {
        let status = operation_status(session, operation_id).await?;
        if TERMINAL_OPERATION_STATUSES.contains(&status.as_str()) {
            ensure!(
                status == "SUCCEEDED",
                "Operation {operation_id} ended in {status}"
            );
            return Ok(());
        }
        let transport = LoopbackHttpTransport::new_with_bootstrap(origin, bootstrap.to_string())?;
        match tokio::time::timeout(
            Duration::from_secs(90),
            run_worker_once(transport, ledger_path, instance_seed + attempt),
        )
        .await
        {
            Ok(Ok(PollOutcome::Completed { .. } | PollOutcome::Idle { .. })) => {}
            Ok(Err(error)) => return Err(error).context("run restarted Agent worker"),
            Err(_) => bail!("Agent worker timed out while driving {operation_id}"),
        }
        tokio::time::sleep(Duration::from_millis(350)).await;
    }
    bail!("Operation {operation_id} did not reach a terminal state")
}

async fn run_worker_once<T: AgentTransport>(
    transport: T,
    ledger_path: &Path,
    instance: u32,
) -> Result<PollOutcome> {
    let runtime = DockerEngineRuntime::connect_local()?;
    let runtime = match std::env::var_os("OJOS_DOCKER_E2E_REGISTRY_CREDENTIALS") {
        Some(path) => runtime.with_registry_credentials_file(Path::new(&path))?,
        None => runtime,
    };
    runtime.ping().await?;
    let ledger = AgentLedger::open(ledger_path)?;
    let mut worker = AgentWorker::new(
        WorkerConfig {
            node_id: NODE_ID.to_string(),
            instance_id: format!("docker-e2e-agent-{instance}"),
            heartbeat_ms: 1_000,
            lease_ms: 30_000,
            transport_retry_ms: 100,
        },
        transport,
        JobExecutor::new(runtime),
        ledger,
    )?;
    worker.poll_once().await.map_err(Into::into)
}

struct FailCompleteTransport<T> {
    inner: T,
}

#[async_trait]
impl<T: AgentTransport> AgentTransport for FailCompleteTransport<T> {
    async fn claim(&self, request: AgentClaimRequest) -> Result<ClaimResponse, TransportError> {
        self.inner.claim(request).await
    }

    async fn heartbeat(
        &self,
        node_id: &str,
        request: HeartbeatRequest,
    ) -> Result<HeartbeatAck, TransportError> {
        self.inner.heartbeat(node_id, request).await
    }

    async fn complete(
        &self,
        _node_id: &str,
        _request: CompleteRequest,
    ) -> Result<(), TransportError> {
        Err(TransportError::Protocol(
            "injected completion response loss".to_string(),
        ))
    }
}

async fn deployment_items(session: &DesktopSession) -> Result<Vec<Value>> {
    let response = session
        .get_json("/api/v1/deployments?limit=200", StatusCode::OK)
        .await?;
    response
        .pointer("/data/items")
        .and_then(Value::as_array)
        .cloned()
        .ok_or_else(|| anyhow!("deployment list has no data.items: {response}"))
}

async fn only_deployment(session: &DesktopSession) -> Result<Value> {
    let items = deployment_items(session).await?;
    ensure!(items.len() == 1, "expected one Deployment, found {items:?}");
    Ok(items.into_iter().next().expect("length checked"))
}

fn assert_projection(
    projection: &Value,
    deployment_id: &str,
    service_id: &str,
    image: &str,
    observed_state: &str,
    health: &str,
) -> Result<()> {
    ensure!(
        projection.pointer("/instance/deployment_id") == Some(&json!(deployment_id))
            && projection.pointer("/instance/service_id") == Some(&json!(service_id))
            && projection.pointer("/instance/artifact_digest") == Some(&json!(image))
            && projection.pointer("/instance/observed_state") == Some(&json!(observed_state))
            && projection.pointer("/instance/health") == Some(&json!(health)),
        "runtime projection did not match the real release: {projection}"
    );
    Ok(())
}

async fn plan_and_apply_health(
    session: &DesktopSession,
    deployment_id: &str,
    container_id: &str,
    key: &str,
) -> Result<String> {
    let planned = session
        .post_json(
            "/api/v1/operations:plan",
            &format!("{key}-plan"),
            &json!({
                "action": "deployment.health",
                "fields": {
                    "target_node_id": NODE_ID,
                    "deployment_id": deployment_id,
                    "payload": {"container_id": container_id}
                }
            }),
            StatusCode::CREATED,
        )
        .await?;
    let operation_id = required_pointer_str(&planned, "/data/operation/operation_id")?;
    session
        .post_json(
            &format!("/api/v1/operations/{operation_id}:confirm"),
            &format!("{key}-confirm"),
            &json!({}),
            StatusCode::OK,
        )
        .await?;
    session
        .post_json(
            &format!("/api/v1/operations/{operation_id}:apply"),
            &format!("{key}-apply"),
            &json!({}),
            StatusCode::ACCEPTED,
        )
        .await?;
    Ok(operation_id)
}

async fn wait_for_actual_health(runtime: &DockerEngineRuntime, container_id: &str) -> Result<()> {
    for _ in 0..60 {
        let instance = runtime.inspect_container(container_id).await?;
        if instance.observed_state == orchestrator_runtime::RuntimeObservedState::Running
            && instance.health == "HEALTHY"
        {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    bail!("Docker container {container_id} did not become HEALTHY")
}

fn expire_job_lease(database_path: &Path, job_id: &str) -> Result<()> {
    let connection = rusqlite::Connection::open(database_path)?;
    connection.busy_timeout(Duration::from_secs(5))?;
    let payload: String = connection.query_row(
        "SELECT payload FROM orchestrator_jobs WHERE job_id = ?1 AND status = 'LEASED'",
        [job_id],
        |row| row.get(0),
    )?;
    let mut payload: Value = serde_json::from_str(&payload)?;
    payload["lease_expires_at_ms"] = json!(0);
    let changed = connection.execute(
        "UPDATE orchestrator_jobs SET payload = ?2 WHERE job_id = ?1 AND status = 'LEASED'",
        rusqlite::params![job_id, serde_json::to_string(&payload)?],
    )?;
    ensure!(changed == 1, "could not expire leased job {job_id}");
    Ok(())
}

fn docker_container_count(service_id: &str) -> Result<usize> {
    let filter = format!("label=ojos.service_id={service_id}");
    let output = docker_command(["ps", "-aq", "--filter", &filter])?;
    Ok(output
        .lines()
        .filter(|line| !line.trim().is_empty())
        .count())
}

fn docker_started_at(container_id: &str) -> Result<String> {
    Ok(
        docker_command(["inspect", "--format", "{{.State.StartedAt}}", container_id])?
            .trim()
            .to_string(),
    )
}

fn docker_command<I, S>(arguments: I) -> Result<String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let output = Command::new("docker").args(arguments).output()?;
    let stderr = String::from_utf8(output.stderr).context("Docker CLI stderr was not UTF-8")?;
    ensure!(output.status.success(), "docker command failed: {stderr}");
    String::from_utf8(output.stdout).context("Docker CLI output was not UTF-8")
}

struct DockerCleanup {
    service_id: String,
}

impl DockerCleanup {
    fn new(service_id: String) -> Self {
        Self { service_id }
    }
}

impl Drop for DockerCleanup {
    fn drop(&mut self) {
        let filter = format!("label=ojos.service_id={}", self.service_id);
        let Ok(ids) = docker_command(["ps", "-aq", "--filter", &filter]) else {
            return;
        };
        for id in ids.lines().map(str::trim).filter(|id| !id.is_empty()) {
            let _ = Command::new("docker").args(["rm", "-f", id]).output();
        }
    }
}

#[derive(Default)]
struct EnvironmentGuard {
    original: Vec<(String, Option<OsString>)>,
}

impl EnvironmentGuard {
    fn remember(&mut self, name: &str) {
        if !self.original.iter().any(|(existing, _)| existing == name) {
            self.original
                .push((name.to_string(), std::env::var_os(name)));
        }
    }

    fn set(&mut self, name: &str, value: &str) {
        self.remember(name);
        // SAFETY: this integration test is a dedicated test executable and is
        // always invoked with --test-threads=1 by the required gate script.
        unsafe { std::env::set_var(name, value) };
    }

    fn remove(&mut self, name: &str) {
        self.remember(name);
        // SAFETY: see `set`; no other test runs in this process.
        unsafe { std::env::remove_var(name) };
    }
}

impl Drop for EnvironmentGuard {
    fn drop(&mut self) {
        for (name, value) in self.original.drain(..).rev() {
            // SAFETY: this process still contains only the dedicated live test.
            unsafe {
                match value {
                    Some(value) => std::env::set_var(name, value),
                    None => std::env::remove_var(name),
                }
            }
        }
    }
}
