//! Protocol-level fixtures used by the live contest-service vertical gate.
//!
//! These fixtures intentionally stop at the protocol boundary: the Gateway,
//! orchestrator, Agent, PostgreSQL, Redis and contest-service process remain
//! the production implementations exercised by the test.

use anyhow::{Context, Result, anyhow, ensure};
use base64::{
    Engine as _,
    engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD},
};
use ed25519_dalek::{Signer, SigningKey};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs,
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, JoinHandle},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use ojos_service::{
    ServiceContractV3, compile,
    publish::{CatalogPublishOptions, publish_catalog_v2},
    seal::{
        RESOLVED_ARTIFACTS_SCHEMA_VERSION, ResolvedArtifactV1, ResolvedArtifactsV1,
        artifact_requirements, seal,
    },
};
use orchestrator_agent::NodeRuntimeFactsV1;
use orchestrator_legacy::{
    NodeRecord, OrchestratorStore, ServiceRelease, ServiceReleaseManifest, TopologyEndpointSpec,
    TopologySpec,
};
use orchestrator_manager::catalog_v2::{CatalogModuleV2, CatalogV2, Ed25519Signature};
use orchestrator_runtime::{
    DeploymentRuntimeObservationV1, DockerRuntimeFacts, RuntimeContract, RuntimeDesiredState,
    RuntimeInstance, RuntimeObservedState,
};
use orchestrator_storage::{
    RuntimeManagementMode, SqliteOrchestratorStore, StoredNodeRuntimeFacts, StoredRuntimeInstance,
    TopologyApplyOutcome,
};
use semver::Version;

use super::{
    AUTH_API, AUTH_DEPLOYMENT, AUTH_SERVICE, AUTH_WORKLOAD_TOKEN, CATALOG_KEY_ID, CONTEST_SERVICE,
    LiveConfig, NODE_ID, PROBLEM_API, PROBLEM_DEPLOYMENT, PROBLEM_SERVICE, TOPOLOGY_ID,
    WORKLOAD_AUDIENCE, WORKLOAD_ISSUER, WORKLOAD_KEY_ID,
};

const POLICY_DIGEST: &str =
    "sha256:9999999999999999999999999999999999999999999999999999999999999999";

pub(crate) struct CatalogFixture {
    pub(crate) trust_json: String,
    pub(crate) sources_json: String,
}

pub(crate) struct SeededTopology {
    pub(crate) applied_revision_id: String,
    pub(crate) provider_observations: Vec<DeploymentRuntimeObservationV1>,
}

pub(crate) fn write_live_catalog(
    repo_root: &std::path::Path,
    directory: &std::path::Path,
    config: &LiveConfig,
) -> Result<CatalogFixture> {
    let source = repo_root.join("services/contest-service/ojos.service.yaml");
    let contract = compile(&source).context("compile checked-in contest service contract")?;
    ensure!(
        contract.service_id == CONTEST_SERVICE,
        "unexpected reference service"
    );
    let signing_key = ephemeral_signing_key()?;
    let key_file = directory.join("contest-signing-key.txt");
    fs::write(&key_file, STANDARD.encode(signing_key.to_bytes()))?;
    let final_catalog_dir = directory.join("catalog-fixture");
    let metadata_dir = final_catalog_dir.join("metadata");
    fs::create_dir_all(&metadata_dir)?;
    let resolved = live_resolved_artifacts(&contract, config)?;
    let lock = seal(&contract, &resolved)?;
    let report = publish_catalog_v2(
        &contract,
        &lock,
        &source,
        &CatalogPublishOptions {
            output_directory: directory.join("published-contest-real"),
            signing_key_file: key_file,
            public_base_url: config.artifact_origin.clone(),
            key_id: CATALOG_KEY_ID.to_string(),
            catalog_id: "contest-real-vertical".to_string(),
            min_orchestrator_version: Version::parse("0.1.0")?,
            target_os: "linux".to_string(),
            target_arch: "x86_64".to_string(),
        },
    )?;
    let published: CatalogV2 = serde_json::from_slice(&fs::read(&report.catalog)?)?;
    let mut release = published.modules[0].releases[0].clone();
    let metadata_name = "contest-0.1.0.release.json";
    fs::copy(&report.metadata, metadata_dir.join(metadata_name))?;
    release.metadata.url = format!("metadata/{metadata_name}");
    let mut catalog = CatalogV2 {
        schema_version: 2,
        id: "contest-real-vertical".to_string(),
        name: "Contest real vertical v3 fixture".to_string(),
        modules: vec![CatalogModuleV2 {
            id: CONTEST_SERVICE.to_string(),
            name: "Contest Service".to_string(),
            description: "signed live Service Contract v3 vertical gate".to_string(),
            kind: "backend-api".to_string(),
            tags: vec!["e2e".to_string(), "service-contract-v3".to_string()],
            releases: vec![release],
        }],
        signatures: Vec::new(),
    };
    let signature = signing_key.sign(&catalog.signing_payload_jcs()?);
    catalog.signatures.push(Ed25519Signature {
        key_id: CATALOG_KEY_ID.to_string(),
        algorithm: "Ed25519".to_string(),
        signature: STANDARD.encode(signature.to_bytes()),
    });
    catalog.validate()?;
    let catalog_path = final_catalog_dir.join("catalog.json");
    fs::write(&catalog_path, serde_json::to_vec_pretty(&catalog)?)?;
    let relative = catalog_path
        .strip_prefix(repo_root)?
        .to_str()
        .context("catalog fixture path must be UTF-8")?
        .replace('\\', "/");
    Ok(CatalogFixture {
        trust_json: serde_json::to_string(&BTreeMap::from([(
            CATALOG_KEY_ID,
            STANDARD.encode(signing_key.verifying_key().to_bytes()),
        )]))?,
        sources_json: serde_json::to_string(&json!([{
            "id": "contest-real-vertical",
            "url": relative,
            "required_key_id": CATALOG_KEY_ID,
            "auth_secret_ref": "",
            "enabled": true,
            "offline_oci_layouts": {}
        }]))?,
    })
}

fn live_resolved_artifacts(
    contract: &ServiceContractV3,
    config: &LiveConfig,
) -> Result<ResolvedArtifactsV1> {
    let migration_slots = contract
        .migrations
        .iter()
        .map(|migration| migration.artifact.as_str())
        .collect::<std::collections::BTreeSet<_>>();
    let mut artifacts = BTreeMap::new();
    for requirement in artifact_requirements(contract)? {
        let (digest, size, reference, media_type) = if requirement.slot == contract.runtime.artifact
        {
            (
                image_digest(&config.runtime_image.to_string())?,
                1,
                config.runtime_image.to_string(),
                "application/vnd.oci.image.manifest.v1+json".to_string(),
            )
        } else if migration_slots.contains(requirement.slot.as_str()) {
            (
                image_digest(&config.migration_image.to_string())?,
                1,
                config.migration_image.to_string(),
                "application/vnd.oci.image.manifest.v1+json".to_string(),
            )
        } else if matches!(
            requirement.slot.as_str(),
            "contest-user-frontend" | "contest-admin-frontend"
        ) {
            let filename = if requirement.slot == "contest-user-frontend" {
                "contest-user.js"
            } else {
                "contest-admin.js"
            };
            let bytes = fs::read(config.artifact_root.join(filename))?;
            let digest = format!("sha256:{:x}", Sha256::digest(&bytes));
            let hex = digest.trim_start_matches("sha256:");
            let relative = PathBuf::from("sha256").join(hex).join(&requirement.slot);
            let destination = config.artifact_root.join(&relative);
            fs::create_dir_all(destination.parent().context("bundle destination parent")?)?;
            fs::write(&destination, &bytes)?;
            let relative = relative
                .to_str()
                .context("bundle artifact path must be valid UTF-8")?
                .replace('\\', "/");
            (
                digest,
                bytes.len() as u64,
                format!("{}/{}", config.artifact_origin, relative),
                "application/javascript".to_string(),
            )
        } else {
            let digest = requirement.expected_digest.unwrap_or_else(|| {
                format!(
                    "sha256:{:x}",
                    Sha256::digest(format!("contest-real\0{}", requirement.slot))
                )
            });
            (
                digest.clone(),
                requirement.expected_size.unwrap_or(1),
                format!(
                    "{}/sha256/{}/{}",
                    config.artifact_origin,
                    digest.trim_start_matches("sha256:"),
                    requirement.slot
                ),
                "application/octet-stream".to_string(),
            )
        };
        artifacts.insert(
            requirement.slot,
            ResolvedArtifactV1 {
                media_type,
                digest,
                size,
                reference: Some(reference),
            },
        );
    }
    Ok(ResolvedArtifactsV1 {
        schema_version: RESOLVED_ARTIFACTS_SCHEMA_VERSION.to_string(),
        artifacts,
    })
}

fn image_digest(reference: &str) -> Result<String> {
    reference
        .rsplit_once('@')
        .map(|(_, digest)| digest.to_string())
        .filter(|digest| digest.starts_with("sha256:") && digest.len() == 71)
        .context("OCI image reference must be digest-pinned")
}

pub(crate) fn seed_live_sqlite(
    database_path: &std::path::Path,
    problem_endpoint: &str,
    auth_endpoint: &str,
    _problem_origin: &str,
    _auth_origin: &str,
) -> Result<SeededTopology> {
    let mut store = SqliteOrchestratorStore::open(database_path)?;
    store.upsert_node(NodeRecord {
        node_id: NODE_ID.to_string(),
        host_ip: "127.0.0.1".to_string(),
        parent_node_id: String::new(),
        role: "standalone".to_string(),
        labels: json!({
            "runtime": "docker",
            "providers": {
                "postgresql": {"enabled": true, "provider_id": "postgresql-local"},
                "redis": {"enabled": true, "connection_id": "shared-events"},
                "migration": {"enabled": true},
                "materialization": {"enabled": true, "secret_provider": "file"}
            }
        }),
        status: "READY".to_string(),
        created_at: "unix-ms:1".to_string(),
        updated_at: "unix-ms:1".to_string(),
    })?;
    for (service, manifest, checksum) in [
        (
            PROBLEM_SERVICE,
            problem_release_manifest()?,
            sha("problem-metadata"),
        ),
        (AUTH_SERVICE, auth_release_manifest()?, sha("auth-metadata")),
    ] {
        store.upsert_service_release(ServiceRelease {
            service_name: service.to_string(),
            version: "1.0.0".to_string(),
            release_url: format!("https://fixtures.invalid/{service}.release.json"),
            manifest: serde_json::to_value(manifest)?,
            checksum,
            created_at: "unix-ms:1".to_string(),
        })?;
    }
    let observed_at = now_unix_ms().try_into().unwrap_or(i64::MAX);
    let problem = provider_runtime(PROBLEM_DEPLOYMENT, PROBLEM_SERVICE, "problem-provider");
    let auth = provider_runtime(AUTH_DEPLOYMENT, AUTH_SERVICE, "auth-provider");
    store.put_runtime_instance(&stored_runtime(
        problem.clone(),
        problem_endpoint,
        observed_at,
    ))?;
    store.put_runtime_instance(&stored_runtime(auth.clone(), auth_endpoint, observed_at))?;
    let provider_observations = vec![runtime_observation(&problem), runtime_observation(&auth)];
    store.put_node_runtime_facts(&StoredNodeRuntimeFacts {
        node_id: NODE_ID.to_string(),
        observed_at_ms: observed_at,
        received_at_ms: observed_at,
        facts: serde_json::to_value(runtime_facts(observed_at, provider_observations.clone()))?,
    })?;
    let spec = TopologySpec::new(
        TOPOLOGY_ID,
        problem_endpoint,
        "private",
        vec![
            TopologyEndpointSpec {
                endpoint: problem_endpoint.to_string(),
                service_id: PROBLEM_SERVICE.to_string(),
                protocol: "http".to_string(),
                health_path: "/readyz".to_string(),
                display_name: "Problem API".to_string(),
                note: "protocol-level healthy API provider".to_string(),
                config: json!({"deployment_id": PROBLEM_DEPLOYMENT, "node_id": NODE_ID}),
            },
            TopologyEndpointSpec {
                endpoint: auth_endpoint.to_string(),
                service_id: AUTH_SERVICE.to_string(),
                protocol: "http".to_string(),
                health_path: "/readyz".to_string(),
                display_name: "Auth permission API".to_string(),
                note: "protocol-level permission/workload provider".to_string(),
                config: json!({"deployment_id": AUTH_DEPLOYMENT, "node_id": NODE_ID}),
            },
        ],
        Vec::new(),
    )?;
    let revision = store.create_initial_topology_revision(
        spec,
        "unix-ms:1",
        "contest-real-seed",
        "pre-existing protocol providers",
    )?;
    store.begin_topology_apply(
        TOPOLOGY_ID,
        revision.revision_id(),
        "seed-real-topology",
        "unix-ms:1",
    )?;
    store.finish_topology_apply(
        TOPOLOGY_ID,
        revision.revision_id(),
        "seed-real-topology",
        TopologyApplyOutcome::Succeeded,
        "unix-ms:2",
    )?;
    Ok(SeededTopology {
        applied_revision_id: revision.revision_id().to_string(),
        provider_observations,
    })
}

fn problem_release_manifest() -> Result<ServiceReleaseManifest> {
    Ok(serde_json::from_value(json!({
        "schema_version": 1,
        "service_name": PROBLEM_SERVICE,
        "version": "1.0.0",
        "description": "protocol-level exact API provider",
        "service_type": "backend-api",
        "source": {"kind":"url","url":"https://fixtures.invalid/problem.release.json","checksum":sha("problem-metadata")},
        "runtime": {"kind":"image","image":immutable_image("problem","problem-runtime"),"command":"","args":[],"env":{}},
        "backend": {"protocol":"http","port":8083,"health_path":"/readyz"},
        "migrations": [],
        "permissions": ["problem.view"],
        "routes": [],
        "apis": [{
            "api_id": PROBLEM_API, "protocol":"http", "port_name":"http",
            "path_prefix":"/problems", "methods":["GET"], "visibility":"explicit",
            "auth_mode":"workload", "permission":"problem.view",
            "stability":"stable", "version":"1.0.0"
        }],
        "redis": [], "storage": [], "dependencies": [], "required_apis": [],
        "config_schema": {}, "secrets": []
    }))?)
}

fn auth_release_manifest() -> Result<ServiceReleaseManifest> {
    Ok(serde_json::from_value(json!({
        "schema_version": 1,
        "service_name": AUTH_SERVICE,
        "version": "1.0.0",
        "description": "protocol-level permission provider",
        "service_type": "backend-api",
        "source": {"kind":"url","url":"https://fixtures.invalid/auth.release.json","checksum":sha("auth-metadata")},
        "runtime": {"kind":"image","image":immutable_image("auth","auth-runtime"),"command":"","args":[],"env":{}},
        "backend": {"protocol":"http","port":8081,"health_path":"/readyz"},
        "migrations": [], "permissions": ["auth.permission.check"], "routes": [],
        "apis": [{
            "api_id": AUTH_API, "protocol":"http", "port_name":"http",
            "path_prefix":"/auth/admin/permission-check", "methods":["POST"], "visibility":"explicit",
            "auth_mode":"workload", "permission":"auth.permission.check", "stability":"stable",
            "version":"1.0.0"
        }],
        "redis": [], "storage": [], "dependencies": [], "required_apis": [],
        "config_schema": {}, "secrets": []
    }))?)
}

fn provider_runtime(deployment: &str, service: &str, artifact_seed: &str) -> RuntimeInstance {
    RuntimeInstance {
        deployment_id: deployment.to_string(),
        service_id: service.to_string(),
        release_version: "1.0.0".to_string(),
        container_id: format!("protocol-{service}"),
        artifact_digest: immutable_image(service, artifact_seed),
        runtime_contract: RuntimeContract::standard_v1(),
        runtime_policy_sha256: POLICY_DIGEST.to_string(),
        effective_runtime_sha256: sha(&format!("effective-{service}")),
        runtime_attested: true,
        desired_state: RuntimeDesiredState::Running,
        observed_state: RuntimeObservedState::Running,
        health: "HEALTHY".to_string(),
    }
}

fn stored_runtime(instance: RuntimeInstance, endpoint: &str, at_ms: i64) -> StoredRuntimeInstance {
    StoredRuntimeInstance {
        node_id: NODE_ID.to_string(),
        instance,
        management_mode: RuntimeManagementMode::Managed,
        endpoint: endpoint.to_string(),
        external_probe_protocol: String::new(),
        external_probe_health_path: String::new(),
        last_observed_at_ms: at_ms,
        drift_reason: String::new(),
        credential_expires_at_ms: 0,
        credential_last_success_at_ms: 0,
        credential_last_error: String::new(),
        updated_at: format!("unix-ms:{at_ms}"),
    }
}

fn runtime_observation(instance: &RuntimeInstance) -> DeploymentRuntimeObservationV1 {
    DeploymentRuntimeObservationV1 {
        deployment_id: instance.deployment_id.clone(),
        service_id: instance.service_id.clone(),
        container_id: instance.container_id.clone(),
        artifact_digest: instance.artifact_digest.clone(),
        runtime_contract: instance.runtime_contract.clone(),
        runtime_policy_sha256: instance.runtime_policy_sha256.clone(),
        effective_runtime_sha256: instance.effective_runtime_sha256.clone(),
        observed_state: instance.observed_state.clone(),
        health: instance.health.clone(),
        runtime_attested: true,
        drift_reason: String::new(),
    }
}

fn runtime_facts(
    at_ms: i64,
    observations: Vec<DeploymentRuntimeObservationV1>,
) -> NodeRuntimeFactsV1 {
    NodeRuntimeFactsV1 {
        schema_version: 1,
        report_id: format!("contest-real-seed-{at_ms}"),
        observed_at_ms: at_ms,
        agent_version: "contest-real-protocol-fixture/1".to_string(),
        runtime_policy_sha256: POLICY_DIGEST.to_string(),
        allowed_contracts: vec![RuntimeContract::standard_v1()],
        judge_sandbox_allowed_images: Vec::new(),
        redis_connection_ids: vec!["shared-events".to_string()],
        docker: DockerRuntimeFacts {
            engine: "docker".to_string(),
            server_version: "live-agent-pending".to_string(),
            operating_system: "Linux".to_string(),
            os_type: "linux".to_string(),
            architecture: "x86_64".to_string(),
            cgroup_version: "2".to_string(),
            memory_limit: true,
            pids_limit: true,
            rootless: false,
            apparmor: false,
            seccomp: true,
            security_options: vec!["seccomp".to_string()],
        },
        inventory_complete: true,
        inventory_error: String::new(),
        deployment_observations: observations,
        credential_statuses: Vec::new(),
    }
}

fn immutable_image(name: &str, seed: &str) -> String {
    format!("registry.invalid/ojos/{name}@{}", sha(seed))
}

fn sha(seed: &str) -> String {
    format!("sha256:{:x}", Sha256::digest(seed.as_bytes()))
}

/// Deterministic Ed25519 test identity shared by the Auth protocol fixture and
/// the production Gateway verifier. The public key file is removed when the
/// fixture falls out of scope; both production consumers read it eagerly.
pub(crate) struct WorkloadKeys {
    pub(crate) signing_key: Arc<SigningKey>,
    pub(crate) public_key_file: PathBuf,
}

impl WorkloadKeys {
    pub(crate) fn new() -> Result<Self> {
        let signing_key = Arc::new(ephemeral_signing_key()?);
        let unique = format!(
            "ojos-contest-real-workload-{}-{}.pem",
            std::process::id(),
            now_unix_ms()
        );
        let public_key_file = std::env::temp_dir().join(unique);
        fs::write(
            &public_key_file,
            public_key_pem(signing_key.verifying_key().as_bytes()),
        )
        .with_context(|| format!("write {}", public_key_file.display()))?;
        Ok(Self {
            signing_key,
            public_key_file,
        })
    }
}

fn ephemeral_signing_key() -> Result<SigningKey> {
    let mut seed = [0_u8; 32];
    getrandom::fill(&mut seed)
        .map_err(|error| anyhow!("generate ephemeral Ed25519 signing seed: {error}"))?;
    Ok(SigningKey::from_bytes(&seed))
}

impl Drop for WorkloadKeys {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.public_key_file);
    }
}

fn public_key_pem(raw: &[u8; 32]) -> String {
    // SubjectPublicKeyInfo for id-Ed25519 (OID 1.3.101.112).
    let mut der = vec![
        0x30, 0x2a, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70, 0x03, 0x21, 0x00,
    ];
    der.extend_from_slice(raw);
    let encoded = STANDARD.encode(der);
    let mut pem = String::from("-----BEGIN PUBLIC KEY-----\n");
    for chunk in encoded.as_bytes().chunks(64) {
        pem.push_str(std::str::from_utf8(chunk).expect("base64 is ASCII"));
        pem.push('\n');
    }
    pem.push_str("-----END PUBLIC KEY-----\n");
    pem
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ProtocolMockKind {
    Problem,
    Auth,
}

#[derive(Debug, Default)]
struct ProviderState {
    revision_id: Option<String>,
    content_sha256: Option<String>,
    projection_sha256: Option<String>,
    absent: bool,
}

/// A bounded raw-HTTP protocol fixture. It provides only APIs belonging to the
/// external Problem/Auth dependencies; no contest or platform behavior is
/// simulated here.
pub(crate) struct ProtocolMock {
    kind: ProtocolMockKind,
    origin: String,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<Result<()>>>,
}

impl ProtocolMock {
    pub(crate) fn spawn(
        kind: ProtocolMockKind,
        signing_key: Option<Arc<SigningKey>>,
    ) -> Result<Self> {
        ensure!(
            kind == ProtocolMockKind::Auth || signing_key.is_none(),
            "only the Auth fixture accepts a workload signing key"
        );
        ensure!(
            kind != ProtocolMockKind::Auth || signing_key.is_some(),
            "Auth fixture requires a workload signing key"
        );
        let listener = TcpListener::bind("127.0.0.1:0")?;
        listener.set_nonblocking(true)?;
        let origin = format!("http://{}", listener.local_addr()?);
        let stop = Arc::new(AtomicBool::new(false));
        let thread_stop = Arc::clone(&stop);
        let provider_state = Arc::new(Mutex::new(ProviderState {
            absent: true,
            ..ProviderState::default()
        }));
        let thread_state = Arc::clone(&provider_state);
        let thread = thread::spawn(move || {
            let run = || -> Result<()> {
                while !thread_stop.load(Ordering::Acquire) {
                    let (mut stream, _) = match listener.accept() {
                        Ok(value) => value,
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            thread::sleep(Duration::from_millis(5));
                            continue;
                        }
                        Err(error) => return Err(error.into()),
                    };
                    stream.set_nonblocking(false)?;
                    stream.set_read_timeout(Some(Duration::from_secs(3)))?;
                    let request = read_request(&mut stream)?;
                    let result = match kind {
                        ProtocolMockKind::Problem => handle_problem(&mut stream, &request),
                        ProtocolMockKind::Auth => handle_auth(
                            &mut stream,
                            &request,
                            signing_key.as_deref().expect("validated Auth key"),
                            &thread_state,
                        ),
                    };
                    if let Err(error) = result {
                        let _ = write_json(
                            &mut stream,
                            500,
                            &json!({"error": "protocol fixture failure", "detail": error.to_string()}),
                        );
                        return Err(error);
                    }
                }
                Ok(())
            };
            let result = run();
            if let Err(error) = &result {
                eprintln!("contest real vertical {kind:?} fixture stopped: {error:#}");
            }
            result
        });
        Ok(Self {
            kind,
            origin,
            stop,
            thread: Some(thread),
        })
    }

    pub(crate) fn origin(&self) -> &str {
        &self.origin
    }
}

impl Drop for ProtocolMock {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        // Wake the nonblocking accept loop promptly instead of adding teardown
        // latency to every CI run.
        if let Ok(url) = url::Url::parse(&self.origin)
            && let Some(port) = url.port()
        {
            let _ = TcpStream::connect(("127.0.0.1", port));
        }
        if let Some(thread) = self.thread.take()
            && let Ok(Err(error)) = thread.join()
        {
            eprintln!(
                "contest real vertical {:?} fixture teardown: {error:#}",
                self.kind
            );
        }
    }
}

pub(crate) fn endpoint_for(mock: &ProtocolMock, service_id: &str) -> Result<String> {
    let parsed = url::Url::parse(mock.origin()).context("parse protocol mock origin")?;
    let port = parsed
        .port_or_known_default()
        .context("protocol mock origin omitted port")?;
    ensure!(
        !service_id.trim().is_empty() && !service_id.contains(':'),
        "invalid service id for topology endpoint"
    );
    Ok(format!("127.0.0.1:{port}:{}", service_id.trim()))
}

fn handle_problem(stream: &mut TcpStream, request: &HttpRequest) -> Result<()> {
    match (request.method.as_str(), request.path_without_query()) {
        ("GET", "/readyz") | ("GET", "/healthz") => {
            write_json(stream, 200, &json!({"status": "ok"}))
        }
        ("GET", "/problems") => write_json(
            stream,
            200,
            &json!({"code": 0, "msg": "", "data": {"items": [], "total": 0}}),
        ),
        _ => write_json(stream, 404, &json!({"error": "not found"})),
    }
}

fn handle_auth(
    stream: &mut TcpStream,
    request: &HttpRequest,
    signing_key: &SigningKey,
    provider_state: &Arc<Mutex<ProviderState>>,
) -> Result<()> {
    let path = request.path_without_query();
    match (request.method.as_str(), path) {
        ("GET", "/readyz") | ("GET", "/healthz") => {
            write_json(stream, 200, &json!({"status": "ok"}))
        }
        ("POST", "/auth/internal/workload-tokens:issue") => {
            let authorized = request
                .headers
                .get("authorization")
                .is_some_and(|value| value == &format!("Bearer {AUTH_WORKLOAD_TOKEN}"));
            if !authorized {
                return write_json(
                    stream,
                    403,
                    &json!({"type":"about:blank","title":"Forbidden","status":403}),
                );
            }
            let body: Value = serde_json::from_slice(&request.body)?;
            let deployment_id = required_string(&body, "deployment_id")?;
            let service_id = required_string(&body, "service_id")?;
            let node_id = required_string(&body, "node_id")?;
            let generation = body
                .get("credential_generation")
                .and_then(Value::as_u64)
                .filter(|value| *value > 0)
                .context("workload issue omitted positive credential_generation")?;
            let now = now_unix_seconds();
            let expires = now + 15 * 60;
            let token = sign_workload_jwt(
                signing_key,
                deployment_id,
                service_id,
                node_id,
                generation,
                now,
                expires,
            )?;
            write_json(
                stream,
                200,
                &json!({
                    "access_token": token,
                    "token_type": "Bearer",
                    "expires_at": rfc3339(expires)?,
                    "expires_in": 15 * 60
                }),
            )
        }
        ("POST", "/auth/permission-check") | ("POST", "/auth/admin/permission-check") => {
            let body: Value = serde_json::from_slice(&request.body)?;
            let user_id = body.get("user_id").and_then(Value::as_i64).unwrap_or(0);
            write_json(
                stream,
                200,
                &json!({"code": 0, "msg": "", "data": {"allowed": user_id == 101}}),
            )
        }
        ("GET", path) if path.starts_with("/api/v1/topologies/") => {
            observe_provider(stream, path, provider_state)
        }
        ("PUT", path) | ("DELETE", path) if path.starts_with("/api/v1/topologies/") => {
            apply_provider(stream, request, provider_state)
        }
        _ => write_json(stream, 404, &json!({"error": "not found", "path": path})),
    }
}

fn observe_provider(
    stream: &mut TcpStream,
    path: &str,
    provider_state: &Arc<Mutex<ProviderState>>,
) -> Result<()> {
    let topology_id = path.rsplit('/').next().unwrap_or_default();
    let state = provider_state
        .lock()
        .map_err(|_| anyhow!("Auth provider state poisoned"))?;
    if state.absent {
        return write_json(
            stream,
            200,
            &json!({
                "api_version": "v1",
                "provider": "auth",
                "topology_id": topology_id,
                "absent": true,
                "endpoints": [],
                "links": []
            }),
        );
    }
    write_json(
        stream,
        200,
        &json!({
            "api_version": "v1",
            "provider": "auth",
            "topology_id": topology_id,
            "observed_revision_id": state.revision_id,
            "observed_content_sha256": state.content_sha256,
            "observed_projection_sha256": state.projection_sha256,
            "absent": false,
            "endpoints": [],
            "links": []
        }),
    )
}

fn apply_provider(
    stream: &mut TcpStream,
    request: &HttpRequest,
    provider_state: &Arc<Mutex<ProviderState>>,
) -> Result<()> {
    let body: Value = serde_json::from_slice(&request.body)?;
    ensure!(
        body.get("provider") == Some(&json!("auth")),
        "wrong provider"
    );
    let action = required_string(&body, "action")?;
    let absent = action == "delete";
    let projection_sha256 = if absent {
        None
    } else {
        Some(projection_digest(&body["routes"], &body["grants"])?)
    };
    {
        let mut state = provider_state
            .lock()
            .map_err(|_| anyhow!("Auth provider state poisoned"))?;
        state.revision_id = body
            .get("desired_revision_id")
            .and_then(Value::as_str)
            .map(str::to_string);
        state.content_sha256 = body
            .get("desired_content_sha256")
            .and_then(Value::as_str)
            .map(str::to_string);
        state.projection_sha256 = projection_sha256;
        state.absent = absent;
    }
    write_json(
        stream,
        200,
        &json!({
            "api_version": "v1",
            "provider": "auth",
            "action": action,
            "topology_id": body["topology_id"],
            "operation_id": body["operation_id"],
            "completed": true,
            "observed_revision_id": body.get("desired_revision_id"),
            "observed_content_sha256": body.get("desired_content_sha256"),
            "absent": absent
        }),
    )
}

fn projection_digest(routes: &Value, grants: &Value) -> Result<String> {
    let mut projection = ProviderProjection {
        routes: serde_json::from_value(routes.clone()).context("decode Auth provider routes")?,
        grants: serde_json::from_value(grants.clone()).context("decode Auth provider grants")?,
    };
    projection
        .routes
        .sort_by(|left, right| left.binding_id.cmp(&right.binding_id));
    projection
        .grants
        .sort_by(|left, right| left.binding_id.cmp(&right.binding_id));
    // Go hashes a typed struct, preserving declaration order for every field.
    // Hashing Value maps here would sort object keys and falsely report drift.
    let encoded = serde_json::to_vec(&projection)?;
    Ok(format!("{:x}", Sha256::digest(encoded)))
}

#[derive(Deserialize, Serialize)]
struct ProviderProjection {
    routes: Vec<ProviderBindingRoute>,
    grants: Vec<ProviderBindingGrant>,
}

#[derive(Deserialize, Serialize)]
struct ProviderBindingRoute {
    binding_id: String,
    requirement_name: String,
    consumer_deployment_id: String,
    consumer_service_id: String,
    consumer_node_id: String,
    credential_generation: u64,
    api_id: String,
    provider_deployment_id: String,
    provider_service_id: String,
    provider_node_id: String,
    provider_endpoint: String,
    upstream_base: String,
    provider_path: String,
    virtual_path: String,
    auth_mode: String,
    provider_auth_mode: String,
    permission: String,
    methods: Vec<String>,
    timeout_ms: u64,
}

#[derive(Deserialize, Serialize)]
struct ProviderBindingGrant {
    binding_id: String,
    requirement_name: String,
    consumer_deployment_id: String,
    consumer_service_id: String,
    consumer_node_id: String,
    credential_generation: u64,
    api_id: String,
    permission: String,
}

fn sign_workload_jwt(
    key: &SigningKey,
    deployment_id: &str,
    service_id: &str,
    node_id: &str,
    credential_generation: u64,
    issued_at: u64,
    expires_at: u64,
) -> Result<String> {
    let header = json!({"alg": "EdDSA", "kid": WORKLOAD_KEY_ID, "typ": "JWT"});
    let claims = json!({
        "deployment_id": deployment_id,
        "service_id": service_id,
        "node_id": node_id,
        "credential_generation": credential_generation,
        "iss": WORKLOAD_ISSUER,
        "sub": deployment_id,
        "aud": [WORKLOAD_AUDIENCE],
        "exp": expires_at,
        "nbf": issued_at.saturating_sub(5),
        "iat": issued_at,
        "jti": URL_SAFE_NO_PAD.encode(Sha256::digest(
            format!("{deployment_id}\0{credential_generation}\0{issued_at}").as_bytes(),
        ))
    });
    let signing_input = format!(
        "{}.{}",
        URL_SAFE_NO_PAD.encode(serde_json::to_vec(&header)?),
        URL_SAFE_NO_PAD.encode(serde_json::to_vec(&claims)?),
    );
    let signature = key.sign(signing_input.as_bytes());
    Ok(format!(
        "{signing_input}.{}",
        URL_SAFE_NO_PAD.encode(signature.to_bytes())
    ))
}

fn rfc3339(unix_seconds: u64) -> Result<String> {
    let timestamp = time::OffsetDateTime::from_unix_timestamp(unix_seconds as i64)?;
    Ok(timestamp.format(&time::format_description::well_known::Rfc3339)?)
}

fn required_string<'a>(value: &'a Value, name: &str) -> Result<&'a str> {
    value
        .get(name)
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .with_context(|| format!("request omitted {name}"))
}

struct HttpRequest {
    method: String,
    path: String,
    headers: BTreeMap<String, String>,
    body: Vec<u8>,
}

impl HttpRequest {
    fn path_without_query(&self) -> &str {
        self.path.split('?').next().unwrap_or(&self.path)
    }
}

fn read_request(stream: &mut TcpStream) -> Result<HttpRequest> {
    const MAX_REQUEST: usize = 8 * 1024 * 1024;
    let mut bytes = Vec::new();
    let header_end = loop {
        let mut chunk = [0_u8; 4096];
        let read = stream.read(&mut chunk)?;
        ensure!(read > 0, "request closed before headers");
        bytes.extend_from_slice(&chunk[..read]);
        ensure!(bytes.len() <= MAX_REQUEST, "request too large");
        if let Some(index) = bytes.windows(4).position(|part| part == b"\r\n\r\n") {
            break index + 4;
        }
    };
    let head = std::str::from_utf8(&bytes[..header_end])?;
    let mut lines = head.split("\r\n");
    let mut request_line = lines
        .next()
        .context("missing request line")?
        .split_whitespace();
    let method = request_line.next().context("missing method")?.to_string();
    let path = request_line.next().context("missing path")?.to_string();
    let headers = lines
        .filter_map(|line| line.split_once(':'))
        .map(|(name, value)| (name.trim().to_ascii_lowercase(), value.trim().to_string()))
        .collect::<BTreeMap<_, _>>();
    let content_length = headers
        .get("content-length")
        .map(|value| value.parse::<usize>())
        .transpose()?
        .unwrap_or_default();
    ensure!(
        header_end + content_length <= MAX_REQUEST,
        "request too large"
    );
    while bytes.len() < header_end + content_length {
        let mut chunk = [0_u8; 4096];
        let read = stream.read(&mut chunk)?;
        ensure!(read > 0, "request closed before body");
        bytes.extend_from_slice(&chunk[..read]);
        ensure!(bytes.len() <= MAX_REQUEST, "request too large");
    }
    Ok(HttpRequest {
        method,
        path,
        headers,
        body: bytes[header_end..header_end + content_length].to_vec(),
    })
}

fn write_json(stream: &mut TcpStream, status: u16, body: &Value) -> Result<()> {
    let bytes = serde_json::to_vec(body)?;
    let reason = match status {
        200 => "OK",
        403 => "Forbidden",
        404 => "Not Found",
        500 => "Internal Server Error",
        _ => "Response",
    };
    write!(
        stream,
        "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nCache-Control: no-store\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        bytes.len()
    )?;
    stream.write_all(&bytes)?;
    stream.flush()?;
    Ok(())
}

fn now_unix_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn now_unix_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}
