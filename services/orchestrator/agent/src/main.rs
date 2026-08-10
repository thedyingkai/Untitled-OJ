use clap::{Args, Parser, Subcommand};
use orchestrator_agent::{
    AgentLedger, AgentWorker, BuiltInReleasePipelineProvider, EnrollmentAttempt, EnrollmentClient,
    HttpMtlsTransport, IdentityError, IdentityStore, JobExecutor, LocalRuntimeContextProvider,
    NodeRuntimeFactsPublisher, RuntimeContextProvider, StoredNodeIdentity, WorkerConfig,
    WorkloadCredentialSupervisor, event_connection_urls_from_env, generate_certificate_request,
    recover_pending_runtime_contexts, validate_enrollment_bundle_fresh,
};
use orchestrator_runtime::DockerEngineRuntime;
use std::fs;
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::sync::watch;

#[derive(Debug, Parser)]
#[command(
    name = "ojos-orchestrator-agent",
    version,
    about = "Pull-based OCI runtime worker for an OJOS orchestrator node"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Redeem a one-time registration code and durably create a Node identity.
    Enroll(EnrollArgs),
    /// Run the worker with the enrolled identity and rotate it automatically.
    Run(RunArgs),
}

#[derive(Debug, Args)]
struct EnrollArgs {
    /// HTTPS base URL of the control plane.
    #[arg(long = "control-plane")]
    control_plane: String,

    /// One-time registration code. Prefer --enrollment-code-file in service automation.
    #[arg(long = "enrollment-code", conflicts_with = "enrollment_code_file")]
    enrollment_code: Option<String>,

    /// UTF-8 file containing the one-time registration code.
    #[arg(long = "enrollment-code-file", conflicts_with = "enrollment_code")]
    enrollment_code_file: Option<PathBuf>,

    /// PEM CA bundle used exclusively to verify the control-plane HTTPS server.
    #[arg(long = "ca")]
    server_ca: PathBuf,

    /// Durable directory for versioned Node certificate generations.
    #[arg(long = "identity-dir")]
    identity_dir: PathBuf,

    /// Expected Node identity used to recover an interrupted enrollment without
    /// redeeming the one-time code again.
    #[arg(long = "expected-node-id")]
    expected_node_id: Option<String>,
}

#[derive(Debug, Args)]
struct RunArgs {
    /// HTTPS base URL of the control plane.
    #[arg(long = "control-plane")]
    control_plane: String,

    /// Durable directory created by the enroll command.
    #[arg(long = "identity-dir")]
    identity_dir: PathBuf,

    /// Unique identity for this process incarnation. Generated when omitted.
    #[arg(long = "instance")]
    instance_id: Option<String>,

    /// Local SQLite execution ledger. Defaults inside the identity directory.
    #[arg(long)]
    ledger: Option<PathBuf>,

    /// Strict JSON credential file used only for digest-pinned private registry pulls.
    #[arg(long = "registry-credentials")]
    registry_credentials: Option<PathBuf>,

    /// Strict Agent-local JSON policy enabling closed runtime profiles. When
    /// omitted, only standard-container-v1 is allowed.
    #[arg(long = "runtime-policy")]
    runtime_policy: Option<PathBuf>,

    /// Enable the removed Node-side Auth/Gateway/API Registry providers for
    /// the old local Compose workflow. This is rejected unless
    /// OJOS_ENVIRONMENT=development; never use it on an enrolled production
    /// Node.
    #[arg(long = "legacy-release-providers", default_value_t = false)]
    legacy_release_providers: bool,

    #[arg(long, default_value_t = 10_000)]
    heartbeat_ms: u64,

    #[arg(long, default_value_t = 30_000)]
    lease_ms: i64,

    #[arg(long, default_value_t = 1_000)]
    transport_retry_ms: u64,

    /// Delay before retrying a failed certificate renewal.
    #[arg(long, default_value_t = 60_000)]
    renewal_retry_ms: u64,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let _install_guard = ojos_orchestrator_installer::acquire_runtime_install_guard()?;
    match Cli::parse().command {
        Command::Enroll(arguments) => enroll(arguments).await,
        Command::Run(arguments) => run(arguments).await,
    }
}

async fn enroll(arguments: EnrollArgs) -> Result<(), Box<dyn std::error::Error>> {
    let expected_node_id = arguments
        .expected_node_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());
    if arguments.expected_node_id.is_some() && expected_node_id.is_none() {
        return Err("--expected-node-id must not be blank".into());
    }

    let identity_store = IdentityStore::new(&arguments.identity_dir);
    // A one-time code is not consumed until durable storage has passed a real
    // create/write/sync/remove preflight.
    identity_store.preflight()?;
    // Hold a process-wide session lock across CSR selection, the HTTP request,
    // identity installation, and completion-marker publication. A second CLI
    // process can never swap the pending CSR between those steps.
    let _enrollment_session = identity_store.begin_enrollment_session()?;
    let server_ca = fs::read(&arguments.server_ca)?;

    // A supplied code always creates or resumes a code-bound attempt before
    // recovery. Therefore an old current generation can never short-circuit a
    // genuine re-enrollment. Only a no-code invocation may recover whichever
    // current generation is already installed.
    let enrollment_code = read_enrollment_code(&arguments)?;
    let enrollment_attempt = enrollment_code
        .as_deref()
        .map(|enrollment_code| {
            identity_store.prepare_enrollment_attempt(
                &arguments.control_plane,
                expected_node_id,
                enrollment_code,
                &server_ca,
            )
        })
        .transpose()?;

    // This recovery check deliberately precedes redeeming the one-time code.
    // A pending attempt requires its exact public key; a completed attempt also
    // requires the exact recorded serial.
    let recovered =
        identity_store.recover_enrollment_identity(enrollment_attempt.as_ref(), |identity| {
            identity.validate_recovery_binding_for(expected_node_id, &server_ca)?;
            if identity.not_after_ms <= unix_ms() {
                return Err(IdentityError::Invalid(format!(
                    "recoverable Node certificate {} is expired",
                    identity.serial_hex
                )));
            }
            Ok(())
        })?;
    if let Some(identity) = recovered {
        let recovery_transport = HttpMtlsTransport::from_pem_files(
            &arguments.control_plane,
            &identity.certificate_path,
            &identity.private_key_path,
            &identity.server_ca_path,
        )
        .map_err(|error| {
            IdentityError::Invalid(format!(
                "recoverable identity cannot create the configured control-plane transport: {error}"
            ))
        })?;
        recovery_transport
            .verify_identity(&identity.node_id, &identity.serial_hex)
            .await?;
        let identity = identity_store.publish_recovered_identity(&identity)?;
        if let Some(attempt) = enrollment_attempt.as_ref() {
            identity_store.complete_enrollment_attempt(attempt, &identity.serial_hex)?;
        }
        print_enrollment_result("RECOVERED", &arguments.identity_dir, &identity)?;
        return Ok(());
    }

    if matches!(
        enrollment_attempt,
        Some(EnrollmentAttempt::Completed { .. })
    ) {
        return Err("the completed enrollment marker does not have its exact installed identity; refusing to redeem a consumed code".into());
    }

    let enrollment_code = enrollment_code
        .as_deref()
        .ok_or("exactly one of --enrollment-code or --enrollment-code-file is required")?;

    // This CSR/private key is durably published before the request. If the
    // server commits redemption but its response is lost, the next process
    // resends the exact CSR and receives the exact issued certificate.
    let request = match enrollment_attempt.as_ref() {
        Some(EnrollmentAttempt::Pending(request)) => request,
        Some(EnrollmentAttempt::Completed { .. }) => unreachable!("handled above"),
        None => unreachable!("an enrollment code always prepares an attempt"),
    };
    let client = EnrollmentClient::from_ca_pem(&arguments.control_plane, &server_ca)?;
    let bundle = client.redeem(enrollment_code, &request.csr_pem).await?;
    if expected_node_id.is_some_and(|expected| bundle.node_id != expected) {
        return Err(format!(
            "enrollment returned Node {}, expected {}",
            bundle.node_id,
            expected_node_id.expect("checked as Some")
        )
        .into());
    }
    validate_enrollment_bundle_fresh(&bundle, unix_ms())?;
    // Parse the returned certificate, private key and current control-plane CA
    // before publishing durable identity state.
    let _candidate_transport = HttpMtlsTransport::from_pem(
        &arguments.control_plane,
        bundle.certificate_pem.as_bytes(),
        request.private_key_pem.as_bytes(),
        &server_ca,
    )?;
    let identity =
        identity_store.install_unpublished(&bundle, &request.private_key_pem, &server_ca)?;
    identity.validate_recovery_binding(expected_node_id.unwrap_or(&bundle.node_id), &server_ca)?;

    // Parse the returned certificate, private key, and server roots together
    // before declaring enrollment successful.
    let transport = HttpMtlsTransport::from_pem_files(
        &arguments.control_plane,
        &identity.certificate_path,
        &identity.private_key_path,
        &identity.server_ca_path,
    )?;
    transport
        .verify_identity(&identity.node_id, &identity.serial_hex)
        .await?;
    let identity = identity_store.publish_recovered_identity(&identity)?;
    identity_store.complete_enrollment_attempt(
        enrollment_attempt
            .as_ref()
            .expect("prepared enrollment attempt"),
        &identity.serial_hex,
    )?;
    print_enrollment_result("ENROLLED", &arguments.identity_dir, &identity)?;
    Ok(())
}

fn read_enrollment_code(
    arguments: &EnrollArgs,
) -> Result<Option<String>, Box<dyn std::error::Error>> {
    let code = match (&arguments.enrollment_code, &arguments.enrollment_code_file) {
        (Some(code), None) => Some(code.trim().to_string()),
        (None, Some(path)) => Some(fs::read_to_string(path)?.trim().to_string()),
        (None, None) => None,
        (Some(_), Some(_)) => {
            return Err(
                "exactly one of --enrollment-code or --enrollment-code-file is required".into(),
            );
        }
    };
    if code.as_deref().is_some_and(str::is_empty) {
        return Err("the enrollment code is empty".into());
    }
    Ok(code)
}

fn print_enrollment_result(
    status: &str,
    identity_directory: &std::path::Path,
    identity: &StoredNodeIdentity,
) -> Result<(), serde_json::Error> {
    println!(
        "{}",
        serde_json::to_string(&serde_json::json!({
            "status": status,
            "node_id": identity.node_id,
            "spiffe_id": identity.spiffe_id,
            "serial_hex": identity.serial_hex,
            "not_after_ms": identity.not_after_ms,
            "renew_after_ms": identity.renew_after_ms,
            "identity_dir": identity_directory,
        }))?
    );
    Ok(())
}

async fn run(arguments: RunArgs) -> Result<(), Box<dyn std::error::Error>> {
    if arguments.renewal_retry_ms == 0 {
        return Err("--renewal-retry-ms must be positive".into());
    }
    let identity_store = IdentityStore::new(&arguments.identity_dir);
    let ledger_path = arguments
        .ledger
        .clone()
        .unwrap_or_else(|| arguments.identity_dir.join("execution-ledger.sqlite3"));
    let instance_id = arguments
        .instance_id
        .clone()
        .unwrap_or_else(|| format!("agent-{}-{}", std::process::id(), unix_ms()));

    let (shutdown_tx, mut shutdown_rx) = watch::channel(false);
    tokio::spawn(async move {
        wait_for_shutdown_signal().await;
        let _ = shutdown_tx.send(true);
    });

    loop {
        if *shutdown_rx.borrow() {
            return Ok(());
        }
        let identity = identity_store.load()?;
        let now = unix_ms();
        if now >= identity.not_after_ms {
            return Err(format!(
                "Node certificate {} expired at {}; enroll a replacement identity",
                identity.serial_hex, identity.not_after_ms
            )
            .into());
        }

        let transport = HttpMtlsTransport::from_pem_files(
            &arguments.control_plane,
            &identity.certificate_path,
            &identity.private_key_path,
            &identity.server_ca_path,
        )?;
        // This proves the newest complete on-disk generation before the
        // control plane revokes any older serial. Replaying it after a lost
        // response is safe and prevents renewal from bricking a Node.
        transport.activate_certificate().await?;
        let renewal_transport = transport.clone();
        let artifact_fetcher = transport.artifact_fetcher(identity.node_id.clone())?;
        let mut ledger = AgentLedger::open(&ledger_path)?;
        let runtime = DockerEngineRuntime::connect_local()?;
        let runtime = if let Some(path) = arguments.registry_credentials.as_deref() {
            runtime.with_registry_credentials_file(path)?
        } else {
            runtime
        };
        runtime.ping().await?;
        let runtime_facts = runtime.runtime_facts().await?;
        let local_runtime_provider = if let Some(path) = arguments.runtime_policy.as_deref() {
            LocalRuntimeContextProvider::from_json_file(path, runtime_facts)?
        } else {
            LocalRuntimeContextProvider::standard_only(
                runtime_facts,
                ledger_path.with_file_name("runtime-contexts"),
            )
        }
        .with_event_connections(event_connection_urls_from_env()?);
        let runtime_provider: std::sync::Arc<dyn RuntimeContextProvider> =
            std::sync::Arc::new(local_runtime_provider);
        recover_pending_runtime_contexts(&mut ledger, runtime_provider.as_ref(), &runtime).await?;
        let credential_exchanger =
            std::sync::Arc::new(transport.workload_credential_exchanger(identity.node_id.clone())?);
        let credential_supervisor = std::sync::Arc::new(WorkloadCredentialSupervisor::new(
            credential_exchanger,
            std::sync::Arc::clone(&runtime_provider),
        ));
        credential_supervisor.recover_active(&ledger).await?;
        let facts_publisher = transport.runtime_facts_publisher(identity.node_id.clone())?;
        let mut initial_facts = runtime_provider.runtime_facts();
        initial_facts.observed_at_ms = unix_ms();
        initial_facts.report_id = format!("{}:{}", instance_id, initial_facts.observed_at_ms);
        initial_facts.docker = runtime.runtime_facts().await?;
        match runtime.managed_deployment_inventory(4_096).await {
            Ok(inventory) => {
                initial_facts.inventory_complete = inventory.inventory_complete;
                initial_facts.inventory_error = inventory.inventory_error;
                initial_facts.deployment_observations = inventory.deployments;
            }
            Err(error) => {
                initial_facts.inventory_complete = false;
                initial_facts.inventory_error = bounded_runtime_report_error(&error.to_string());
            }
        }
        initial_facts.credential_statuses = credential_supervisor.status().await;
        facts_publisher
            .publish_runtime_facts(&identity.node_id, &initial_facts)
            .await?;
        let facts_runtime = runtime.clone();
        let facts_provider = std::sync::Arc::clone(&runtime_provider);
        let facts_credentials = std::sync::Arc::clone(&credential_supervisor);
        let facts_node_id = identity.node_id.clone();
        let facts_instance_id = instance_id.clone();
        let facts_task = tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_secs(30)).await;
                let docker = match facts_runtime.runtime_facts().await {
                    Ok(facts) => facts,
                    Err(error) => {
                        eprintln!("runtime facts Docker probe failed: {error}");
                        continue;
                    }
                };
                let mut report = facts_provider.runtime_facts();
                report.observed_at_ms = unix_ms();
                report.report_id = format!("{}:{}", facts_instance_id, report.observed_at_ms);
                report.docker = docker;
                match facts_runtime.managed_deployment_inventory(4_096).await {
                    Ok(inventory) => {
                        report.inventory_complete = inventory.inventory_complete;
                        report.inventory_error = inventory.inventory_error;
                        report.deployment_observations = inventory.deployments;
                    }
                    Err(error) => {
                        report.inventory_complete = false;
                        report.inventory_error = bounded_runtime_report_error(&error.to_string());
                    }
                }
                report.credential_statuses = facts_credentials.status().await;
                if let Err(error) = facts_publisher
                    .publish_runtime_facts(&facts_node_id, &report)
                    .await
                {
                    eprintln!("runtime facts publication failed: {error}");
                }
            }
        });
        let provider_state_database = ledger_path.with_file_name("provider-state.sqlite3");
        let pipeline_provider = if arguments.legacy_release_providers {
            BuiltInReleasePipelineProvider::from_legacy_development_env_with_state_database(
                provider_state_database,
            )?
        } else {
            BuiltInReleasePipelineProvider::from_remote_agent_env_with_state_database(
                provider_state_database,
            )?
        };
        let executor = JobExecutor::new(runtime)
            .with_pipeline_provider(std::sync::Arc::new(pipeline_provider))
            .with_artifact_fetcher(std::sync::Arc::new(artifact_fetcher))
            .with_runtime_context(
                std::sync::Arc::clone(&runtime_provider),
                std::sync::Arc::clone(&credential_supervisor),
            );
        let config = WorkerConfig {
            node_id: identity.node_id.clone(),
            instance_id: instance_id.clone(),
            heartbeat_ms: arguments.heartbeat_ms,
            lease_ms: arguments.lease_ms,
            transport_retry_ms: arguments.transport_retry_ms,
        };
        let mut worker = AgentWorker::new(config, transport, executor, ledger)?;
        let (worker_shutdown_tx, worker_shutdown_rx) = watch::channel(false);
        let mut worker_run = Box::pin(worker.run_until_shutdown(worker_shutdown_rx));
        let mut next_renewal_attempt_ms = identity.renew_after_ms.max(unix_ms());

        loop {
            let wait_ms = next_renewal_attempt_ms.saturating_sub(unix_ms()).max(0) as u64;
            tokio::select! {
                result = &mut worker_run => {
                    facts_task.abort();
                    credential_supervisor.shutdown_all().await;
                    result?;
                    return Err("Agent worker stopped before shutdown or certificate rotation".into());
                }
                changed = shutdown_rx.changed() => {
                    if changed.is_err() || *shutdown_rx.borrow() {
                        let _ = worker_shutdown_tx.send(true);
                        worker_run.await?;
                        facts_task.abort();
                        credential_supervisor.shutdown_all().await;
                        return Ok(());
                    }
                }
                _ = tokio::time::sleep(Duration::from_millis(wait_ms)) => {
                    // Do not ask the server to revoke the current certificate
                    // unless the replacement generation can first be persisted.
                    identity_store.preflight()?;
                    let request = generate_certificate_request()?;
                    match renewal_transport.renew_certificate(&request.csr_pem).await {
                        Ok(bundle) => {
                            if bundle.node_id != identity.node_id {
                                return Err(format!(
                                    "renewal returned Node {}, expected {}",
                                    bundle.node_id, identity.node_id
                                ).into());
                            }
                            let server_ca = fs::read(&identity.server_ca_path)?;
                            let installed = identity_store.install(
                                &bundle,
                                &request.private_key_pem,
                                &server_ca,
                            )?;
                            eprintln!(
                                "rotated Node certificate {} -> {}; restarting worker transport",
                                identity.serial_hex, installed.serial_hex
                            );
                            let _ = worker_shutdown_tx.send(true);
                            worker_run.await?;
                            facts_task.abort();
                            credential_supervisor.shutdown_all().await;
                            break;
                        }
                        Err(error) => {
                            let retry_at = unix_ms().saturating_add(
                                arguments.renewal_retry_ms.min(i64::MAX as u64) as i64,
                            );
                            if retry_at >= identity.not_after_ms {
                                return Err(format!(
                                    "certificate renewal failed and no safe retry remains before expiry: {error}"
                                ).into());
                            }
                            eprintln!(
                                "certificate renewal failed; retrying in {} ms: {}",
                                arguments.renewal_retry_ms, error
                            );
                            next_renewal_attempt_ms = retry_at;
                        }
                    }
                }
            }
        }
    }
}

fn unix_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(i64::MAX as u128) as i64
}

fn bounded_runtime_report_error(value: &str) -> String {
    const MAX_BYTES: usize = 512;
    if value.len() <= MAX_BYTES {
        return value.to_string();
    }
    let mut end = MAX_BYTES;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    value[..end].to_string()
}

async fn wait_for_shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};

        match signal(SignalKind::terminate()) {
            Ok(mut terminate) => {
                tokio::select! {
                    _ = tokio::signal::ctrl_c() => {}
                    _ = terminate.recv() => {}
                }
            }
            Err(_) => {
                let _ = tokio::signal::ctrl_c().await;
            }
        }
    }

    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}
