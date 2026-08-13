//! Clean-room gate for the signed contest-service v3 developer-platform path.
//!
//! This intentionally uses SQLite plus protocol-level fake Agent/provider
//! implementations. It exercises the real Catalog registry, HTTP APIs,
//! durable Operation DAG, Topology projection, Contribution controller and
//! runtime evidence without requiring Docker or PostgreSQL on a PR runner.

use anyhow::{Context, Result, anyhow, bail, ensure};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use ed25519_dalek::{Signer, SigningKey};
use ojos_service::{
    ServiceContractV3, compile,
    publish::{CatalogPublishOptions, publish_catalog_v2},
    seal::{
        RESOLVED_ARTIFACTS_SCHEMA_VERSION, ResolvedArtifactV1, ResolvedArtifactsV1,
        artifact_requirements, seal,
    },
};
use orchestrator_agent::{ClaimResponse, LeasedJob, NodeRuntimeFactsV1};
use orchestrator_backend::{
    EmbeddedServerHandle, EmbeddedServerOptions, EmbeddedStorage, start_embedded_server,
};
use orchestrator_control_plane::{CompletionStatus, JobKind, OperationRepository};
use orchestrator_legacy::{
    NodeRecord, OrchestratorStore, ServiceRelease, ServiceReleaseManifest, TopologyEndpointSpec,
    TopologySpec,
};
use orchestrator_manager::catalog_v2::{
    CatalogModuleV2, CatalogReleaseV2, CatalogV2, Ed25519Signature,
};
use orchestrator_runtime::{
    DeploymentRuntimeObservationV1, DockerRuntimeFacts, OciImageReference, RuntimeContract,
    RuntimeDesiredState, RuntimeInstance, RuntimeObservedState,
};
use orchestrator_storage::{
    ContributionRepository, RuntimeManagementMode, SqliteOptions, SqliteOrchestratorStore,
    StoredNodeRuntimeFacts, StoredRuntimeInstance, TopologyApplyOutcome,
};
use reqwest::{Client, StatusCode, header::SET_COOKIE};
use semver::Version;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::OsString,
    fs,
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, JoinHandle},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

const NODE_ID: &str = "desktop-local";
const TOPOLOGY_ID: &str = "contest-clean-room";
const CONTEST_SERVICE: &str = "contest-service";
const PROBLEM_SERVICE: &str = "problem-service";
const PROBLEM_API: &str = "problem.problem.read";
const PROBLEM_DEPLOYMENT: &str = "problem-provider-1";
const PROBLEM_ENDPOINT: &str = "127.0.0.1:18083:problem-service";
const AUTH_SERVICE: &str = "auth-service";
const AUTH_API: &str = "auth.user.permission.check";
const AUTH_DEPLOYMENT: &str = "auth-provider-1";
const AUTH_ENDPOINT: &str = "127.0.0.1:18081:auth-service";
const CONTEST_ENDPOINT: &str = "127.0.0.1:18080:contest-service";
const CATALOG_KEY_ID: &str = "contest-clean-room-key";
const INTERNAL_TOKEN: &str = "contest-clean-room-internal-token-00000001";
const GATEWAY_ACK_TOKEN: &str = "contest-clean-room-gateway-ack-token-0001";
const AUTH_ACK_TOKEN: &str = "contest-clean-room-auth-ack-token-0000001";
const POLICY_DIGEST: &str =
    "sha256:9999999999999999999999999999999999999999999999999999999999999999";
const EMPTY_PROJECTION_DIGEST: &str =
    "fa9d28278a0d02b19bfebeae5afd5aa6dde1c685d8396acc8defe8832848865c";
const TERMINAL_OPERATION_STATUSES: &[&str] = &[
    "SUCCEEDED",
    "FAILED",
    "CANCELLED",
    "NEEDS_ATTENTION",
    "ROLLED_BACK",
];
const CAPABILITIES: &[&str] = &[
    "install",
    "release_pipeline",
    "upgrade",
    "start",
    "stop",
    "restart",
    "uninstall",
    "rollback",
    "health",
    "binding_context_apply",
    "resource_purge",
];

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn signed_contest_v3_composes_and_runs_durable_clean_room_lifecycle() {
    run_gate()
        .await
        .expect("contest-service clean-room gate failed");
}

async fn run_gate() -> Result<()> {
    let repo_root = workspace_root()?;
    let scratch_root = repo_root.join(".tmp");
    fs::create_dir_all(&scratch_root)?;
    let data = tempfile::Builder::new()
        .prefix("contest-clean-room-")
        .tempdir_in(&scratch_root)?;
    let web_root = data.path().join("web");
    fs::create_dir_all(&web_root)?;
    fs::write(web_root.join("index.html"), "contest clean room")?;
    let database_path = data.path().join("orchestrator.db");
    let artifact_root = data.path().join("artifacts");

    let catalog = write_signed_contest_catalog(&repo_root, data.path())?;
    let topology = seed_sqlite(&database_path)?;
    let gateway = MockManagementProvider::spawn("gateway")?;
    let auth = MockManagementProvider::spawn("auth")?;

    let mut environment = EnvironmentGuard::default();
    environment.set("ORCHESTRATOR_CATALOG_TRUST_KEYS", &catalog.trust_json);
    environment.set("ORCHESTRATOR_CATALOG_SOURCES", &catalog.sources_json);
    environment.set("ORCHESTRATOR_GATEWAY_ADMIN_ORIGIN", gateway.origin());
    environment.set("ORCHESTRATOR_AUTH_ADMIN_ORIGIN", auth.origin());
    environment.set("ORCHESTRATOR_AUTH_ADMIN_TOKEN", "contest-e2e-token");
    environment.set(
        "ORCHESTRATOR_GATEWAY_WORKLOAD_ORIGIN",
        "http://127.0.0.1:18000",
    );
    let workload_key_path = data.path().join("workload-public-key.pem");
    fs::write(
        &workload_key_path,
        "-----BEGIN PUBLIC KEY-----\nMCowBQYDK2VwAyEAERERERERERERERERERERERERERERERERERERERERERE=\n-----END PUBLIC KEY-----\n",
    )?;
    environment.set(
        "ORCHESTRATOR_WORKLOAD_PUBLIC_KEY_FILE",
        workload_key_path
            .to_str()
            .context("clean-room workload key path is not UTF-8")?,
    );
    environment.set("ORCHESTRATOR_WORKLOAD_KEY_ID", "workload-1");
    environment.set("ORCHESTRATOR_WORKLOAD_ISSUER", "ojos-auth/workload");
    environment.set("ORCHESTRATOR_WORKLOAD_AUDIENCE", "ojos-gateway");
    environment.set("ORCHESTRATOR_PROVIDER_TIMEOUT_MS", "2000");
    environment.set("ORCHESTRATOR_MAX_WORKERS", "4");
    environment.set(
        "ORCHESTRATOR_CONTRIBUTION_GATEWAY_ACK_TOKEN_SHA256",
        &sha(GATEWAY_ACK_TOKEN),
    );
    environment.set(
        "ORCHESTRATOR_CONTRIBUTION_AUTH_ACK_TOKEN_SHA256",
        &sha(AUTH_ACK_TOKEN),
    );
    for name in [
        "ORCHESTRATOR_DATABASE_URL",
        "ORCHESTRATOR_GATEWAY_ADMIN_TOKEN",
        "ORCHESTRATOR_AUTH_WORKLOAD_ORIGIN",
        "ORCHESTRATOR_AUTH_WORKLOAD_TOKEN",
        "ORCHESTRATOR_GATEWAY_WORKLOAD_CA_FILE",
    ] {
        environment.remove(name);
    }

    let agent_bootstrap = format!("contest-agent-{}", std::process::id());
    let desktop_bootstrap = format!("contest-web-{}", std::process::id());
    let server = start_server(
        &repo_root,
        &web_root,
        &artifact_root,
        &database_path,
        &desktop_bootstrap,
        &agent_bootstrap,
    )?;
    let origin = format!("http://{}", server.local_addr());
    let session = DesktopSession::exchange(&origin, &desktop_bootstrap).await?;
    let agent = FakeAgent::new(&origin, &agent_bootstrap)?;
    agent.publish_runtime_facts("bootstrap").await?;

    let base = json!({
        "service_id": CONTEST_SERVICE,
        "version": "0.1.0",
        "target_node_id": NODE_ID,
        "endpoint": CONTEST_ENDPOINT,
        "bindings": [{
            "name": PROBLEM_API,
            "provider_deployment_id": PROBLEM_DEPLOYMENT
        }],
        "topology_id": TOPOLOGY_ID,
        "topology_etag": format!("\"{}\"", topology.applied_revision_id),
    });
    let unresolved = session
        .post_json(
            "/api/v1/store/releases:validate",
            "contest-validate-unresolved",
            &base,
            StatusCode::OK,
        )
        .await?;
    ensure!(
        unresolved.pointer("/data/valid") == Some(&json!(false)),
        "validate must expose unresolved config without side effects: {unresolved}"
    );
    assert_composition_plan(&unresolved, false)?;
    let plan_digest = required_pointer_str(&unresolved, "/data/composition_plan/planDigest")?;
    let release_graph_digest =
        required_pointer_str(&unresolved, "/data/composition_plan/releaseGraphDigest")?;
    let config_node = composition_node_id(&unresolved, "config")?;
    let inputs = json!({
        (config_node): {"config": {"registration": {"mode": "open"}}}
    });

    let mut validated_request = base.clone();
    validated_request["inputs"] = inputs.clone();
    let validated = session
        .post_json(
            "/api/v1/store/releases:validate",
            "contest-validate-ready",
            &validated_request,
            StatusCode::OK,
        )
        .await?;
    ensure!(
        validated.pointer("/data/valid") == Some(&json!(true)),
        "CompositionPlan should be fully resolved: {validated}"
    );
    assert_composition_plan(&validated, true)?;
    ensure!(
        validated.pointer("/data/side_effects")
            == Some(&json!({
                "release_imports": 0,
                "operations": 0,
                "jobs": 0,
                "runtime_calls": 0,
            })),
        "validate mutated durable state: {validated}"
    );

    let mut install_request = base;
    install_request["plan_digest"] = json!(plan_digest);
    install_request["release_graph_digest"] = json!(release_graph_digest);
    install_request["inputs"] = inputs;
    let installed = session
        .post_json(
            "/api/v1/store/releases:install",
            "contest-install-v1",
            &install_request,
            StatusCode::ACCEPTED,
        )
        .await?;
    let install_operation = required_pointer_str(&installed, "/data/operation_id")?;
    let deployment_v1 = required_pointer_str(&installed, "/data/deployment_id")?;
    assert_install_dag(&installed, &deployment_v1)?;
    agent
        .drive_success(&session, &install_operation, "install-v1")
        .await?;
    assert_active_snapshot(&session, &deployment_v1, 1).await?;
    assert_binding(&database_path, &deployment_v1, true)?;

    let applied_after_install = topology_applied_revision(&database_path)?;
    agent.publish_runtime_facts("before-upgrade-v2").await?;
    let upgraded = session
        .post_json(
            "/api/v1/store/releases:upgrade",
            "contest-upgrade-v2",
            &json!({
                "deployment_id": deployment_v1,
                "version": "0.2.0",
                "config": {"registration": {"mode": "open"}},
                "topology_id": TOPOLOGY_ID,
                "topology_etag": format!("\"{applied_after_install}\""),
            }),
            StatusCode::ACCEPTED,
        )
        .await?;
    let upgrade_operation = required_pointer_str(&upgraded, "/data/operation_id")?;
    let deployment_v2 = required_pointer_str(&upgraded, "/data/deployment_id")?;
    assert_replacement_resource_identity(&upgraded, &deployment_v1, &deployment_v2)?;
    agent
        .drive_success(&session, &upgrade_operation, "upgrade-v2")
        .await?;
    assert_active_snapshot(&session, &deployment_v2, 2).await?;
    assert_no_runtime(&database_path, &deployment_v1)?;

    // Cancel after planning but before an Agent can claim runtime work. The
    // controller must compensate any already-materialized PREPARE work and,
    // crucially, must leave generation allocation usable for the next action.
    let applied_before_cancel = topology_applied_revision(&database_path)?;
    agent.publish_runtime_facts("before-cancel-v3").await?;
    let cancel_candidate = session
        .post_json(
            "/api/v1/store/releases:upgrade",
            "contest-upgrade-v3-cancel",
            &json!({
                "deployment_id": deployment_v2,
                "version": "0.3.0",
                "config": {"registration": {"mode": "open"}},
                "topology_id": TOPOLOGY_ID,
                "topology_etag": format!("\"{applied_before_cancel}\""),
            }),
            StatusCode::ACCEPTED,
        )
        .await?;
    let cancelled_operation = required_pointer_str(&cancel_candidate, "/data/operation_id")?;
    let cancelled_deployment = required_pointer_str(&cancel_candidate, "/data/deployment_id")?;
    wait_operation_step_status(
        &session,
        &cancelled_operation,
        "topology-binding-prepare-",
        "SUCCEEDED",
    )
    .await?;
    wait_operation_step_status(
        &session,
        &cancelled_operation,
        "contribution-prepare-",
        "SUCCEEDED",
    )
    .await?;
    session
        .post_json(
            &format!("/api/v1/operations/{cancelled_operation}:cancel"),
            "contest-cancel-v3",
            &json!({}),
            StatusCode::ACCEPTED,
        )
        .await?;
    agent
        .drive_terminal(&session, &cancelled_operation, "CANCELLED", "cancel-v3")
        .await?;
    assert_active_snapshot(&session, &deployment_v2, 2).await?;
    assert_no_runtime(&database_path, &cancelled_deployment)?;
    assert_no_candidate_residue(&database_path, &cancelled_deployment)?;
    assert_topology_head_restored(&database_path, &applied_before_cancel)?;

    // This action is deliberately after cancellation. It proves an ABORTED
    // immutable revision cannot permanently consume the next generation.
    let applied_before_rollback = topology_applied_revision(&database_path)?;
    agent.publish_runtime_facts("before-rollback-v1").await?;
    let rolled_back = session
        .post_json(
            "/api/v1/store/releases:rollback",
            "contest-rollback-v1-after-cancel",
            &json!({
                "deployment_id": deployment_v2,
                "version": "0.1.0",
                "config": {"registration": {"mode": "open"}},
                "topology_id": TOPOLOGY_ID,
                "topology_etag": format!("\"{applied_before_rollback}\""),
            }),
            StatusCode::ACCEPTED,
        )
        .await?;
    let rollback_operation = required_pointer_str(&rolled_back, "/data/operation_id")?;
    let deployment_rollback = required_pointer_str(&rolled_back, "/data/deployment_id")?;
    agent
        .drive_success(&session, &rollback_operation, "rollback-v1")
        .await?;
    let rollback_generation = active_generation(&session).await?;
    ensure!(
        rollback_generation > 2,
        "rollback after cancellation did not advance generation"
    );
    assert_active_snapshot(&session, &deployment_rollback, rollback_generation).await?;

    // Uninstall is intentionally rejected while the immutable topology still
    // grants the API Binding. Remove the Link through the public immutable
    // Topology API and drive its durable provider/context DAG before retrying.
    let blocked = session
        .post_json(
            &format!("/api/v1/deployments/{deployment_rollback}:uninstall"),
            "contest-uninstall-blocked",
            &json!({}),
            StatusCode::CONFLICT,
        )
        .await?;
    ensure!(
        blocked.pointer("/code") == Some(&json!("DEPLOYMENT_ACTIVE_BINDINGS")),
        "uninstall did not fail closed on an active binding: {blocked}"
    );
    let removal_revision = apply_contest_link_removal(&session, &agent).await?;
    gateway.assert_empty_projection(&removal_revision)?;
    auth.assert_empty_projection(&removal_revision)?;
    assert_contest_bindings_are_revoked(&database_path)?;
    let uninstall = session
        .post_json(
            &format!("/api/v1/deployments/{deployment_rollback}:uninstall"),
            "contest-uninstall-final",
            &json!({}),
            StatusCode::ACCEPTED,
        )
        .await?;
    let uninstall_operation = required_pointer_str(&uninstall, "/data/operation_id")?;
    agent
        .drive_success(&session, &uninstall_operation, "uninstall")
        .await?;
    agent.publish_runtime_facts("after-uninstall").await?;
    assert_uninstalled_snapshot(&session).await?;
    assert_no_runtime(&database_path, &deployment_rollback)?;
    assert_contest_bindings_are_revoked(&database_path)?;
    gateway.assert_empty_projection(&removal_revision)?;
    auth.assert_empty_projection(&removal_revision)?;
    assert_resource_identity_reused(&database_path)?;

    shutdown_server(server)?;
    gateway.shutdown()?;
    auth.shutdown()?;
    Ok(())
}

struct CatalogFixture {
    trust_json: String,
    sources_json: String,
}

fn write_signed_contest_catalog(repo_root: &Path, directory: &Path) -> Result<CatalogFixture> {
    let source = repo_root.join("services/contest-service/ojos.service.yaml");
    let base = compile(&source).context("compile checked-in contest service contract")?;
    ensure!(
        base.service_id == CONTEST_SERVICE,
        "unexpected reference service"
    );
    let signing_key = SigningKey::from_bytes(&[42_u8; 32]);
    let key_file = directory.join("contest-signing-key.txt");
    fs::write(&key_file, STANDARD.encode(signing_key.to_bytes()))?;
    let final_catalog_dir = directory.join("catalog-fixture");
    let metadata_dir = final_catalog_dir.join("metadata");
    fs::create_dir_all(&metadata_dir)?;
    let mut releases = Vec::<CatalogReleaseV2>::new();

    for version in ["0.1.0", "0.2.0", "0.3.0"] {
        let mut contract = base.clone();
        contract.service_version = Version::parse(version)?;
        let resolved = resolved_artifacts(&contract, version)?;
        let lock = seal(&contract, &resolved)?;
        let output = directory.join(format!("published-{}", version.replace('.', "-")));
        let report = publish_catalog_v2(
            &contract,
            &lock,
            &source,
            &CatalogPublishOptions {
                output_directory: output,
                signing_key_file: key_file.clone(),
                public_base_url: "https://fixtures.invalid/contest".to_string(),
                key_id: CATALOG_KEY_ID.to_string(),
                catalog_id: format!("contest-{version}"),
                min_orchestrator_version: Version::parse("0.1.0")?,
                target_os: "linux".to_string(),
                target_arch: "x86_64".to_string(),
            },
        )?;
        let published: CatalogV2 = serde_json::from_slice(&fs::read(&report.catalog)?)?;
        let mut release = published.modules[0].releases[0].clone();
        let metadata_name = format!("contest-{version}.release.json");
        fs::copy(&report.metadata, metadata_dir.join(&metadata_name))?;
        release.metadata.url = format!("metadata/{metadata_name}");
        releases.push(release);
    }

    let mut catalog = CatalogV2 {
        schema_version: 2,
        id: "contest-clean-room".to_string(),
        name: "Contest clean-room v3 fixtures".to_string(),
        modules: vec![CatalogModuleV2 {
            id: CONTEST_SERVICE.to_string(),
            name: "Contest Service".to_string(),
            description: "signed Service Contract v3 vertical gate".to_string(),
            kind: "backend-api".to_string(),
            tags: vec!["e2e".to_string(), "service-contract-v3".to_string()],
            releases,
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
            "id": "contest-clean-room",
            "url": relative,
            "required_key_id": CATALOG_KEY_ID,
            "auth_secret_ref": "",
            "enabled": true,
            "offline_oci_layouts": {}
        }]))?,
    })
}

fn resolved_artifacts(contract: &ServiceContractV3, version: &str) -> Result<ResolvedArtifactsV1> {
    let migration_slots = contract
        .migrations
        .iter()
        .map(|migration| migration.artifact.as_str())
        .collect::<BTreeSet<_>>();
    let mut artifacts = BTreeMap::new();
    for requirement in artifact_requirements(contract)? {
        let digest = requirement.expected_digest.unwrap_or_else(|| {
            format!(
                "sha256:{:x}",
                Sha256::digest(format!("contest-fixture\0{version}\0{}", requirement.slot))
            )
        });
        let size = requirement.expected_size.unwrap_or(1);
        let is_oci = requirement.slot == contract.runtime.artifact
            || migration_slots.contains(requirement.slot.as_str());
        let reference = if is_oci {
            format!(
                "registry.invalid/ojos/{}@{}",
                requirement.slot.replace('.', "-"),
                digest
            )
        } else {
            format!(
                "https://fixtures.invalid/artifacts/{}/{}",
                digest.trim_start_matches("sha256:"),
                requirement.slot
            )
        };
        artifacts.insert(
            requirement.slot,
            ResolvedArtifactV1 {
                media_type: if is_oci {
                    "application/vnd.oci.image.manifest.v1+json".to_string()
                } else {
                    "application/octet-stream".to_string()
                },
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

struct SeededTopology {
    applied_revision_id: String,
}

fn seed_sqlite(database_path: &Path) -> Result<SeededTopology> {
    let mut store = SqliteOrchestratorStore::open(database_path)?;
    store.upsert_node(NodeRecord {
        node_id: NODE_ID.to_string(),
        host_ip: "127.0.0.1".to_string(),
        parent_node_id: String::new(),
        role: "standalone".to_string(),
        labels: json!({
            "runtime": "docker",
            "providers": {
                "postgresql": {
                    "enabled": true,
                    "provider_id": "postgresql-local"
                },
                "redis": {
                    "enabled": true,
                    "connection_id": "shared-events"
                },
                "migration": {"enabled": true},
                "materialization": {
                    "enabled": true,
                    "secret_provider": "file"
                }
            }
        }),
        status: "READY".to_string(),
        created_at: "unix-ms:1".to_string(),
        updated_at: "unix-ms:1".to_string(),
    })?;

    for (service, manifest, url, checksum) in [
        (
            PROBLEM_SERVICE,
            problem_release_manifest()?,
            "https://fixtures.invalid/problem.release.json",
            sha("problem-metadata"),
        ),
        (
            AUTH_SERVICE,
            auth_release_manifest()?,
            "https://fixtures.invalid/auth.release.json",
            sha("auth-metadata"),
        ),
    ] {
        store.upsert_service_release(ServiceRelease {
            service_name: service.to_string(),
            version: "1.0.0".to_string(),
            release_url: url.to_string(),
            manifest: serde_json::to_value(&manifest)?,
            checksum,
            created_at: "unix-ms:1".to_string(),
        })?;
    }
    let now = now_ms();
    let problem = RuntimeInstance {
        deployment_id: PROBLEM_DEPLOYMENT.to_string(),
        service_id: PROBLEM_SERVICE.to_string(),
        release_version: "1.0.0".to_string(),
        container_id: "fake-problem-provider".to_string(),
        artifact_digest: immutable_image("problem", "problem-runtime"),
        runtime_contract: RuntimeContract::standard_v1(),
        runtime_policy_sha256: POLICY_DIGEST.to_string(),
        effective_runtime_sha256: sha("problem-effective-runtime"),
        runtime_attested: true,
        desired_state: RuntimeDesiredState::Running,
        observed_state: RuntimeObservedState::Running,
        health: "HEALTHY".to_string(),
    };
    store.put_runtime_instance(&stored_runtime(problem.clone(), PROBLEM_ENDPOINT, now))?;
    let auth = RuntimeInstance {
        deployment_id: AUTH_DEPLOYMENT.to_string(),
        service_id: AUTH_SERVICE.to_string(),
        release_version: "1.0.0".to_string(),
        container_id: "fake-auth-provider".to_string(),
        artifact_digest: immutable_image("auth", "auth-runtime"),
        runtime_contract: RuntimeContract::standard_v1(),
        runtime_policy_sha256: POLICY_DIGEST.to_string(),
        effective_runtime_sha256: sha("auth-effective-runtime"),
        runtime_attested: true,
        desired_state: RuntimeDesiredState::Running,
        observed_state: RuntimeObservedState::Running,
        health: "HEALTHY".to_string(),
    };
    store.put_runtime_instance(&stored_runtime(auth.clone(), AUTH_ENDPOINT, now))?;
    let facts = runtime_facts(
        "seed",
        now,
        vec![runtime_observation(&problem), runtime_observation(&auth)],
    );
    store.put_node_runtime_facts(&StoredNodeRuntimeFacts {
        node_id: NODE_ID.to_string(),
        observed_at_ms: now,
        received_at_ms: now,
        facts: serde_json::to_value(facts)?,
    })?;

    let spec = TopologySpec::new(
        TOPOLOGY_ID,
        PROBLEM_ENDPOINT,
        "private",
        vec![
            TopologyEndpointSpec {
                endpoint: PROBLEM_ENDPOINT.to_string(),
                service_id: PROBLEM_SERVICE.to_string(),
                protocol: "http".to_string(),
                health_path: "/readyz".to_string(),
                display_name: "Problem API".to_string(),
                note: "pre-existing healthy API provider".to_string(),
                config: json!({"deployment_id": PROBLEM_DEPLOYMENT, "node_id": NODE_ID}),
            },
            TopologyEndpointSpec {
                endpoint: AUTH_ENDPOINT.to_string(),
                service_id: AUTH_SERVICE.to_string(),
                protocol: "http".to_string(),
                health_path: "/readyz".to_string(),
                display_name: "Auth permission API".to_string(),
                note: "pre-existing healthy API provider".to_string(),
                config: json!({"deployment_id": AUTH_DEPLOYMENT, "node_id": NODE_ID}),
            },
        ],
        Vec::new(),
    )?;
    let revision = store.create_initial_topology_revision(
        spec,
        "unix-ms:1",
        "clean-room-seed",
        "pre-existing problem provider",
    )?;
    store.begin_topology_apply(
        TOPOLOGY_ID,
        revision.revision_id(),
        "seed-problem-topology",
        "unix-ms:1",
    )?;
    store.finish_topology_apply(
        TOPOLOGY_ID,
        revision.revision_id(),
        "seed-problem-topology",
        TopologyApplyOutcome::Succeeded,
        "unix-ms:2",
    )?;
    Ok(SeededTopology {
        applied_revision_id: revision.revision_id().to_string(),
    })
}

fn problem_release_manifest() -> Result<ServiceReleaseManifest> {
    Ok(serde_json::from_value(json!({
        "schema_version": 1,
        "service_name": PROBLEM_SERVICE,
        "version": "1.0.0",
        "description": "pre-existing exact API provider",
        "service_type": "backend-api",
        "source": {
            "kind": "url",
            "url": "https://fixtures.invalid/problem.release.json",
            "checksum": sha("problem-metadata")
        },
        "runtime": {
            "kind": "image",
            "image": immutable_image("problem", "problem-runtime"),
            "command": "",
            "args": [],
            "env": {}
        },
        "backend": {"protocol": "http", "port": 8083, "health_path": "/readyz"},
        "migrations": [],
        "permissions": ["problem-service.problem.read"],
        "routes": [],
        "apis": [{
            "api_id": PROBLEM_API,
            "protocol": "http",
            "port_name": "http",
            "path_prefix": "/problems",
            "methods": ["GET"],
            "visibility": "explicit",
            "auth_mode": "workload",
            "permission": "problem-service.problem.read",
            "stability": "stable",
            "version": "1.0.0"
        }],
        "redis": [],
        "storage": [],
        "dependencies": [],
        "required_apis": [],
        "config_schema": {},
        "secrets": []
    }))?)
}

fn auth_release_manifest() -> Result<ServiceReleaseManifest> {
    Ok(serde_json::from_value(json!({
        "schema_version": 1,
        "service_name": AUTH_SERVICE,
        "version": "1.0.0",
        "description": "pre-existing exact permission provider",
        "service_type": "backend-api",
        "source": {"kind": "url", "url": "https://fixtures.invalid/auth.release.json", "checksum": sha("auth-metadata")},
        "runtime": {"kind": "image", "image": immutable_image("auth", "auth-runtime"), "command": "", "args": [], "env": {}},
        "backend": {"protocol": "http", "port": 8081, "health_path": "/readyz"},
        "migrations": [],
        "permissions": ["system.admin"],
        "routes": [],
        "apis": [{
            "api_id": AUTH_API,
            "protocol": "http",
            "port_name": "http",
            "path_prefix": "/internal/permissions/check",
            "methods": ["POST"],
            "visibility": "explicit",
            "auth_mode": "workload",
            "permission": "system.admin",
            "stability": "stable",
            "version": "1.0.0"
        }],
        "redis": [], "storage": [], "dependencies": [], "required_apis": [], "config_schema": {}, "secrets": []
    }))?)
}

#[derive(Clone)]
struct FakeAgent {
    client: Client,
    origin: String,
    bootstrap: String,
    state: Arc<Mutex<BTreeMap<String, RuntimeInstance>>>,
    report_sequence: Arc<Mutex<u64>>,
    last_observed_at_ms: Arc<Mutex<i64>>,
    contribution_consumer: ContributionConsumer,
}

#[derive(Clone)]
struct ContributionConsumer {
    client: Client,
    origin: String,
}

impl ContributionConsumer {
    async fn reconcile(&self, scope: &str) -> Result<()> {
        let snapshot_response = self
            .client
            .get(format!("{}/api/v1/contributions/snapshot", self.origin))
            .header("x-ojos-orchestrator-token", INTERNAL_TOKEN)
            .send()
            .await?;
        let snapshot = json_response(
            snapshot_response,
            StatusCode::OK,
            "/api/v1/contributions/snapshot",
        )
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
        for (target, token) in [("GATEWAY", GATEWAY_ACK_TOKEN), ("AUTH", AUTH_ACK_TOKEN)] {
            let response = self
                .client
                .post(format!(
                    "{}/api/v1/contributions/projections:ack",
                    self.origin
                ))
                .header("x-ojos-orchestrator-token", INTERNAL_TOKEN)
                .header("x-ojos-contribution-ack-token", token)
                .header(
                    "idempotency-key",
                    format!("clean-room-{scope}-{target}-{digest}"),
                )
                .json(&json!({
                    "schema_version": "ojos.dev/contribution-projection-ack/v1",
                    "target": target,
                    "scope_id": scope_id,
                    "snapshot_digest": digest,
                    "acknowledgements": acknowledgements.clone(),
                }))
                .send()
                .await?;
            if response.status() == StatusCode::CONFLICT {
                // The control-plane worker may publish or restore another
                // head between GET and POST. Real consumers keep their LKG
                // and poll again; the fixture models that transient race.
                return Ok(());
            }
            json_response(
                response,
                StatusCode::OK,
                "/api/v1/contributions/projections:ack",
            )
            .await?;
        }
        Ok(())
    }
}

impl FakeAgent {
    fn new(origin: &str, bootstrap: &str) -> Result<Self> {
        let problem = RuntimeInstance {
            deployment_id: PROBLEM_DEPLOYMENT.to_string(),
            service_id: PROBLEM_SERVICE.to_string(),
            release_version: "1.0.0".to_string(),
            container_id: "fake-problem-provider".to_string(),
            artifact_digest: immutable_image("problem", "problem-runtime"),
            runtime_contract: RuntimeContract::standard_v1(),
            runtime_policy_sha256: POLICY_DIGEST.to_string(),
            effective_runtime_sha256: sha("problem-effective-runtime"),
            runtime_attested: true,
            desired_state: RuntimeDesiredState::Running,
            observed_state: RuntimeObservedState::Running,
            health: "HEALTHY".to_string(),
        };
        let auth = RuntimeInstance {
            deployment_id: AUTH_DEPLOYMENT.to_string(),
            service_id: AUTH_SERVICE.to_string(),
            release_version: "1.0.0".to_string(),
            container_id: "fake-auth-provider".to_string(),
            artifact_digest: immutable_image("auth", "auth-runtime"),
            runtime_contract: RuntimeContract::standard_v1(),
            runtime_policy_sha256: POLICY_DIGEST.to_string(),
            effective_runtime_sha256: sha("auth-effective-runtime"),
            runtime_attested: true,
            desired_state: RuntimeDesiredState::Running,
            observed_state: RuntimeObservedState::Running,
            health: "HEALTHY".to_string(),
        };
        Ok(Self {
            client: Client::builder()
                .no_proxy()
                .connect_timeout(Duration::from_secs(2))
                .timeout(Duration::from_secs(10))
                .build()?,
            origin: origin.to_string(),
            bootstrap: bootstrap.to_string(),
            state: Arc::new(Mutex::new(BTreeMap::from([
                (PROBLEM_DEPLOYMENT.to_string(), problem),
                (AUTH_DEPLOYMENT.to_string(), auth),
            ]))),
            report_sequence: Arc::new(Mutex::new(0)),
            last_observed_at_ms: Arc::new(Mutex::new(now_ms())),
            contribution_consumer: ContributionConsumer {
                client: Client::builder()
                    .no_proxy()
                    .connect_timeout(Duration::from_secs(2))
                    .timeout(Duration::from_secs(10))
                    .build()?,
                origin: origin.to_string(),
            },
        })
    }

    async fn drive_success(
        &self,
        session: &DesktopSession,
        operation_id: &str,
        scope: &str,
    ) -> Result<()> {
        self.drive_terminal(session, operation_id, "SUCCEEDED", scope)
            .await
    }

    async fn drive_terminal(
        &self,
        session: &DesktopSession,
        operation_id: &str,
        expected: &str,
        scope: &str,
    ) -> Result<()> {
        for _ in 0..240 {
            let status = operation_status(session, operation_id).await?;
            if TERMINAL_OPERATION_STATUSES.contains(&status.as_str()) {
                ensure!(
                    status == expected,
                    "Operation {operation_id} ended in {status}, expected {expected}: {}",
                    session
                        .get_json(
                            &format!("/api/v1/operations/{operation_id}"),
                            StatusCode::OK
                        )
                        .await?
                );
                self.publish_runtime_facts(scope).await?;
                return Ok(());
            }
            if let Some(job) = self.claim(scope).await? {
                let result = self.success_result(&job)?;
                self.complete(&job, result).await?;
            } else {
                self.contribution_consumer.reconcile(scope).await?;
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
        }
        bail!("Operation {operation_id} did not become terminal")
    }

    async fn claim(&self, scope: &str) -> Result<Option<LeasedJob>> {
        let response = self
            .client
            .post(format!(
                "{}/api/v1/agent/nodes/{NODE_ID}/jobs:claim",
                self.origin
            ))
            .header("x-ojos-agent-bootstrap", &self.bootstrap)
            .json(&json!({
                "instance_id": format!("contest-fake-agent-{scope}"),
                "protocol_version": "v1",
                "capabilities": CAPABILITIES,
                "max_jobs": 1
            }))
            .send()
            .await?;
        let status = response.status();
        let body = response.bytes().await?;
        let body_text = strict_response_body_text(&body, "Agent claim")?;
        ensure!(
            status == StatusCode::OK,
            "Agent claim returned {status}: {}",
            body_text
        );
        let claimed: ClaimResponse = serde_json::from_slice(&body)?;
        ensure!(claimed.jobs.len() <= 1, "protocol returned multiple jobs");
        Ok(claimed.jobs.into_iter().next())
    }

    fn success_result(&self, job: &LeasedJob) -> Result<Value> {
        let observed_at_ms = now_ms();
        match &job.kind {
            JobKind::Install | JobKind::ReleasePipeline => {
                let spec = if job.kind == JobKind::ReleasePipeline {
                    job.payload.pointer("/install/spec")
                } else {
                    job.payload.pointer("/spec")
                }
                .context("install job omitted ContainerSpec")?;
                let instance = instance_from_spec(spec)?;
                self.state
                    .lock()
                    .map_err(|_| anyhow!("fake Agent state poisoned"))?
                    .insert(instance.deployment_id.clone(), instance.clone());
                Ok(json!({
                    "instance": instance,
                    "runtime_observed_at_ms": observed_at_ms,
                    "resource_claims": resource_claim_receipts(&job.payload),
                }))
            }
            JobKind::Upgrade | JobKind::Rollback => {
                let spec = job
                    .payload
                    .pointer("/new_spec")
                    .context("replacement omitted new_spec")?;
                let instance = instance_from_spec(spec)?;
                let old_deployment = required_pointer_str(&job.payload, "/old_deployment_id")?;
                let old_container = required_pointer_str(&job.payload, "/old_container_id")?;
                let preserve = job
                    .payload
                    .get("preserve_old_until_topology_cutover")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                let mut state = self
                    .state
                    .lock()
                    .map_err(|_| anyhow!("fake Agent state poisoned"))?;
                if !preserve {
                    state.remove(&old_deployment);
                }
                state.insert(instance.deployment_id.clone(), instance.clone());
                Ok(json!({
                    "instance": instance,
                    "replaced_deployment_id": old_deployment,
                    "replaced_container_id": old_container,
                    "runtime_observed_at_ms": observed_at_ms,
                }))
            }
            JobKind::Health => {
                let container_id = job
                    .payload
                    .get("container_id")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let instance = self
                    .state
                    .lock()
                    .map_err(|_| anyhow!("fake Agent state poisoned"))?
                    .values()
                    .find(|instance| instance.container_id == container_id)
                    .cloned()
                    .ok_or_else(|| anyhow!("health requested unknown container {container_id}"))?;
                Ok(json!({
                    "instance": instance,
                    "runtime_observed_at_ms": observed_at_ms,
                }))
            }
            JobKind::Uninstall => {
                let deployment_id = job
                    .payload
                    .get("deployment_id")
                    .and_then(Value::as_str)
                    .map(str::to_string)
                    .or_else(|| {
                        let container = job.payload.get("container_id")?.as_str()?;
                        self.state
                            .lock()
                            .ok()?
                            .values()
                            .find(|instance| instance.container_id == container)
                            .map(|instance| instance.deployment_id.clone())
                    })
                    .context("uninstall has no deployment identity")?;
                let removed = self
                    .state
                    .lock()
                    .map_err(|_| anyhow!("fake Agent state poisoned"))?
                    .remove(&deployment_id);
                Ok(json!({
                    "container_id": removed
                        .map(|instance| instance.container_id)
                        .or_else(|| job.payload.get("container_id").and_then(Value::as_str).map(str::to_string))
                        .unwrap_or_default(),
                    "runtime_observed_at_ms": observed_at_ms,
                }))
            }
            JobKind::BindingContextApply => Ok(json!({
                "deployment_id": job.payload.get("deployment_id"),
            })),
            JobKind::ResourcePurge => Ok(json!({
                "schema_version": "ojos.dev/resource-purge-result/v1",
                "claim_id": job.payload.get("claim_id"),
                "claim_digest": job.payload.get("claim_digest"),
                "generation": job.payload.get("generation"),
                "status": "DELETED",
                "purge_audit_intent_digest": format!("sha256:{}", "a".repeat(64)),
            })),
            other => bail!("fake Agent unexpectedly claimed {other:?}"),
        }
    }

    async fn complete(&self, job: &LeasedJob, result: Value) -> Result<()> {
        let response = self
            .client
            .post(format!(
                "{}/api/v1/agent/nodes/{NODE_ID}/jobs/{}:complete",
                self.origin, job.job_id
            ))
            .header("x-ojos-agent-bootstrap", &self.bootstrap)
            .json(&json!({
                "lease_token": job.lease_token,
                "status": CompletionStatus::Succeeded,
                "result": result,
                "error_message": "",
                "events": []
            }))
            .send()
            .await?;
        let status = response.status();
        let body = response.bytes().await?;
        let body_text = strict_response_body_text(&body, "Agent completion")?;
        ensure!(
            status == StatusCode::NO_CONTENT,
            "complete {} returned {status}: {}",
            job.job_id,
            body_text
        );
        Ok(())
    }

    async fn publish_runtime_facts(&self, scope: &str) -> Result<()> {
        let observed_at_ms = {
            let mut previous = self
                .last_observed_at_ms
                .lock()
                .map_err(|_| anyhow!("fake Agent timestamp state poisoned"))?;
            let observed_at_ms = now_ms().max(*previous + 1);
            *previous = observed_at_ms;
            observed_at_ms
        };
        let observations = self
            .state
            .lock()
            .map_err(|_| anyhow!("fake Agent state poisoned"))?
            .values()
            .map(runtime_observation)
            .collect::<Vec<_>>();
        let sequence = {
            let mut sequence = self
                .report_sequence
                .lock()
                .map_err(|_| anyhow!("fake Agent sequence poisoned"))?;
            *sequence += 1;
            *sequence
        };
        let facts = runtime_facts(&format!("{scope}-{sequence}"), observed_at_ms, observations);
        let response = self
            .client
            .put(format!(
                "{}/api/v1/agent/nodes/{NODE_ID}/runtime-facts",
                self.origin
            ))
            .header("x-ojos-agent-bootstrap", &self.bootstrap)
            .json(&facts)
            .send()
            .await?;
        let status = response.status();
        let body = response.bytes().await?;
        let body_text = strict_response_body_text(&body, "runtime facts publication")?;
        ensure!(
            status == StatusCode::NO_CONTENT,
            "runtime facts returned {status}: {}",
            body_text
        );
        Ok(())
    }
}

fn instance_from_spec(spec: &Value) -> Result<RuntimeInstance> {
    let image: OciImageReference = serde_json::from_value(
        spec.get("image")
            .cloned()
            .context("ContainerSpec omitted image")?,
    )?;
    let deployment_id = required_pointer_str(spec, "/deployment_id")?;
    Ok(RuntimeInstance {
        deployment_id: deployment_id.clone(),
        service_id: required_pointer_str(spec, "/service_id")?,
        release_version: spec
            .pointer("/labels/ojos.release_version")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        container_id: format!("fake-{}", deployment_id),
        artifact_digest: image.to_string(),
        runtime_contract: serde_json::from_value(
            spec.get("runtime_contract")
                .cloned()
                .unwrap_or_else(|| serde_json::to_value(RuntimeContract::standard_v1()).unwrap()),
        )?,
        runtime_policy_sha256: POLICY_DIGEST.to_string(),
        effective_runtime_sha256: sha(&format!("effective-runtime:{deployment_id}")),
        runtime_attested: true,
        desired_state: RuntimeDesiredState::Running,
        observed_state: RuntimeObservedState::Running,
        health: "HEALTHY".to_string(),
    })
}

fn resource_claim_receipts(payload: &Value) -> Vec<Value> {
    payload
        .get("resource_claims")
        .or_else(|| payload.pointer("/resource_claims"))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .map(|claim| {
            json!({
                "claimId": claim.get("claimId"),
                "resourceName": claim.get("resourceName"),
                "secretRef": "agent-local-redacted",
            })
        })
        .collect()
}

fn runtime_facts(
    report_id: &str,
    observed_at_ms: i64,
    deployment_observations: Vec<DeploymentRuntimeObservationV1>,
) -> NodeRuntimeFactsV1 {
    NodeRuntimeFactsV1 {
        schema_version: 1,
        report_id: format!("contest-clean-room-{report_id}-{observed_at_ms}"),
        observed_at_ms,
        agent_version: "contest-fake-agent/1".to_string(),
        runtime_policy_sha256: POLICY_DIGEST.to_string(),
        allowed_contracts: vec![RuntimeContract::standard_v1()],
        judge_sandbox_allowed_images: Vec::new(),
        redis_connection_ids: vec!["shared-events".to_string()],
        docker: DockerRuntimeFacts {
            engine: "docker".to_string(),
            server_version: "fake-1".to_string(),
            operating_system: "Linux".to_string(),
            os_type: "linux".to_string(),
            architecture: "x86_64".to_string(),
            cgroup_version: "2".to_string(),
            memory_limit: true,
            pids_limit: true,
            rootless: true,
            apparmor: false,
            seccomp: true,
            security_options: vec!["seccomp".to_string()],
        },
        inventory_complete: true,
        inventory_error: String::new(),
        deployment_observations,
        credential_statuses: Vec::new(),
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
            "desktop exchange failed"
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

    async fn post_json_if_match(
        &self,
        path: &str,
        idempotency_key: &str,
        revision_id: &str,
        body: &Value,
        expected: StatusCode,
    ) -> Result<Value> {
        let response = self
            .client
            .post(format!("{}{path}", self.origin))
            .header("cookie", &self.cookie)
            .header("x-csrf-token", &self.csrf)
            .header("idempotency-key", idempotency_key)
            .header("if-match", format!("\"{revision_id}\""))
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
        Err(error) => json!({
            "non_json_body": strict_response_body_text(
                &bytes,
                &format!("{path} JSON decoding failed ({error})"),
            )?,
        }),
    };
    ensure!(
        status == expected,
        "{path} returned {status}, expected {expected}: {body}"
    );
    Ok(body)
}

fn strict_response_body_text<'a>(bytes: &'a [u8], context: &str) -> Result<&'a str> {
    std::str::from_utf8(bytes)
        .with_context(|| format!("{context} returned a response body that is not valid UTF-8"))
}

#[test]
fn clean_room_response_diagnostics_reject_non_utf8_text() {
    let error = strict_response_body_text(&[0xff, 0xfe], "test response")
        .expect_err("invalid UTF-8 must fail closed");
    assert!(error.to_string().contains("not valid UTF-8"));
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

async fn wait_operation_step_status(
    session: &DesktopSession,
    operation_id: &str,
    step_prefix: &str,
    expected: &str,
) -> Result<()> {
    for _ in 0..240 {
        let response = session
            .get_json(
                &format!("/api/v1/operations/{operation_id}"),
                StatusCode::OK,
            )
            .await?;
        let operation = response
            .pointer("/data/operation")
            .context("operation response omitted durable operation")?;
        let step_id = operation
            .get("planned_jobs")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|job| job.get("step_id").and_then(Value::as_str))
            .find(|step| step.starts_with(step_prefix))
            .with_context(|| format!("Operation omitted step prefix {step_prefix}"))?;
        let status = operation
            .get("result")
            .and_then(|result| result.get(step_id))
            .and_then(|step| step.get("status"))
            .and_then(Value::as_str)
            .unwrap_or_default();
        if status == expected {
            return Ok(());
        }
        let operation_status = operation
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or_default();
        ensure!(
            !TERMINAL_OPERATION_STATUSES.contains(&operation_status),
            "Operation {operation_id} became {operation_status} while waiting for {step_id}={expected}: {operation}"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    bail!("Operation {operation_id} did not reach {step_prefix}={expected}")
}

fn assert_composition_plan(response: &Value, resolved: bool) -> Result<()> {
    let nodes = response
        .pointer("/data/composition_plan/nodes")
        .and_then(Value::as_array)
        .context("validate omitted CompositionPlan nodes")?;
    let node = |kind: &str| {
        nodes
            .iter()
            .find(|node| node.get("kind").and_then(Value::as_str) == Some(kind))
    };
    let api = node("api-binding").context("plan omitted ApiBinding")?;
    ensure!(
        api.get("api_id") == Some(&json!(PROBLEM_API)),
        "wrong API node: {api}"
    );
    ensure!(
        api.pointer("/provider/selectedProviderId") == Some(&json!(PROBLEM_DEPLOYMENT)),
        "running provider outside Catalog release graph was not selected: {api}"
    );
    let resource = node("resource-claim").context("plan omitted ResourceClaim")?;
    ensure!(
        resource.get("name") == Some(&json!("contests"))
            && resource.get("resource_type") == Some(&json!("postgresql.database"))
            && resource.pointer("/provider/selectedProviderId") == Some(&json!("postgresql-local")),
        "resource claim did not bind the Node provider: {resource}"
    );
    ensure!(node("package").is_some(), "plan omitted package node");
    ensure!(node("config").is_some(), "plan omitted config node");
    let unresolved = nodes
        .iter()
        .flat_map(|node| {
            node.get("unresolvedInputs")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
        })
        .filter(|input| input.get("required") == Some(&json!(true)))
        .count();
    if resolved {
        ensure!(
            response.pointer("/data/composition_inputs_valid") == Some(&json!(true)),
            "inputs did not resolve CompositionPlan"
        );
    } else {
        ensure!(unresolved >= 1, "unresolved plan hid required input");
    }
    Ok(())
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

fn assert_install_dag(response: &Value, deployment_id: &str) -> Result<()> {
    let jobs = response
        .pointer("/data/operation/planned_jobs")
        .and_then(Value::as_array)
        .context("install response omitted durable planned_jobs")?;
    let root = jobs
        .iter()
        .find(|job| job.get("kind") == Some(&json!("release_pipeline")))
        .context("contest install did not use ReleasePipeline")?;
    let claims = root
        .pointer("/payload/resource_claims")
        .and_then(Value::as_array)
        .context("ReleasePipeline omitted ResourceClaim")?;
    ensure!(claims.len() == 1, "expected one database claim: {claims:?}");
    let claim = &claims[0];
    ensure!(
        claim.get("deploymentId") == Some(&json!(deployment_id))
            && claim.get("serviceId") == Some(&json!(CONTEST_SERVICE))
            && claim.get("resourceName") == Some(&json!("contests"))
            && claim.get("providerId") == Some(&json!("postgresql-local"))
            && claim.get("generation") == Some(&json!(1)),
        "ResourceClaim identity is wrong: {claim}"
    );
    let serialized = serde_json::to_string(response)?;
    for forbidden in ["postgres://", "postgresql://", "password", "DATABASE_URL"] {
        ensure!(
            !serialized
                .to_ascii_lowercase()
                .contains(&forbidden.to_ascii_lowercase()),
            "planned Operation leaked secret material matching {forbidden}"
        );
    }
    let phases = jobs
        .iter()
        .filter_map(|job| job.pointer("/payload/phase").and_then(Value::as_str))
        .collect::<BTreeSet<_>>();
    ensure!(
        phases.contains("PREPARE") && phases.contains("COMMIT") && phases.contains("ABORT"),
        "Contribution/Topology phases are incomplete: {phases:?}"
    );
    ensure!(
        jobs.iter().any(|job| {
            job.pointer("/payload/controller") == Some(&json!("ojos.dev/contribution-job/v1"))
        }),
        "install DAG omitted Contribution controller jobs"
    );
    Ok(())
}

fn assert_replacement_resource_identity(
    response: &Value,
    old_deployment: &str,
    new_deployment: &str,
) -> Result<()> {
    let jobs = response
        .pointer("/data/operation/planned_jobs")
        .and_then(Value::as_array)
        .context("replacement omitted planned_jobs")?;
    let replacement = jobs
        .iter()
        .find(|job| {
            matches!(
                job.get("kind").and_then(Value::as_str),
                Some("upgrade" | "rollback")
            )
        })
        .context("replacement runtime Job missing")?;
    let claims = replacement
        .pointer("/payload/resource_claims")
        .and_then(Value::as_array)
        .context("replacement omitted reused ResourceClaim")?;
    ensure!(claims.len() == 1, "replacement changed claim set");
    ensure!(
        claims[0].get("deploymentId") == Some(&json!(new_deployment))
            && claims[0]
                .get("ownerInstanceId")
                .and_then(Value::as_str)
                .is_some()
            && claims[0].get("generation") == Some(&json!(1)),
        "replacement claim did not retain stable owner/generation: {}",
        claims[0]
    );
    ensure!(
        replacement.pointer("/payload/old_deployment_id") == Some(&json!(old_deployment)),
        "replacement runtime identity omitted its exact source"
    );
    Ok(())
}

async fn assert_active_snapshot(
    session: &DesktopSession,
    deployment_id: &str,
    generation: u64,
) -> Result<()> {
    let snapshot = session
        .get_json("/api/v1/contributions/snapshot", StatusCode::OK)
        .await?;
    let revisions = snapshot
        .pointer("/data/revisions")
        .and_then(Value::as_array)
        .context("snapshot omitted revisions")?;
    let contest = revisions
        .iter()
        .find(|revision| revision.get("service_id") == Some(&json!(CONTEST_SERVICE)))
        .context("snapshot omitted active contest revision")?;
    ensure!(
        contest.get("deployment_id") == Some(&json!(deployment_id))
            && contest.get("generation") == Some(&json!(generation))
            && contest.get("runtime_ready") == Some(&json!(true)),
        "active Contribution identity/health is wrong: {contest}"
    );
    for pointer in [
        "/data/gateway_routes",
        "/data/permission_definitions",
        "/data/user_frontend_modules",
        "/data/admin_frontend_modules",
    ] {
        let entries = snapshot
            .pointer(pointer)
            .and_then(Value::as_array)
            .with_context(|| format!("snapshot omitted {pointer}"))?;
        ensure!(
            !entries.is_empty(),
            "active contribution left {pointer} empty"
        );
        ensure!(
            entries.iter().all(|entry| {
                entry.get("deployment_id").is_none()
                    || entry.get("deployment_id") == Some(&json!(deployment_id))
            }),
            "snapshot contains stale deployment entries at {pointer}: {entries:?}"
        );
    }
    Ok(())
}

async fn active_generation(session: &DesktopSession) -> Result<u64> {
    let snapshot = session
        .get_json("/api/v1/contributions/snapshot", StatusCode::OK)
        .await?;
    snapshot
        .pointer("/data/revisions")
        .and_then(Value::as_array)
        .and_then(|revisions| {
            revisions
                .iter()
                .find(|revision| revision.get("service_id") == Some(&json!(CONTEST_SERVICE)))
        })
        .and_then(|revision| revision.get("generation"))
        .and_then(Value::as_u64)
        .context("active snapshot omitted contest generation")
}

async fn assert_uninstalled_snapshot(session: &DesktopSession) -> Result<()> {
    let snapshot = session
        .get_json("/api/v1/contributions/snapshot", StatusCode::OK)
        .await?;
    let revisions = snapshot
        .pointer("/data/revisions")
        .and_then(Value::as_array)
        .context("uninstall snapshot omitted revisions")?;
    let tombstone = revisions
        .iter()
        .find(|revision| revision.get("service_id") == Some(&json!(CONTEST_SERVICE)))
        .context("uninstall must retain a monotonic empty Contribution head")?;
    ensure!(
        tombstone.get("runtime_ready") == Some(&json!(false)),
        "uninstall tombstone claims runtime ready"
    );
    for pointer in [
        "/data/api_surfaces",
        "/data/gateway_routes",
        "/data/permission_definitions",
        "/data/user_frontend_modules",
        "/data/admin_frontend_modules",
    ] {
        ensure!(
            snapshot
                .pointer(pointer)
                .and_then(Value::as_array)
                .is_some_and(Vec::is_empty),
            "uninstall left active contribution residue at {pointer}: {snapshot}"
        );
    }
    Ok(())
}

fn assert_binding(database_path: &Path, deployment_id: &str, active: bool) -> Result<()> {
    let store = open_shared(database_path)?;
    let bindings = store.api_bindings_for_deployment(deployment_id)?;
    ensure!(bindings.len() == 2, "expected two contest API Bindings");
    let problem = bindings
        .iter()
        .find(|binding| binding.api_id == PROBLEM_API)
        .context("contest problem API Binding is missing")?;
    let auth = bindings
        .iter()
        .find(|binding| binding.api_id == AUTH_API)
        .context("contest auth permission API Binding is missing")?;
    ensure!(
        problem.provider_deployment_id == PROBLEM_DEPLOYMENT
            && auth.provider_deployment_id == AUTH_DEPLOYMENT
            && (!active
                || bindings.iter().all(|binding| {
                    binding.desired_state == "ACTIVE"
                        && binding.state == orchestrator_storage::ApiBindingState::Active
                })),
        "wrong binding projection: {:?}",
        bindings
    );
    Ok(())
}

fn assert_no_candidate_residue(database_path: &Path, deployment_id: &str) -> Result<()> {
    let store = open_shared(database_path)?;
    ensure!(
        store.runtime_instance(deployment_id)?.is_none(),
        "cancelled runtime survived"
    );
    ensure!(
        store.api_bindings_for_deployment(deployment_id)?.is_empty(),
        "cancelled binding survived"
    );
    let revisions = store.contribution_revisions("default", Some(CONTEST_SERVICE))?;
    ensure!(
        revisions
            .iter()
            .filter(|revision| revision.deployment_id() == deployment_id)
            .all(|revision| {
                revision.status() == orchestrator_legacy::ContributionRevisionStatusV1::Aborted
            }),
        "cancelled Contribution candidate remained live"
    );
    Ok(())
}

fn assert_no_runtime(database_path: &Path, deployment_id: &str) -> Result<()> {
    ensure!(
        open_shared(database_path)?
            .runtime_instance(deployment_id)?
            .is_none(),
        "runtime {deployment_id} survived cleanup"
    );
    Ok(())
}

async fn apply_contest_link_removal(session: &DesktopSession, agent: &FakeAgent) -> Result<String> {
    let topology = session
        .get_json(&format!("/api/v1/topologies/{TOPOLOGY_ID}"), StatusCode::OK)
        .await?;
    let expected = required_pointer_str(&topology, "/data/heads/draft_revision_id")?;
    let mut spec = topology
        .pointer("/data/draft/spec")
        .cloned()
        .context("Topology export omitted draft spec")?;
    let endpoints = spec
        .get_mut("endpoints")
        .and_then(Value::as_array_mut)
        .context("Topology spec omitted endpoints")?;
    let contest_endpoints = endpoints
        .iter()
        .filter(|endpoint| endpoint.get("service_id") == Some(&json!(CONTEST_SERVICE)))
        .filter_map(|endpoint| endpoint.get("endpoint").and_then(Value::as_str))
        .map(str::to_string)
        .collect::<BTreeSet<_>>();
    ensure!(
        !contest_endpoints.is_empty(),
        "applied Topology omitted the contest endpoint"
    );
    endpoints.retain(|endpoint| endpoint.get("service_id") != Some(&json!(CONTEST_SERVICE)));
    let links = spec
        .get_mut("links")
        .and_then(Value::as_array_mut)
        .context("Topology spec omitted links")?;
    links.retain(|link| {
        !link
            .get("source_endpoint")
            .and_then(Value::as_str)
            .is_some_and(|endpoint| contest_endpoints.contains(endpoint))
            && !link
                .get("target_endpoint")
                .and_then(Value::as_str)
                .is_some_and(|endpoint| contest_endpoints.contains(endpoint))
    });
    let revision = session
        .post_json_if_match(
            &format!("/api/v1/topologies/{TOPOLOGY_ID}/revisions"),
            "contest-topology-remove-link-revision",
            &expected,
            &spec,
            StatusCode::CREATED,
        )
        .await?;
    let revision_id = required_pointer_str(&revision, "/data/revision/revision_id")?;
    let applied = session
        .post_json_if_match(
            &format!("/api/v1/topologies/{TOPOLOGY_ID}:apply"),
            "contest-topology-remove-link-apply",
            &revision_id,
            &json!({}),
            StatusCode::ACCEPTED,
        )
        .await?;
    let operation_id = required_pointer_str(&applied, "/data/operation_id")?;
    agent
        .drive_success(session, &operation_id, "topology-remove-link")
        .await?;
    let topology = session
        .get_json(&format!("/api/v1/topologies/{TOPOLOGY_ID}"), StatusCode::OK)
        .await?;
    ensure!(
        topology.pointer("/data/heads/applied_revision_id") == Some(&json!(revision_id))
            && topology.pointer("/data/heads/draft_revision_id") == Some(&json!(revision_id))
            && topology.pointer("/data/heads/applying_revision_id") == Some(&Value::Null)
            && topology.pointer("/data/heads/applying_operation_id") == Some(&Value::Null),
        "link-removal Topology apply did not converge its durable head: {topology}"
    );
    Ok(revision_id)
}

fn assert_contest_bindings_are_revoked(database_path: &Path) -> Result<()> {
    let store = open_shared(database_path)?;
    let contest_bindings = store
        .api_bindings_for_topology(TOPOLOGY_ID)?
        .into_iter()
        .filter(|binding| binding.consumer_service_id == CONTEST_SERVICE)
        .collect::<Vec<_>>();
    ensure!(
        !contest_bindings.is_empty(),
        "contest ApiBinding audit projection unexpectedly disappeared"
    );
    ensure!(
        contest_bindings.iter().all(|binding| {
            binding.desired_state == "REVOKED"
                && binding.observed_state == "REVOKED"
                && binding.state == orchestrator_legacy::ApiBindingState::Revoked
        }),
        "contest retained an effective ApiBinding after topology removal/uninstall: {contest_bindings:?}"
    );
    Ok(())
}

fn assert_resource_identity_reused(database_path: &Path) -> Result<()> {
    let store = open_shared(database_path)?;
    let mut identities = BTreeSet::new();
    for operation in orchestrator_storage::SqliteOperationStore::new(store).list()? {
        if !matches!(
            operation.action.as_str(),
            "release.install" | "release.upgrade" | "release.rollback"
        ) {
            continue;
        }
        for job in operation.planned_jobs {
            let claims = job
                .payload
                .get("resource_claims")
                .and_then(Value::as_array)
                .into_iter()
                .flatten();
            for claim in claims {
                identities.insert((
                    claim
                        .get("claimId")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string(),
                    claim
                        .get("ownerInstanceId")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string(),
                    claim
                        .get("generation")
                        .and_then(Value::as_u64)
                        .unwrap_or_default(),
                ));
            }
        }
    }
    ensure!(
        identities.len() == 1
            && identities.iter().all(|(claim, owner, generation)| {
                !claim.is_empty() && !owner.is_empty() && *generation == 1
            }),
        "resource claim identity changed across lifecycle: {identities:?}"
    );
    Ok(())
}

fn topology_applied_revision(database_path: &Path) -> Result<String> {
    open_shared(database_path)?
        .topology_heads(TOPOLOGY_ID)?
        .and_then(|heads| heads.applied_revision_id)
        .context("topology has no applied revision")
}

fn assert_topology_head_restored(database_path: &Path, expected_revision: &str) -> Result<()> {
    let heads = open_shared(database_path)?
        .topology_heads(TOPOLOGY_ID)?
        .context("cancelled topology lost its durable head")?;
    ensure!(
        heads.applied_revision_id.as_deref() == Some(expected_revision)
            && heads.draft_revision_id == expected_revision
            && heads.applying_revision_id.is_none()
            && heads.applying_operation_id.is_none(),
        "cancelled topology did not restore a reusable head: {heads:?}"
    );
    Ok(())
}

fn open_shared(database_path: &Path) -> Result<SqliteOrchestratorStore> {
    Ok(SqliteOrchestratorStore::open_with_options(
        database_path,
        SqliteOptions {
            acquire_instance_lock: false,
            ..SqliteOptions::default()
        },
    )?)
}

struct ProviderState {
    revision_id: Option<String>,
    content_sha256: Option<String>,
    projection_sha256: Option<String>,
    absent: bool,
}

struct MockManagementProvider {
    origin: String,
    state: Arc<Mutex<ProviderState>>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<Result<()>>>,
}

impl MockManagementProvider {
    fn spawn(provider: &'static str) -> Result<Self> {
        let listener = TcpListener::bind("127.0.0.1:0")?;
        listener.set_nonblocking(true)?;
        let origin = format!("http://{}", listener.local_addr()?);
        let stop = Arc::new(AtomicBool::new(false));
        let thread_stop = Arc::clone(&stop);
        let state = Arc::new(Mutex::new(ProviderState {
            revision_id: None,
            content_sha256: None,
            projection_sha256: None,
            absent: true,
        }));
        let thread_state = Arc::clone(&state);
        let thread = thread::spawn(move || {
            let run = || -> Result<()> {
                while !thread_stop.load(Ordering::Acquire) {
                    let mut stream = match listener.accept() {
                        Ok((stream, _)) => stream,
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            thread::sleep(Duration::from_millis(5));
                            continue;
                        }
                        Err(error) => return Err(error.into()),
                    };
                    // Windows may inherit O_NONBLOCK from the listening
                    // socket. Provider requests can span more than one read,
                    // so use a blocking accepted stream with a bounded timeout.
                    stream.set_nonblocking(false)?;
                    stream.set_read_timeout(Some(Duration::from_secs(2)))?;
                    let request = read_provider_request(&mut stream)?;
                    ensure!(
                        request.path.starts_with("/api/v1/topologies/"),
                        "unexpected provider path {}",
                        request.path
                    );
                    let topology_id = request.path.rsplit('/').next().unwrap_or_default();
                    if request.method == "GET" {
                        let current = thread_state
                            .lock()
                            .map_err(|_| anyhow!("provider state poisoned"))?;
                        let response = if current.absent {
                            json!({
                                "api_version": "v1",
                                "provider": provider,
                                "topology_id": topology_id,
                                "absent": true,
                                "endpoints": [],
                                "links": []
                            })
                        } else {
                            json!({
                                "api_version": "v1",
                                "provider": provider,
                                "topology_id": topology_id,
                                "observed_revision_id": current.revision_id,
                                "observed_content_sha256": current.content_sha256,
                                "observed_projection_sha256": current.projection_sha256,
                                "absent": false,
                                "endpoints": [],
                                "links": []
                            })
                        };
                        write_provider_response(&mut stream, 200, &response)?;
                        continue;
                    }
                    let body: Value = serde_json::from_slice(&request.body)?;
                    ensure!(
                        body.get("provider") == Some(&json!(provider)),
                        "wrong provider"
                    );
                    let action = required_pointer_str(&body, "/action")?;
                    let absent = action == "delete";
                    let projection_sha256 = if absent {
                        None
                    } else {
                        Some(projection_digest(&body["routes"], &body["grants"])?)
                    };
                    {
                        let mut current = thread_state
                            .lock()
                            .map_err(|_| anyhow!("provider state poisoned"))?;
                        current.revision_id = body
                            .get("desired_revision_id")
                            .and_then(Value::as_str)
                            .map(str::to_string);
                        current.content_sha256 = body
                            .get("desired_content_sha256")
                            .and_then(Value::as_str)
                            .map(str::to_string);
                        current.projection_sha256 = projection_sha256;
                        current.absent = absent;
                    }
                    let response = json!({
                        "api_version": "v1",
                        "provider": provider,
                        "action": action,
                        "topology_id": body["topology_id"],
                        "operation_id": body["operation_id"],
                        "completed": true,
                        "observed_revision_id": body.get("desired_revision_id"),
                        "observed_content_sha256": body.get("desired_content_sha256"),
                        "absent": absent
                    });
                    write_provider_response(&mut stream, 200, &response)?;
                }
                Ok(())
            };
            let outcome = run();
            if let Err(error) = &outcome {
                eprintln!("contest clean-room {provider} provider stopped: {error:#}");
            }
            outcome
        });
        Ok(Self {
            origin,
            state,
            stop,
            thread: Some(thread),
        })
    }

    fn origin(&self) -> &str {
        &self.origin
    }

    fn assert_empty_projection(&self, revision_id: &str) -> Result<()> {
        let state = self
            .state
            .lock()
            .map_err(|_| anyhow!("provider state poisoned"))?;
        ensure!(
            !state.absent
                && state.revision_id.as_deref() == Some(revision_id)
                && state.projection_sha256.as_deref() == Some(EMPTY_PROJECTION_DIGEST),
            "provider retained a stale projection after link removal: revision={:?}, projection={:?}, absent={}",
            state.revision_id,
            state.projection_sha256,
            state.absent
        );
        Ok(())
    }

    fn shutdown(mut self) -> Result<()> {
        self.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            thread
                .join()
                .map_err(|_| anyhow!("provider thread panicked"))??;
        }
        Ok(())
    }
}

impl Drop for MockManagementProvider {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

struct ProviderRequest {
    method: String,
    path: String,
    body: Vec<u8>,
}

fn read_provider_request(stream: &mut TcpStream) -> Result<ProviderRequest> {
    const MAX_REQUEST: usize = 8 * 1024 * 1024;
    let mut bytes = Vec::new();
    let header_end = loop {
        let mut chunk = [0_u8; 4096];
        let read = stream.read(&mut chunk)?;
        ensure!(read > 0, "provider request closed before headers");
        bytes.extend_from_slice(&chunk[..read]);
        ensure!(bytes.len() <= MAX_REQUEST, "provider request too large");
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
    while bytes.len() < header_end + content_length {
        let mut chunk = [0_u8; 4096];
        let read = stream.read(&mut chunk)?;
        ensure!(read > 0, "provider request closed before body");
        bytes.extend_from_slice(&chunk[..read]);
        ensure!(bytes.len() <= MAX_REQUEST, "provider request too large");
    }
    Ok(ProviderRequest {
        method,
        path,
        body: bytes[header_end..header_end + content_length].to_vec(),
    })
}

fn write_provider_response(stream: &mut TcpStream, status: u16, body: &Value) -> Result<()> {
    let body = body.to_string();
    write!(
        stream,
        "HTTP/1.1 {status} Mock\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )?;
    stream.flush()?;
    Ok(())
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ProviderBindingRouteFixture {
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
#[serde(deny_unknown_fields)]
struct ProviderBindingGrantFixture {
    binding_id: String,
    requirement_name: String,
    consumer_deployment_id: String,
    consumer_service_id: String,
    consumer_node_id: String,
    credential_generation: u64,
    api_id: String,
    permission: String,
}

#[derive(Serialize)]
struct ProviderProjectionFixture {
    routes: Vec<ProviderBindingRouteFixture>,
    grants: Vec<ProviderBindingGrantFixture>,
}

fn projection_digest(routes: &Value, grants: &Value) -> Result<String> {
    let mut projection = ProviderProjectionFixture {
        routes: serde_json::from_value(routes.clone()).context("decode provider routes")?,
        grants: serde_json::from_value(grants.clone()).context("decode provider grants")?,
    };
    projection
        .routes
        .sort_by(|left, right| left.binding_id.cmp(&right.binding_id));
    projection
        .grants
        .sort_by(|left, right| left.binding_id.cmp(&right.binding_id));
    let bytes = serde_json::to_vec(&projection)?;
    let digest = format!("{:x}", Sha256::digest(bytes));
    if projection.routes.is_empty() && projection.grants.is_empty() {
        ensure!(
            digest == EMPTY_PROJECTION_DIGEST,
            "fixture projection encoder drifted from the production empty digest"
        );
    }
    Ok(digest)
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

fn shutdown_server(server: EmbeddedServerHandle) -> Result<()> {
    server.shutdown()?;
    server.join_timeout(Duration::from_secs(10))?;
    Ok(())
}

fn workspace_root() -> Result<PathBuf> {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(3)
        .map(Path::to_path_buf)
        .ok_or_else(|| anyhow!("could not locate workspace root"))
}

fn required_pointer_str(value: &Value, pointer: &str) -> Result<String> {
    value
        .pointer(pointer)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .ok_or_else(|| anyhow!("value omitted {pointer}: {value}"))
}

fn immutable_image(name: &str, seed: &str) -> String {
    format!("registry.invalid/ojos/{name}@{}", sha(seed))
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
        // SAFETY: this integration test is a dedicated test executable.
        unsafe { std::env::set_var(name, value) };
    }

    fn remove(&mut self, name: &str) {
        self.remember(name);
        // SAFETY: this integration test is a dedicated test executable.
        unsafe { std::env::remove_var(name) };
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
