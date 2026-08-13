//! Fail-closed, live contest-service vertical acceptance gate.
//!
//! The Linux harness builds the checked-in contest runtime and migration OCI
//! images, installs them through the real control-plane/Agent path, and then
//! supplies this test with the resulting deployment evidence.  Ordinary unit
//! test runs may skip the external dependency gate; the required workflow sets
//! `OJOS_REQUIRE_CONTEST_REAL_VERTICAL_E2E=1`, and every input below is then
//! mandatory.

#[path = "support/contest_real_vertical_fixtures.rs"]
mod contest_real_vertical_fixtures;

use contest_real_vertical_fixtures::{
    ProtocolMock, ProtocolMockKind, WorkloadKeys, endpoint_for, seed_live_sqlite,
    write_live_catalog,
};

use anyhow::{Context, Result, anyhow, bail, ensure};
use jsonwebtoken::{Algorithm, EncodingKey, Header, encode};
use orchestrator_agent::{
    AgentLedger, AgentWorker, BuiltInPipelineProviderConfig, BuiltInReleasePipelineProvider,
    JobExecutor, LocalRuntimeContextProvider, LoopbackHttpTransport, NodeRuntimeFactsPublisher,
    PipelineProviderConfig, PollOutcome, RuntimeContextProvider, WorkerConfig,
    WorkloadCredentialSupervisor,
    resource_claim::{
        FileResourceSecretStore, LivePostgreSqlExecutor, LocalResourceClaimManager,
        PostgreSqlAdminConfigV1, PostgreSqlProviderDescriptorV1, PostgreSqlTlsModeV1,
        PostgreSqlTlsTrustV1, SecretMaterial,
    },
};
use orchestrator_backend::{
    EmbeddedServerHandle, EmbeddedServerOptions, EmbeddedStorage, start_embedded_server,
};
use orchestrator_runtime::{
    DeploymentRuntimeObservationV1, DockerEngineRuntime, OciImageReference, WorkloadFileOwnership,
};
use orchestrator_storage::{SqliteOptions, SqliteOrchestratorStore};
use reqwest::{Certificate, Client, StatusCode, header::SET_COOKIE};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    ffi::OsString,
    fs,
    net::TcpListener,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

const NODE_ID: &str = "desktop-local";
const TOPOLOGY_ID: &str = "contest-real-vertical";
const CONTEST_SERVICE: &str = "contest-service";
const PROBLEM_SERVICE: &str = "problem-service";
const PROBLEM_API: &str = "problem.problem.read";
const PROBLEM_DEPLOYMENT: &str = "problem-provider-real-vertical";
const AUTH_SERVICE: &str = "auth-service";
const AUTH_API: &str = "auth.user.permission.check";
const AUTH_DEPLOYMENT: &str = "auth-provider-real-vertical";
const CONTEST_CREATED_EVENT_TYPE: &str = "contest-service.contest-created";
const CONTEST_CREATED_EVENT_SCHEMA: &str = "urn:ojos:event:contest-service.contest-created:v1";
const CATALOG_KEY_ID: &str = "contest-real-vertical-key";
const WORKLOAD_KEY_ID: &str = "contest-real-workload-1";
const WORKLOAD_ISSUER: &str = "ojos-auth/workload";
const WORKLOAD_AUDIENCE: &str = "ojos-gateway";
const INTERNAL_TOKEN: &str = "contest-real-orchestrator-internal-000001";
const GATEWAY_MANAGEMENT_TOKEN: &str = "contest-real-gateway-management-000001";
const GATEWAY_ACK_TOKEN: &str = "contest-real-gateway-ack-token-00000001";
const AUTH_ACK_TOKEN: &str = "contest-real-auth-ack-token-0000000001";
const AUTH_WORKLOAD_TOKEN: &str = "contest-real-auth-workload-token-000001";
const JWT_SECRET: &str = "contest-real-user-jwt-secret-0000000001";
const TERMINAL_OPERATION_STATUSES: &[&str] = &[
    "SUCCEEDED",
    "FAILED",
    "CANCELLED",
    "NEEDS_ATTENTION",
    "ROLLED_BACK",
];

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct LiveEvidenceV1 {
    schema_version: String,
    operation_id: String,
    deployment_id: String,
    resource_claim_id: String,
    resource_output_reference: String,
    migration_container_id: String,
    migration_image: String,
    runtime_container_id: String,
    context_generation: u64,
    binding_generation: u64,
    user_bundle_digest: String,
    admin_bundle_digest: String,
    user_bundle_path: String,
    admin_bundle_path: String,
    postgres_database: String,
    event_stream: String,
}

struct CreatedContestEvidence {
    id: i64,
    slug: String,
}

struct PublishedEventEvidence {
    event_id: String,
    contest_id: i64,
    slug: String,
}

struct LiveConfig {
    runtime_image: OciImageReference,
    migration_image: OciImageReference,
    gateway_origin: String,
    postgres_admin_url: String,
    redis_host_url: String,
    redis_runtime_url: String,
    evidence_path: PathBuf,
    driver_output: PathBuf,
    gateway_bin: PathBuf,
    gateway_http_port: u16,
    gateway_container_origin: String,
    artifact_origin: String,
    artifact_root: PathBuf,
    postgres_provider_host: String,
    postgres_provider_port: u16,
    postgres_ca_file: PathBuf,
    scratch_root: PathBuf,
    staged_repo_root: PathBuf,
    contract_source: PathBuf,
}

impl LiveConfig {
    fn from_env() -> Result<Self> {
        let staged_repo_root = canonical_env_directory("OJOS_CONTEST_E2E_STAGED_REPO_ROOT")?;
        let contract_source = canonical_env_file("OJOS_CONTEST_E2E_CONTRACT_SOURCE")?;
        let scratch_root = canonical_env_directory("OJOS_CONTEST_E2E_SCRATCH_ROOT")?;
        ensure!(
            contract_source.strip_prefix(&staged_repo_root)?
                == Path::new("services/contest-service/ojos.service.yaml"),
            "contest contract source must be the fixed entrypoint under the staged root"
        );
        ensure!(
            scratch_root.strip_prefix(&staged_repo_root)? == Path::new(".runtime"),
            "contest scratch root must be the fixed private subtree under the staged root"
        );
        Ok(Self {
            runtime_image: OciImageReference::parse(&required_env(
                "OJOS_CONTEST_E2E_RUNTIME_IMAGE",
            )?)
            .context("parse digest-pinned contest runtime image")?,
            migration_image: OciImageReference::parse(&required_env(
                "OJOS_CONTEST_E2E_MIGRATION_IMAGE",
            )?)
            .context("parse digest-pinned contest migration image")?,
            gateway_origin: required_https_origin("OJOS_CONTEST_E2E_GATEWAY_ORIGIN")?,
            postgres_admin_url: required_env("OJOS_CONTEST_E2E_POSTGRES_ADMIN_URL")?,
            redis_host_url: required_redis_url("OJOS_CONTEST_E2E_REDIS_HOST_URL", true)?,
            redis_runtime_url: required_redis_url("OJOS_CONTEST_E2E_REDIS_RUNTIME_URL", false)?,
            evidence_path: required_env("OJOS_CONTEST_E2E_EVIDENCE")?.into(),
            driver_output: required_env("OJOS_CONTEST_E2E_DRIVER_OUTPUT")?.into(),
            gateway_bin: required_env("OJOS_CONTEST_E2E_GATEWAY_BIN")?.into(),
            gateway_http_port: required_env("OJOS_CONTEST_E2E_GATEWAY_HTTP_PORT")?
                .parse()
                .context("parse Gateway HTTP port")?,
            gateway_container_origin: required_https_origin(
                "OJOS_CONTEST_E2E_GATEWAY_CONTAINER_ORIGIN",
            )?,
            artifact_origin: required_https_origin("OJOS_CONTEST_E2E_ARTIFACT_ORIGIN")?,
            artifact_root: required_env("OJOS_CONTEST_E2E_ARTIFACT_ROOT")?.into(),
            postgres_provider_host: required_env("OJOS_CONTEST_E2E_POSTGRES_PROVIDER_HOST")?,
            postgres_provider_port: required_env("OJOS_CONTEST_E2E_POSTGRES_PROVIDER_PORT")?
                .parse()
                .context("parse PostgreSQL provider port")?,
            postgres_ca_file: required_env("OJOS_CONTEST_E2E_POSTGRES_CA_FILE")?.into(),
            scratch_root,
            staged_repo_root,
            contract_source,
        })
    }
}

struct DriverEndpoints {
    orchestrator_origin: String,
    orchestrator_token: String,
    allow_token: String,
    deny_token: String,
}

struct LiveDriver {
    endpoints: DriverEndpoints,
    _guards: LiveDriverGuards,
}

struct LiveDriverGuards {
    _data: tempfile::TempDir,
    _server: EmbeddedServerHandle,
    _gateway: GatewayProcess,
    _problem: ProtocolMock,
    _auth: ProtocolMock,
    _agent: LiveAgent,
    _environment: EnvironmentGuard,
}

/// The driver deliberately lives in the same process as the assertions.  Its
/// only output is written after the real Agent completion and Gateway ACK have
/// both been observed, so a stale/pre-baked evidence file cannot satisfy CI.
async fn drive_real_install(config: &LiveConfig) -> Result<LiveDriver> {
    ensure!(
        config.scratch_root.is_absolute() && config.scratch_root.is_dir(),
        "live harness scratch root must be an existing absolute directory"
    );
    let data = tempfile::Builder::new()
        .prefix("contest-real-vertical-")
        .tempdir_in(&config.scratch_root)?;
    let web_root = data.path().join("web");
    let artifact_store = data.path().join("agent-artifacts");
    fs::create_dir_all(&web_root)?;
    fs::write(web_root.join("index.html"), "contest real vertical")?;

    let problem = ProtocolMock::spawn(ProtocolMockKind::Problem, None)?;
    let workload_keys = WorkloadKeys::new()?;
    let auth = ProtocolMock::spawn(
        ProtocolMockKind::Auth,
        Some(Arc::clone(&workload_keys.signing_key)),
    )?;
    let contest_port = free_local_port()?;
    let contest_endpoint = format!("127.0.0.1:{contest_port}:{CONTEST_SERVICE}");
    let problem_endpoint = endpoint_for(&problem, PROBLEM_SERVICE)?;
    let auth_endpoint = endpoint_for(&auth, AUTH_SERVICE)?;
    let database_path = data.path().join("orchestrator.db");
    let topology = seed_live_sqlite(
        &database_path,
        &problem_endpoint,
        &auth_endpoint,
        problem.origin(),
        auth.origin(),
    )?;
    let catalog = write_live_catalog(
        &config.staged_repo_root,
        &config.contract_source,
        data.path(),
        config,
    )?;

    let mut environment = EnvironmentGuard::default();
    environment.set("ORCHESTRATOR_CATALOG_TRUST_KEYS", &catalog.trust_json);
    environment.set("ORCHESTRATOR_CATALOG_SOURCES", &catalog.sources_json);
    environment.set(
        "ORCHESTRATOR_CATALOG_CA_FILE",
        path_text(&config.postgres_ca_file)?,
    );
    environment.set(
        "ORCHESTRATOR_GATEWAY_ADMIN_ORIGIN",
        &format!("http://127.0.0.1:{}", config.gateway_http_port),
    );
    environment.set("ORCHESTRATOR_GATEWAY_ADMIN_TOKEN", GATEWAY_MANAGEMENT_TOKEN);
    environment.set("ORCHESTRATOR_AUTH_ADMIN_ORIGIN", auth.origin());
    environment.set("ORCHESTRATOR_AUTH_ADMIN_TOKEN", GATEWAY_MANAGEMENT_TOKEN);
    environment.set("ORCHESTRATOR_AUTH_WORKLOAD_ORIGIN", auth.origin());
    environment.set("ORCHESTRATOR_AUTH_WORKLOAD_TOKEN", AUTH_WORKLOAD_TOKEN);
    environment.set(
        "ORCHESTRATOR_GATEWAY_WORKLOAD_ORIGIN",
        &config.gateway_container_origin,
    );
    environment.set(
        "ORCHESTRATOR_GATEWAY_WORKLOAD_CA_FILE",
        path_text(&config.postgres_ca_file)?,
    );
    environment.set(
        "ORCHESTRATOR_WORKLOAD_PUBLIC_KEY_FILE",
        path_text(&workload_keys.public_key_file)?,
    );
    environment.set("ORCHESTRATOR_WORKLOAD_KEY_ID", WORKLOAD_KEY_ID);
    environment.set("ORCHESTRATOR_WORKLOAD_ISSUER", WORKLOAD_ISSUER);
    environment.set("ORCHESTRATOR_WORKLOAD_AUDIENCE", WORKLOAD_AUDIENCE);
    environment.set("ORCHESTRATOR_PROVIDER_TIMEOUT_MS", "5000");
    environment.set("ORCHESTRATOR_MAX_WORKERS", "4");
    environment.set(
        "ORCHESTRATOR_CONTRIBUTION_GATEWAY_ACK_TOKEN_SHA256",
        &sha(GATEWAY_ACK_TOKEN),
    );
    environment.set(
        "ORCHESTRATOR_CONTRIBUTION_AUTH_ACK_TOKEN_SHA256",
        &sha(AUTH_ACK_TOKEN),
    );

    let desktop_bootstrap = format!("contest-real-web-{}", std::process::id());
    let agent_bootstrap = format!("contest-real-agent-{}", std::process::id());
    let server = start_server(
        &config.staged_repo_root,
        &web_root,
        &artifact_store,
        &database_path,
        &desktop_bootstrap,
        &agent_bootstrap,
    )?;
    let orchestrator_origin = format!("http://{}", server.local_addr());
    let session = DesktopSession::exchange(&orchestrator_origin, &desktop_bootstrap).await?;

    let gateway_config = write_gateway_config(data.path(), config.gateway_http_port)?;
    let mut gateway = GatewayProcess::spawn(
        config,
        &gateway_config,
        &orchestrator_origin,
        auth.origin(),
        &workload_keys.public_key_file,
    )?;
    gateway.wait_ready().await?;
    let mut agent = LiveAgent::new(
        config,
        data.path(),
        &orchestrator_origin,
        &agent_bootstrap,
        topology.provider_observations.clone(),
    )
    .await?;
    agent.publish_runtime_facts("bootstrap").await?;

    let base = json!({
        "service_id": CONTEST_SERVICE,
        "version": "0.1.0",
        "target_node_id": NODE_ID,
        "endpoint": contest_endpoint,
        "bindings": [
            {"name": PROBLEM_API, "provider_deployment_id": PROBLEM_DEPLOYMENT},
            {"name": AUTH_API, "provider_deployment_id": AUTH_DEPLOYMENT}
        ],
        "topology_id": TOPOLOGY_ID,
        "topology_etag": format!("\"{}\"", topology.applied_revision_id),
    });
    let unresolved = session
        .post_json(
            "/api/v1/store/releases:validate",
            "contest-real-validate-unresolved",
            &base,
            StatusCode::OK,
        )
        .await?;
    ensure!(
        unresolved.pointer("/data/valid") == Some(&json!(false)),
        "clean install must expose unresolved config: {unresolved}"
    );
    let plan_digest = required_pointer_str(&unresolved, "/data/composition_plan/planDigest")?;
    let graph_digest =
        required_pointer_str(&unresolved, "/data/composition_plan/releaseGraphDigest")?;
    let config_node = composition_node_id(&unresolved, "config")?;
    let inputs = json!({(config_node): {"config": {"registration": {"mode": "open"}}}});
    let mut install = base;
    install["plan_digest"] = json!(plan_digest);
    install["release_graph_digest"] = json!(graph_digest);
    install["inputs"] = inputs;
    let accepted = session
        .post_json(
            "/api/v1/store/releases:install",
            "contest-real-install",
            &install,
            StatusCode::ACCEPTED,
        )
        .await?;
    let operation_id = required_pointer_str(&accepted, "/data/operation_id")?;
    let deployment_id = required_pointer_str(&accepted, "/data/deployment_id")?;
    agent
        .drive_operation(&session, &operation_id, "install")
        .await?;
    agent.publish_runtime_facts("installed").await?;
    let contribution_snapshot = wait_active_contribution(&session, &deployment_id).await?;

    let operation = session
        .get_json(
            &format!("/api/v1/operations/{operation_id}"),
            StatusCode::OK,
        )
        .await?;
    let evidence = build_live_evidence(
        config,
        &database_path,
        &agent,
        &operation,
        &contribution_snapshot,
        operation_id,
        deployment_id,
    )?;
    let bytes = serde_json::to_vec_pretty(&evidence)?;
    fs::write(&config.driver_output, bytes)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&config.driver_output, fs::Permissions::from_mode(0o644))?;
    }

    Ok(LiveDriver {
        endpoints: DriverEndpoints {
            orchestrator_origin,
            orchestrator_token: INTERNAL_TOKEN.to_string(),
            allow_token: user_token(101)?,
            deny_token: user_token(202)?,
        },
        _guards: LiveDriverGuards {
            _data: data,
            _server: server,
            _gateway: gateway,
            _problem: problem,
            _auth: auth,
            _agent: agent,
            _environment: environment,
        },
    })
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn real_contest_runtime_resource_migration_binding_gateway_vertical() {
    if std::env::var("OJOS_REQUIRE_CONTEST_REAL_VERTICAL_E2E").as_deref() != Ok("1") {
        eprintln!(
            "skipping external contest vertical; run deploy/ops/contest-service-real-vertical-e2e.sh"
        );
        return;
    }
    run_gate()
        .await
        .expect("real contest-service vertical gate failed");
}

async fn run_gate() -> Result<()> {
    let config = LiveConfig::from_env()?;
    ensure!(
        config.driver_output == config.evidence_path,
        "driver output and assertion evidence paths must be identical"
    );
    ensure!(
        !config.evidence_path.exists(),
        "live evidence must not be pre-created by the caller"
    );
    let driver = drive_real_install(&config).await?;
    let evidence: LiveEvidenceV1 = serde_json::from_slice(
        &fs::read(&config.evidence_path)
            .with_context(|| format!("read {}", config.evidence_path.display()))?,
    )
    .context("decode live vertical evidence")?;
    validate_evidence(&evidence, &config)?;

    let docker = DockerEngineRuntime::connect_local().context("connect to Docker Engine")?;
    docker.ping().await.context("ping Docker Engine")?;
    let inventory = docker
        .managed_deployment_inventory(4_096)
        .await
        .context("inspect live Agent-managed runtime inventory")?;
    ensure!(
        inventory.inventory_complete,
        "Docker inventory was incomplete: {}",
        inventory.inventory_error
    );
    let runtime = inventory
        .deployments
        .iter()
        .find(|item| item.deployment_id == evidence.deployment_id)
        .context("live contest deployment missing from Docker inventory")?;
    ensure!(
        runtime.container_id == evidence.runtime_container_id,
        "control-plane/runtime container evidence disagrees"
    );
    ensure!(
        runtime.artifact_digest == config.runtime_image.to_string(),
        "running contest container is not the signed runtime digest"
    );
    ensure!(runtime.runtime_attested, "contest runtime was not attested");
    ensure!(
        runtime.health == "HEALTHY",
        "contest runtime is not healthy"
    );

    assert_control_plane_evidence(&driver.endpoints, &evidence).await?;
    let contest = assert_gateway_permission_and_crud(&config, &driver.endpoints, &evidence).await?;
    let event = assert_postgres_migration_and_outbox(&config, &evidence, &contest)?;
    assert_redis_event(&config, &evidence, &event)?;
    assert_frontend_artifacts(&config, &evidence).await?;
    Ok(())
}

fn validate_evidence(evidence: &LiveEvidenceV1, config: &LiveConfig) -> Result<()> {
    ensure!(
        evidence.schema_version == "ojos.dev/contest-real-vertical-evidence/v1",
        "unexpected evidence schema"
    );
    for (field, value) in [
        ("operation_id", &evidence.operation_id),
        ("deployment_id", &evidence.deployment_id),
        ("resource_claim_id", &evidence.resource_claim_id),
        (
            "resource_output_reference",
            &evidence.resource_output_reference,
        ),
        ("migration_container_id", &evidence.migration_container_id),
        ("migration_image", &evidence.migration_image),
        ("runtime_container_id", &evidence.runtime_container_id),
        ("postgres_database", &evidence.postgres_database),
        ("event_stream", &evidence.event_stream),
    ] {
        ensure!(!value.trim().is_empty(), "evidence omitted {field}");
    }
    ensure!(
        evidence.context_generation > 0 && evidence.binding_generation > 0,
        "service context/Binding generations must be materialized"
    );
    ensure!(
        evidence.event_stream == "ojos:events:v1",
        "contest must publish through the platform event stream"
    );
    ensure!(
        evidence
            .resource_output_reference
            .starts_with("agent-secret://resource-outputs/"),
        "ResourceClaim output is not an Agent-local resource-output reference"
    );
    for (name, digest) in [
        ("user", &evidence.user_bundle_digest),
        ("admin", &evidence.admin_bundle_digest),
    ] {
        ensure!(is_sha256(digest), "{name} frontend digest is not immutable");
    }
    ensure!(
        config.runtime_image.to_string().contains("@sha256:")
            && config.migration_image.to_string().contains("@sha256:"),
        "runtime and migration images must be digest-pinned"
    );
    ensure!(
        evidence.migration_image == config.migration_image.to_string(),
        "Agent ledger migration receipt is not bound to the signed migration OCI"
    );
    Ok(())
}

async fn assert_control_plane_evidence(
    endpoints: &DriverEndpoints,
    evidence: &LiveEvidenceV1,
) -> Result<()> {
    let client = client(None)?;
    let operation = json_response(
        client
            .get(format!(
                "{}/api/v1/operations/{}",
                endpoints.orchestrator_origin, evidence.operation_id
            ))
            .header("x-ojos-orchestrator-token", &endpoints.orchestrator_token)
            .send()
            .await?,
        StatusCode::OK,
        "operation evidence",
    )
    .await?;
    ensure!(
        operation.pointer("/data/operation/status") == Some(&Value::String("SUCCEEDED".into())),
        "install Operation did not succeed: {operation}"
    );
    let serialized = serde_json::to_string(&operation)?;
    ensure!(
        serialized.contains(&evidence.resource_claim_id),
        "Operation omitted the real ResourceClaim receipt"
    );
    ensure!(
        !serialized.contains("postgresql://"),
        "control-plane evidence leaked ResourceClaim credentials"
    );

    let snapshot = json_response(
        client
            .get(format!(
                "{}/api/v1/contributions/snapshot",
                endpoints.orchestrator_origin
            ))
            .header("x-ojos-orchestrator-token", &endpoints.orchestrator_token)
            .send()
            .await?,
        StatusCode::OK,
        "Contribution snapshot",
    )
    .await?;
    let snapshot_text = serde_json::to_string(&snapshot)?;
    ensure!(
        snapshot_text.contains(&evidence.deployment_id),
        "active Contribution does not target the live deployment"
    );
    ensure!(
        snapshot_text.contains(&evidence.user_bundle_digest)
            && snapshot_text.contains(&evidence.admin_bundle_digest),
        "active Contribution omitted signed frontend digests"
    );
    Ok(())
}

async fn assert_gateway_permission_and_crud(
    config: &LiveConfig,
    endpoints: &DriverEndpoints,
    evidence: &LiveEvidenceV1,
) -> Result<CreatedContestEvidence> {
    let client = client(Some(&config.postgres_ca_file))?;
    let denied = client
        .post(format!("{}/api/contests", config.gateway_origin))
        .bearer_auth(&endpoints.deny_token)
        .json(&serde_json::json!({
            "slug": "must-not-exist",
            "title": "must-not-exist",
            "description": "permission deny probe",
            "startsAt": "2026-08-13T00:00:00Z",
            "endsAt": "2026-08-14T00:00:00Z"
        }))
        .send()
        .await?;
    ensure!(
        denied.status() == StatusCode::FORBIDDEN,
        "Gateway permission deny returned {}, expected 403",
        denied.status()
    );

    let marker = format!("real-vertical-{}", evidence.deployment_id);
    let created = json_response(
        client
            .post(format!("{}/api/contests", config.gateway_origin))
            .bearer_auth(&endpoints.allow_token)
            .header("idempotency-key", &marker)
            .json(&serde_json::json!({
                "slug": marker,
                "title": marker,
                "description": "real vertical contest",
                "startsAt": "2026-08-13T00:00:00Z",
                "endsAt": "2026-08-14T00:00:00Z"
            }))
            .send()
            .await?,
        StatusCode::CREATED,
        "Gateway -> contest create",
    )
    .await?;
    let contest_id = created
        .pointer("/id")
        .and_then(Value::as_i64)
        .filter(|value| *value > 0)
        .context("contest create omitted positive id")?;
    let fetched = json_response(
        client
            .get(format!(
                "{}/api/contests/{contest_id}",
                config.gateway_origin
            ))
            .bearer_auth(&endpoints.allow_token)
            .send()
            .await?,
        StatusCode::OK,
        "Gateway -> contest get",
    )
    .await?;
    ensure!(
        fetched.pointer("/title") == Some(&Value::String(marker.clone())),
        "CRUD response did not come from the live contest database: {fetched}"
    );
    let deleted = client
        .delete(format!(
            "{}/api/contests/{contest_id}",
            config.gateway_origin
        ))
        .bearer_auth(&endpoints.allow_token)
        .send()
        .await?;
    ensure!(
        deleted.status() == StatusCode::NO_CONTENT,
        "Gateway -> contest delete returned {}, expected 204",
        deleted.status()
    );
    let gone = client
        .get(format!(
            "{}/api/contests/{contest_id}",
            config.gateway_origin
        ))
        .bearer_auth(&endpoints.allow_token)
        .send()
        .await?;
    ensure!(
        gone.status() == StatusCode::NOT_FOUND,
        "Gateway -> contest read-after-delete returned {}, expected 404",
        gone.status()
    );
    Ok(CreatedContestEvidence {
        id: contest_id,
        slug: marker,
    })
}

fn assert_postgres_migration_and_outbox(
    config: &LiveConfig,
    evidence: &LiveEvidenceV1,
    contest: &CreatedContestEvidence,
) -> Result<PublishedEventEvidence> {
    let query = format!(
        "SELECT event_id, event_type, payload->>'dataschema', \
         payload->'data'->>'contestId', payload->'data'->>'slug', \
         CASE WHEN published_at IS NULL THEN 'pending' ELSE 'published' END \
         FROM integration_outbox WHERE aggregate_type='contest' \
         AND aggregate_id='contest/{}' ORDER BY sequence DESC LIMIT 1",
        contest.id
    );
    let mut last_detail = String::from("no row");
    for _ in 0..120 {
        let output = Command::new("psql")
            .env("PGCONNECT_TIMEOUT", "5")
            .env("PGSSLROOTCERT", &config.postgres_ca_file)
            .arg(&config.postgres_admin_url)
            .args([
                "-XAt",
                "-F",
                "\t",
                "-d",
                &evidence.postgres_database,
                "-c",
                &query,
            ])
            .output()
            .context("execute live PostgreSQL migration/outbox assertion")?;
        ensure!(
            output.status.success(),
            "psql migration/outbox assertion failed: {}",
            std::str::from_utf8(&output.stderr).context("psql stderr must be UTF-8")?
        );
        let row = std::str::from_utf8(&output.stdout)
            .context("psql stdout must be UTF-8")?
            .trim()
            .to_string();
        let columns = row.split('\t').collect::<Vec<_>>();
        if columns.len() == 6 {
            ensure!(
                columns[1] == CONTEST_CREATED_EVENT_TYPE
                    && columns[2] == CONTEST_CREATED_EVENT_SCHEMA
                    && columns[3] == contest.id.to_string()
                    && columns[4] == contest.slug,
                "transactional outbox row drifted from the generated event contract: {columns:?}"
            );
            if columns[5] == "published" {
                ensure!(!columns[0].is_empty(), "outbox event_id is empty");
                return Ok(PublishedEventEvidence {
                    event_id: columns[0].to_string(),
                    contest_id: contest.id,
                    slug: contest.slug.clone(),
                });
            }
        }
        last_detail = row;
        std::thread::sleep(Duration::from_millis(250));
    }
    bail!(
        "migration/outbox row for contest {} was not published: {}",
        contest.id,
        last_detail
    )
}

fn assert_redis_event(
    config: &LiveConfig,
    evidence: &LiveEvidenceV1,
    expected: &PublishedEventEvidence,
) -> Result<()> {
    let mut last_output = String::new();
    for _ in 0..120 {
        let output = Command::new("redis-cli")
            .args([
                "-u",
                &config.redis_host_url,
                "--raw",
                "XREVRANGE",
                &evidence.event_stream,
                "+",
                "-",
                "COUNT",
                "256",
            ])
            .output()
            .context("execute Redis event assertion")?;
        ensure!(
            output.status.success(),
            "redis-cli assertion failed: {}",
            std::str::from_utf8(&output.stderr).context("redis-cli stderr must be UTF-8")?
        );
        last_output = std::str::from_utf8(&output.stdout)
            .context("redis-cli stdout must be UTF-8")?
            .to_string();
        if let Some(fields) = redis_entry_for_event(&last_output, &expected.event_id) {
            ensure!(
                fields.get("type").map(String::as_str) == Some(CONTEST_CREATED_EVENT_TYPE)
                    && fields.get("subject").map(String::as_str)
                        == Some(format!("contest/{}", expected.contest_id).as_str()),
                "Redis stream fields drifted from the outbox event: {fields:?}"
            );
            let envelope: Value = serde_json::from_str(
                fields
                    .get("event")
                    .context("Redis stream entry omitted CloudEvent envelope")?,
            )
            .context("decode relayed CloudEvent envelope")?;
            ensure!(
                envelope.get("id") == Some(&json!(&expected.event_id))
                    && envelope.get("type") == Some(&json!(CONTEST_CREATED_EVENT_TYPE))
                    && envelope.get("subject")
                        == Some(&json!(format!("contest/{}", expected.contest_id)))
                    && envelope.get("dataschema") == Some(&json!(CONTEST_CREATED_EVENT_SCHEMA))
                    && envelope.pointer("/data/contestId") == Some(&json!(expected.contest_id))
                    && envelope.pointer("/data/slug") == Some(&json!(&expected.slug)),
                "Redis CloudEvent is not the exact contest event committed in PostgreSQL: {envelope}"
            );
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    bail!(
        "Redis stream omitted published contest event {} (last bytes={})",
        expected.event_id,
        last_output.len()
    )
}

fn redis_entry_for_event(raw: &str, event_id: &str) -> Option<BTreeMap<String, String>> {
    let lines = raw.lines().collect::<Vec<_>>();
    let mut index = 0;
    while index < lines.len() {
        if !is_redis_stream_id(lines[index]) {
            index += 1;
            continue;
        }
        index += 1;
        let mut fields = BTreeMap::new();
        while index + 1 < lines.len() && !is_redis_stream_id(lines[index]) {
            fields.insert(lines[index].to_string(), lines[index + 1].to_string());
            index += 2;
        }
        if fields.get("event_id").map(String::as_str) == Some(event_id) {
            return Some(fields);
        }
    }
    None
}

fn is_redis_stream_id(value: &str) -> bool {
    value
        .split_once('-')
        .is_some_and(|(milliseconds, sequence)| {
            !milliseconds.is_empty()
                && !sequence.is_empty()
                && milliseconds.bytes().all(|byte| byte.is_ascii_digit())
                && sequence.bytes().all(|byte| byte.is_ascii_digit())
        })
}

async fn assert_frontend_artifacts(config: &LiveConfig, evidence: &LiveEvidenceV1) -> Result<()> {
    let client = client(Some(&config.postgres_ca_file))?;
    for (target, path, expected) in [
        (
            "user",
            &evidence.user_bundle_path,
            &evidence.user_bundle_digest,
        ),
        (
            "admin",
            &evidence.admin_bundle_path,
            &evidence.admin_bundle_digest,
        ),
    ] {
        ensure!(
            path.starts_with("/__ojos/extensions/"),
            "{target} bundle did not use the Gateway allowlist"
        );
        let response = client
            .get(format!("{}{}", config.gateway_origin, path))
            .send()
            .await?;
        ensure!(
            response.status() == StatusCode::OK,
            "{target} bundle allowlist returned {}",
            response.status()
        );
        let bytes = response.bytes().await?;
        ensure!(
            format!("sha256:{:x}", Sha256::digest(&bytes)) == *expected,
            "{target} bundle bytes did not match the signed digest"
        );
    }
    let rejected = client
        .get(format!(
            "{}/__ojos/extensions/{}/not-in-the-signed-manifest.js",
            config.gateway_origin,
            "0".repeat(64)
        ))
        .send()
        .await?;
    ensure!(
        matches!(
            rejected.status(),
            StatusCode::NOT_FOUND | StatusCode::FORBIDDEN
        ),
        "Gateway served an unsigned frontend path: {}",
        rejected.status()
    );
    Ok(())
}

fn client(extra_ca_file: Option<&Path>) -> Result<Client> {
    let mut builder = Client::builder()
        .no_proxy()
        .connect_timeout(Duration::from_secs(5))
        .timeout(Duration::from_secs(30))
        .danger_accept_invalid_certs(false);
    if let Some(path) = extra_ca_file {
        let pem = fs::read(path).with_context(|| format!("read TLS CA {}", path.display()))?;
        for certificate in Certificate::from_pem_bundle(&pem)
            .with_context(|| format!("decode TLS CA {}", path.display()))?
        {
            builder = builder.add_root_certificate(certificate);
        }
    }
    builder.build().context("build live gate HTTP client")
}

async fn json_response(
    response: reqwest::Response,
    expected: StatusCode,
    scope: &str,
) -> Result<Value> {
    let status = response.status();
    let bytes = response.bytes().await?;
    ensure!(
        status == expected,
        "{scope} returned {status}, expected {expected}: {}",
        std::str::from_utf8(&bytes).context("HTTP error body must be UTF-8")?
    );
    serde_json::from_slice(&bytes).with_context(|| format!("decode {scope} JSON"))
}

fn required_env(name: &str) -> Result<String> {
    let value =
        std::env::var(name).with_context(|| format!("{name} must be set by live harness"))?;
    ensure!(!value.trim().is_empty(), "{name} must not be empty");
    Ok(value)
}

fn required_https_origin(name: &str) -> Result<String> {
    let value = required_env(name)?;
    ensure!(value.starts_with("https://"), "{name} must use HTTPS");
    Ok(value.trim_end_matches('/').to_string())
}

fn required_redis_url(name: &str, require_loopback: bool) -> Result<String> {
    let value = required_env(name)?;
    let parsed = url::Url::parse(&value).with_context(|| format!("parse {name}"))?;
    ensure!(parsed.scheme() == "redis", "{name} must use redis://");
    ensure!(
        parsed.username().is_empty() && parsed.password().is_none(),
        "{name} must not contain credentials"
    );
    let host = parsed
        .host_str()
        .context(format!("{name} must contain a host"))?;
    ensure!(
        parsed.port().is_some(),
        "{name} must contain an explicit port"
    );
    ensure!(parsed.path() == "/0", "{name} must select Redis database 0");
    ensure!(
        parsed.query().is_none() && parsed.fragment().is_none(),
        "{name} must not contain a query or fragment"
    );
    if require_loopback {
        ensure!(host == "127.0.0.1", "{name} must use IPv4 loopback");
    } else {
        ensure!(
            host.parse::<std::net::Ipv4Addr>()?.is_private() && host != "127.0.0.1",
            "{name} must use a non-loopback private IPv4 bridge address"
        );
    }
    Ok(value)
}

fn is_sha256(value: &str) -> bool {
    value.len() == 71
        && value.starts_with("sha256:")
        && value[7..]
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
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
            "desktop exchange failed: {}",
            response.status()
        );
        let cookie = response
            .headers()
            .get(SET_COOKIE)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.split(';').next())
            .map(str::to_string)
            .context("desktop exchange omitted session cookie")?;
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
        internal_token: Some(INTERNAL_TOKEN.to_string()),
        desktop_bootstrap_secret: Some(desktop_bootstrap.to_string()),
        desktop_agent_secret: Some(agent_bootstrap.to_string()),
        storage: EmbeddedStorage::Sqlite {
            database_path: database_path.to_path_buf(),
        },
    })
}

fn canonical_env_directory(name: &str) -> Result<PathBuf> {
    let supplied = PathBuf::from(required_env(name)?);
    ensure!(supplied.is_absolute(), "{name} must be absolute");
    let metadata = fs::symlink_metadata(&supplied)
        .with_context(|| format!("inspect {name} {}", supplied.display()))?;
    ensure!(
        metadata.is_dir() && !metadata.file_type().is_symlink(),
        "{name} must be a non-symlink directory"
    );
    let canonical = fs::canonicalize(&supplied)
        .with_context(|| format!("canonicalize {name} {}", supplied.display()))?;
    ensure!(canonical == supplied, "{name} must already be canonical");
    Ok(canonical)
}

fn canonical_env_file(name: &str) -> Result<PathBuf> {
    let supplied = PathBuf::from(required_env(name)?);
    ensure!(supplied.is_absolute(), "{name} must be absolute");
    let metadata = fs::symlink_metadata(&supplied)
        .with_context(|| format!("inspect {name} {}", supplied.display()))?;
    ensure!(
        metadata.is_file() && !metadata.file_type().is_symlink(),
        "{name} must be a non-symlink file"
    );
    let canonical = fs::canonicalize(&supplied)
        .with_context(|| format!("canonicalize {name} {}", supplied.display()))?;
    ensure!(canonical == supplied, "{name} must already be canonical");
    Ok(canonical)
}

fn required_pointer_str(value: &Value, pointer: &str) -> Result<String> {
    value
        .pointer(pointer)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .ok_or_else(|| anyhow!("value omitted {pointer}: {value}"))
}

fn composition_node_id(response: &Value, kind: &str) -> Result<String> {
    response
        .pointer("/data/composition_plan/nodes")
        .and_then(Value::as_array)
        .and_then(|nodes| {
            nodes.iter().find(|node| {
                node.get("kind").and_then(Value::as_str) == Some(kind)
                    && node.get("serviceId").and_then(Value::as_str) == Some(CONTEST_SERVICE)
            })
        })
        .and_then(|node| node.get("nodeId"))
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| anyhow!("CompositionPlan omitted {kind} node"))
}

fn path_text(path: &Path) -> Result<&str> {
    path.to_str()
        .with_context(|| format!("path is not valid UTF-8: {}", path.display()))
}

fn sha(seed: &str) -> String {
    format!("sha256:{:x}", Sha256::digest(seed.as_bytes()))
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(i64::MAX)
}

fn free_local_port() -> Result<u16> {
    let listener = TcpListener::bind("127.0.0.1:0")?;
    Ok(listener.local_addr()?.port())
}

fn write_gateway_config(directory: &Path, port: u16) -> Result<PathBuf> {
    let path = directory.join("gateway.yml");
    let contents = format!(
        r#"Name: gateway-service
Host: 127.0.0.1
Port: {port}
Timeout: 600000
Middlewares:
  Timeout: false
  Recover: false
Database:
  Url: ""
Redis:
  Url: ""
Jaeger:
  Endpoint: ""
Jwt:
  Secret: ""
Storage:
  ProblemsRoot: ""
  SubmissionsRoot: ""
Proxy:
  TrustedServices: []
  Routes: []
ServiceStatus:
  ComposeServices: []
InternalAuth:
  Enabled: false
Orchestrator:
  Endpoint: ""
  InternalToken: ""
  ManagementToken: ""
  ContributionAckToken: ""
  NodeID: ""
AuthService:
  Endpoint: ""
WorkloadIdentity:
  PublicKeyFile: ""
  KeyID: ""
  Issuer: ""
  Audience: ""
"#
    );
    fs::write(&path, contents)?;
    Ok(path)
}

struct GatewayProcess {
    child: Child,
    origin: String,
}

struct LiveAgent {
    worker: AgentWorker<LoopbackHttpTransport, DockerEngineRuntime>,
    runtime: DockerEngineRuntime,
    runtime_provider: Arc<LocalRuntimeContextProvider>,
    _credentials: Arc<WorkloadCredentialSupervisor>,
    facts_publisher: orchestrator_agent::HttpNodeRuntimeFactsPublisher,
    provider_observations: Vec<DeploymentRuntimeObservationV1>,
    report_sequence: u64,
}

impl LiveAgent {
    async fn new(
        config: &LiveConfig,
        directory: &Path,
        orchestrator_origin: &str,
        bootstrap: &str,
        provider_observations: Vec<DeploymentRuntimeObservationV1>,
    ) -> Result<Self> {
        let runtime = DockerEngineRuntime::connect_local().context("connect real Agent runtime")?;
        runtime.ping().await.context("ping real Agent runtime")?;
        let docker_facts = runtime.runtime_facts().await?;
        let runtime_provider = Arc::new(
            LocalRuntimeContextProvider::standard_only(
                docker_facts,
                directory.join("runtime-contexts"),
            )
            .with_workload_file_ownership(WorkloadFileOwnership::standard_v3())?
            .with_event_connections(BTreeMap::from([(
                "shared-events".to_string(),
                config.redis_runtime_url.clone(),
            )])),
        );
        let transport =
            LoopbackHttpTransport::new_with_bootstrap(orchestrator_origin, bootstrap.to_string())?;
        let exchanger = Arc::new(transport.workload_credential_exchanger(NODE_ID.to_string())?);
        let credentials = Arc::new(WorkloadCredentialSupervisor::new(
            exchanger,
            Arc::clone(&runtime_provider) as Arc<dyn RuntimeContextProvider>,
        ));
        let postgres = LivePostgreSqlExecutor::new(PostgreSqlAdminConfigV1 {
            provider: PostgreSqlProviderDescriptorV1 {
                provider_id: "postgresql-local".to_string(),
                host: config.postgres_provider_host.clone(),
                port: config.postgres_provider_port,
                // Runtime containers receive sslmode=require; the Agent still
                // verifies the administrator channel with the pinned CA.
                tls_mode: PostgreSqlTlsModeV1::Require,
            },
            admin_url: SecretMaterial::new(config.postgres_admin_url.as_bytes().to_vec())?,
            tls_trust: PostgreSqlTlsTrustV1::CaCertificate(config.postgres_ca_file.clone()),
            state_database: directory.join("resource-postgres-receipts.sqlite3"),
        })
        .map_err(anyhow::Error::msg)?;
        let resources = LocalResourceClaimManager::new(
            PostgreSqlProviderDescriptorV1 {
                provider_id: "postgresql-local".to_string(),
                host: config.postgres_provider_host.clone(),
                port: config.postgres_provider_port,
                tls_mode: PostgreSqlTlsModeV1::Require,
            },
            postgres,
            FileResourceSecretStore::new_with_ownership(
                directory.join("resource-secrets"),
                WorkloadFileOwnership::standard_v3(),
            )
            .map_err(anyhow::Error::msg)?,
            directory.join("resource-claims.sqlite3"),
        )?;
        let pipeline = BuiltInReleasePipelineProvider::new(
            PipelineProviderConfig::managed_node(),
            BuiltInPipelineProviderConfig::new(directory.join("provider-state.sqlite3")),
        )?;
        let executor = JobExecutor::new(runtime.clone())
            .with_pipeline_provider(Arc::new(pipeline))
            .with_runtime_context(
                Arc::clone(&runtime_provider) as Arc<dyn RuntimeContextProvider>,
                Arc::clone(&credentials),
            )
            .with_resource_claims(Arc::new(resources));
        let ledger = AgentLedger::open(directory.join("agent-ledger.sqlite3"))?;
        let worker = AgentWorker::new(
            WorkerConfig {
                node_id: NODE_ID.to_string(),
                instance_id: format!("contest-real-agent-{}", std::process::id()),
                heartbeat_ms: 1_000,
                lease_ms: 30_000,
                transport_retry_ms: 100,
            },
            transport.clone(),
            executor,
            ledger,
        )?;
        Ok(Self {
            worker,
            runtime,
            runtime_provider,
            _credentials: credentials,
            facts_publisher: transport.runtime_facts_publisher(NODE_ID.to_string())?,
            provider_observations,
            report_sequence: 0,
        })
    }

    async fn publish_runtime_facts(&mut self, scope: &str) -> Result<()> {
        self.report_sequence += 1;
        let mut facts = self.runtime_provider.runtime_facts();
        let observed_at_ms = now_ms();
        facts.report_id = format!(
            "contest-real-{scope}-{}-{}",
            self.report_sequence, observed_at_ms
        );
        facts.observed_at_ms = observed_at_ms;
        let inventory = self.runtime.managed_deployment_inventory(4_096).await?;
        ensure!(
            inventory.inventory_complete,
            "real Docker inventory is incomplete: {}",
            inventory.inventory_error
        );
        facts.inventory_complete = true;
        facts.inventory_error.clear();
        facts.deployment_observations = inventory.deployments;
        for observation in &self.provider_observations {
            if !facts
                .deployment_observations
                .iter()
                .any(|item| item.deployment_id == observation.deployment_id)
            {
                facts.deployment_observations.push(observation.clone());
            }
        }
        self.facts_publisher
            .publish_runtime_facts(NODE_ID, &facts)
            .await?;
        Ok(())
    }

    async fn drive_operation(
        &mut self,
        session: &DesktopSession,
        operation_id: &str,
        scope: &str,
    ) -> Result<()> {
        for _ in 0..300 {
            let response = session
                .get_json(
                    &format!("/api/v1/operations/{operation_id}"),
                    StatusCode::OK,
                )
                .await?;
            let status = required_pointer_str(&response, "/data/operation/status")?;
            if TERMINAL_OPERATION_STATUSES.contains(&status.as_str()) {
                ensure!(
                    status == "SUCCEEDED",
                    "real Agent operation ended in {status}: {response}"
                );
                return Ok(());
            }
            acknowledge_auth_projection(session, scope).await?;
            match tokio::time::timeout(Duration::from_secs(120), self.worker.poll_once()).await {
                Ok(Ok(PollOutcome::Completed { .. } | PollOutcome::Idle { .. })) => {}
                Ok(Err(error)) => return Err(error).context("run real Agent worker"),
                Err(_) => bail!("real Agent worker timed out for operation {operation_id}"),
            }
            self.publish_runtime_facts(scope).await?;
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
        bail!("real Agent operation {operation_id} did not become terminal")
    }
}

async fn acknowledge_auth_projection(session: &DesktopSession, scope: &str) -> Result<()> {
    let snapshot = session
        .get_json("/api/v1/contributions/snapshot", StatusCode::OK)
        .await?;
    let data = snapshot
        .get("data")
        .cloned()
        .context("Contribution snapshot omitted data")?;
    let digest = required_pointer_str(&data, "/digest")?;
    let scope_id = required_pointer_str(&data, "/scope_id")?;
    let acknowledgements = data
        .get("acknowledgements")
        .cloned()
        .unwrap_or_else(|| json!([]));
    let response = session
        .client
        .post(format!(
            "{}/api/v1/contributions/projections:ack",
            session.origin
        ))
        .header("x-ojos-orchestrator-token", INTERNAL_TOKEN)
        .header("x-ojos-contribution-ack-token", AUTH_ACK_TOKEN)
        .header(
            "idempotency-key",
            format!("contest-real-{scope}-auth-{digest}"),
        )
        .json(&json!({
            "schema_version": "ojos.dev/contribution-projection-ack/v1",
            "target": "AUTH",
            "scope_id": scope_id,
            "snapshot_digest": digest,
            "acknowledgements": acknowledgements,
        }))
        .send()
        .await?;
    ensure!(
        response.status().is_success() || response.status() == StatusCode::CONFLICT,
        "Auth projection ACK failed: {}",
        response.status()
    );
    Ok(())
}

async fn wait_active_contribution(session: &DesktopSession, deployment_id: &str) -> Result<Value> {
    for _ in 0..240 {
        let snapshot = session
            .get_json("/api/v1/contributions/snapshot", StatusCode::OK)
            .await?;
        let active = snapshot
            .pointer("/data/revisions")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .find(|revision| {
                revision.get("service_id") == Some(&json!(CONTEST_SERVICE))
                    && revision.get("deployment_id") == Some(&json!(deployment_id))
                    && revision.get("runtime_ready") == Some(&json!(true))
            });
        let gateway_ready = snapshot
            .pointer("/data/gateway_routes")
            .and_then(Value::as_array)
            .is_some_and(|routes| !routes.is_empty());
        let user_ready = snapshot
            .pointer("/data/user_frontend_modules")
            .and_then(Value::as_array)
            .is_some_and(|modules| !modules.is_empty());
        let admin_ready = snapshot
            .pointer("/data/admin_frontend_modules")
            .and_then(Value::as_array)
            .is_some_and(|modules| !modules.is_empty());
        if active.is_some() && gateway_ready && user_ready && admin_ready {
            return Ok(snapshot);
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    bail!("live contest Contribution did not become active")
}

fn build_live_evidence(
    _config: &LiveConfig,
    database_path: &Path,
    agent: &LiveAgent,
    operation: &Value,
    contribution_snapshot: &Value,
    operation_id: String,
    deployment_id: String,
) -> Result<LiveEvidenceV1> {
    use orchestrator_agent::resource_claim::{ResourceClaimIdentityV1, ResourceClaimV1};
    use orchestrator_runtime::ResourceClaimStepV1;

    let durable = operation
        .pointer("/data/operation")
        .context("operation response omitted durable operation")?;
    let pipeline_jobs = durable
        .get("planned_jobs")
        .and_then(Value::as_array)
        .context("operation omitted planned_jobs")?
        .iter()
        .filter(|job| job.get("kind") == Some(&json!("release_pipeline")))
        .collect::<Vec<_>>();
    ensure!(
        pipeline_jobs.len() == 1,
        "operation must contain exactly one ReleasePipeline job: {pipeline_jobs:?}"
    );
    let payload = pipeline_jobs[0]
        .get("payload")
        .context("ReleasePipeline job omitted payload")?;
    let claim_value = payload
        .pointer("/resource_claims/0")
        .or_else(|| payload.pointer("/install/resource_claims/0"))
        .context("ReleasePipeline omitted PostgreSQL ResourceClaim")?;
    let step: ResourceClaimStepV1 = serde_json::from_value(claim_value.clone())?;
    let claim = ResourceClaimV1::requested(
        ResourceClaimIdentityV1 {
            claim_id: step.claim_id.clone(),
            owner_instance_id: step.owner_instance_id.clone(),
            service_id: step.service_id.clone(),
            resource_name: step.resource_name.clone(),
            resource_type: step.resource_type.clone(),
        },
        step.generation,
        step.provider_id,
    )?;
    let migration = agent
        .worker
        .ledger()
        .migration(CONTEST_SERVICE, "contest-schema-v1")?
        .context("Agent ledger omitted signed contest migration")?;
    ensure!(
        migration.state == "SUCCEEDED",
        "contest migration did not succeed: {}",
        migration.state
    );
    let store = SqliteOrchestratorStore::open_with_options(
        database_path,
        SqliteOptions {
            acquire_instance_lock: false,
            ..SqliteOptions::default()
        },
    )?;
    let runtime_container_id = store
        .runtime_instance(&deployment_id)?
        .map(|runtime| runtime.instance.container_id)
        .context("control plane omitted the real runtime container identity")?;
    let bindings = store.api_bindings_for_deployment(&deployment_id)?;
    ensure!(
        bindings.len() == 2,
        "real deployment omitted its two API Bindings"
    );
    let binding_generation = bindings
        .iter()
        .map(|binding| binding.context_generation)
        .min()
        .unwrap_or_default();
    let binding_providers = bindings
        .iter()
        .map(|binding| {
            (
                binding.api_id.as_str(),
                binding.provider_deployment_id.as_str(),
            )
        })
        .collect::<BTreeMap<_, _>>();
    ensure!(
        bindings
            .iter()
            .all(|binding| binding.context_generation > 0)
            && binding_providers.len() == 2
            && binding_providers.get(PROBLEM_API) == Some(&PROBLEM_DEPLOYMENT)
            && binding_providers.get(AUTH_API) == Some(&AUTH_DEPLOYMENT),
        "real API Binding projection is incomplete: {bindings:?}"
    );

    // Read frontend delivery fields from the active, deployment-scoped
    // snapshot consumed by the real Gateway. This proves the published head,
    // rather than merely finding a staged candidate in an Operation payload.
    let snapshot_module = |field: &str, target: &str| -> Result<&Value> {
        let matches = contribution_snapshot
            .pointer(&format!("/data/{field}"))
            .and_then(Value::as_array)
            .context("Contribution snapshot omitted frontend module array")?
            .iter()
            .filter(|module| {
                module.get("target") == Some(&json!(target))
                    && module.get("service_id") == Some(&json!(CONTEST_SERVICE))
                    && module.get("deployment_id") == Some(&json!(&deployment_id))
                    && module.get("enabled") == Some(&json!(true))
            })
            .collect::<Vec<_>>();
        ensure!(
            matches.len() == 1,
            "active Contribution must contain exactly one {target} module: {matches:?}"
        );
        Ok(matches[0])
    };
    let user = snapshot_module("user_frontend_modules", "user-shell")?;
    let admin = snapshot_module("admin_frontend_modules", "admin-shell")?;
    let frontend = |module: &Value| -> Result<(String, String)> {
        let digest = required_pointer_str(module, "/bundle_digest")?;
        let artifact = required_pointer_str(module, "/artifact")?;
        Ok((
            digest.clone(),
            format!(
                "/__ojos/extensions/{}/{}",
                digest.trim_start_matches("sha256:"),
                artifact
            ),
        ))
    };
    let (user_bundle_digest, user_bundle_path) = frontend(user)?;
    let (admin_bundle_digest, admin_bundle_path) = frontend(admin)?;
    Ok(LiveEvidenceV1 {
        schema_version: "ojos.dev/contest-real-vertical-evidence/v1".to_string(),
        operation_id,
        deployment_id,
        resource_claim_id: step.claim_id,
        resource_output_reference: claim.output_secret_reference(),
        migration_container_id: migration
            .container_id
            .context("Agent ledger omitted real migration container identity")?,
        migration_image: migration.image,
        runtime_container_id,
        context_generation: binding_generation,
        binding_generation,
        user_bundle_digest,
        admin_bundle_digest,
        user_bundle_path,
        admin_bundle_path,
        postgres_database: claim.postgres_names()?.database_name,
        event_stream: "ojos:events:v1".to_string(),
    })
}

impl GatewayProcess {
    fn spawn(
        config: &LiveConfig,
        gateway_config: &Path,
        orchestrator_origin: &str,
        auth_origin: &str,
        workload_public_key: &Path,
    ) -> Result<Self> {
        let origin = format!("http://127.0.0.1:{}", config.gateway_http_port);
        let child = Command::new(&config.gateway_bin)
            .args(["-f", path_text(gateway_config)?])
            .env("OJOS_PLATFORM_BOOTSTRAP", "1")
            .env("OJOS_ENVIRONMENT", "production")
            .env("REDIS_URL", &config.redis_host_url)
            .env("JWT_SECRET", JWT_SECRET)
            .env("ORCHESTRATOR_PLATFORM_ORIGIN", orchestrator_origin)
            .env("ORCHESTRATOR_INTERNAL_TOKEN", INTERNAL_TOKEN)
            .env("ORCHESTRATOR_GATEWAY_ADMIN_TOKEN", GATEWAY_MANAGEMENT_TOKEN)
            .env(
                "ORCHESTRATOR_CONTRIBUTION_GATEWAY_ACK_TOKEN",
                GATEWAY_ACK_TOKEN,
            )
            .env("ORCHESTRATOR_NODE_ID", NODE_ID)
            .env("AUTH_SERVICE_ENDPOINT", auth_origin)
            .env(
                "OJOS_WORKLOAD_PUBLIC_KEY_FILE",
                path_text(workload_public_key)?,
            )
            .env("OJOS_WORKLOAD_KEY_ID", WORKLOAD_KEY_ID)
            .env("OJOS_WORKLOAD_ISSUER", WORKLOAD_ISSUER)
            .env("OJOS_WORKLOAD_AUDIENCE", WORKLOAD_AUDIENCE)
            .env("JAEGER_ENDPOINT", "")
            .stdin(Stdio::null())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .spawn()
            .with_context(|| {
                format!("start real Gateway binary {}", config.gateway_bin.display())
            })?;
        Ok(Self { child, origin })
    }

    async fn wait_ready(&mut self) -> Result<()> {
        let client = Client::builder()
            .no_proxy()
            .connect_timeout(Duration::from_millis(500))
            .timeout(Duration::from_secs(2))
            .build()?;
        for _ in 0..120 {
            if let Some(status) = self.child.try_wait()? {
                bail!("real Gateway exited before readiness: {status}");
            }
            if client
                .get(format!("{}/readyz", self.origin))
                .send()
                .await
                .is_ok_and(|response| response.status().is_success())
            {
                return Ok(());
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
        bail!("real Gateway did not become ready at {}", self.origin)
    }
}

impl Drop for GatewayProcess {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn user_token(user_id: i64) -> Result<String> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let claims = json!({
        "iss": "ojos-auth",
        "sub": user_id.to_string(),
        "user_id": user_id,
        "username": format!("contest-e2e-{user_id}"),
        "roles": ["user"],
        "iat": now,
        "nbf": now.saturating_sub(1),
        "exp": now + 900,
    });
    encode(
        &Header::new(Algorithm::HS256),
        &claims,
        &EncodingKey::from_secret(JWT_SECRET.as_bytes()),
    )
    .context("sign external Gateway test token")
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
        // SAFETY: this file is a dedicated integration-test executable.
        unsafe { std::env::set_var(name, value) };
    }
}

impl Drop for EnvironmentGuard {
    fn drop(&mut self) {
        for (name, value) in self.original.drain(..).rev() {
            // SAFETY: no other test shares this dedicated executable.
            unsafe {
                match value {
                    Some(value) => std::env::set_var(name, value),
                    None => std::env::remove_var(name),
                }
            }
        }
    }
}
