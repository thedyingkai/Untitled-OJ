use clap::{Args, Parser, Subcommand};
use orchestrator_agent::resource_claim::{
    FileResourceSecretStore, LivePostgreSqlExecutor, LocalResourceClaimManager,
    PostgreSqlAdminConfigV1, PostgreSqlProviderDescriptorV1, PostgreSqlTlsModeV1,
    PostgreSqlTlsTrustV1, SecretMaterial,
};
use orchestrator_agent::{
    AgentLedger, AgentWorker, EnrollmentAttempt, EnrollmentClient, HttpMtlsTransport,
    IdentityError, IdentityStore, JobExecutor, LocalRuntimeContextProvider,
    NodeRuntimeFactsPublisher, PipelineBootstrapConfig, RuntimeContextProvider, StoredNodeIdentity,
    WorkerConfig, WorkloadCredentialSupervisor, generate_certificate_request,
    reconcile_migration_containers, recover_pending_runtime_contexts,
    validate_agent_workload_file_ownership, validate_enrollment_bundle_fresh,
};
use orchestrator_runtime::{DockerEngineRuntime, WorkloadFileOwnership};
use serde::Deserialize;
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

    /// Dedicated workload-visible export root. Docker-in-Docker deployments
    /// mount only this root into the daemon namespace, read-only. It must be
    /// disjoint from identity, ledger, registry and provider state.
    #[arg(long = "workload-export-dir")]
    workload_export_dir: PathBuf,

    /// Enable the removed Node-side Auth/Gateway/API Registry providers for
    /// the old local Compose workflow. This is rejected unless
    /// OJOS_ENVIRONMENT=development; never use it on an enrolled production
    /// Node.
    #[arg(long = "legacy-release-providers", default_value_t = false)]
    legacy_release_providers: bool,

    /// Strict JSON file containing the Agent-local PostgreSQL provider
    /// descriptor and administrator connection URL. The administrator secret
    /// is never accepted through a Job or control-plane API.
    #[arg(long = "postgres-resource-provider")]
    postgres_resource_provider: Option<PathBuf>,

    /// Durable Agent-internal root for ResourceClaim provider credentials.
    /// Workload DSN outputs are written below --workload-export-dir instead.
    /// Required together with --postgres-resource-provider.
    #[arg(long = "resource-secret-dir")]
    resource_secret_dir: Option<PathBuf>,

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

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct PostgreSqlResourceProviderDocument {
    schema_version: u32,
    provider_id: String,
    host: String,
    port: u16,
    #[serde(default = "default_postgres_tls_mode")]
    tls_mode: String,
    admin_url_file: PathBuf,
    #[serde(default)]
    ca_file: Option<PathBuf>,
}

fn default_postgres_tls_mode() -> String {
    "verify-full".to_string()
}

impl PostgreSqlResourceProviderDocument {
    fn descriptor(&self) -> Result<PostgreSqlProviderDescriptorV1, Box<dyn std::error::Error>> {
        if self.schema_version != 1 {
            return Err("PostgreSQL resource provider schema_version must be 1".into());
        }
        let tls_mode = match self.tls_mode.as_str() {
            "require" => PostgreSqlTlsModeV1::Require,
            "verify-ca" => PostgreSqlTlsModeV1::VerifyCa,
            "verify-full" => PostgreSqlTlsModeV1::VerifyFull,
            _ => return Err(
                "PostgreSQL resource provider tls_mode must be require, verify-ca, or verify-full"
                    .into(),
            ),
        };
        let descriptor = PostgreSqlProviderDescriptorV1 {
            provider_id: self.provider_id.clone(),
            host: self.host.clone(),
            port: self.port,
            tls_mode,
        };
        descriptor.validate()?;
        if matches!(
            tls_mode,
            PostgreSqlTlsModeV1::VerifyCa | PostgreSqlTlsModeV1::VerifyFull
        ) && self.ca_file.is_none()
        {
            return Err("verify-ca/verify-full PostgreSQL provider requires ca_file".into());
        }
        Ok(descriptor)
    }

    fn tls_trust(&self) -> PostgreSqlTlsTrustV1 {
        self.ca_file
            .as_ref()
            .map(|path| PostgreSqlTlsTrustV1::CaCertificate(path.clone()))
            .unwrap_or(PostgreSqlTlsTrustV1::Platform)
    }
}

fn read_postgres_resource_provider(
    path: &std::path::Path,
) -> Result<PostgreSqlResourceProviderDocument, Box<dyn std::error::Error>> {
    let metadata = fs::metadata(path)?;
    if !metadata.is_file() || metadata.len() == 0 || metadata.len() > 64 * 1024 {
        return Err("PostgreSQL resource provider configuration must be a non-empty regular file no larger than 64 KiB".into());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o077 != 0 {
            return Err(
                "PostgreSQL resource provider configuration must not be group/world accessible"
                    .into(),
            );
        }
    }
    let document: PostgreSqlResourceProviderDocument = serde_json::from_slice(&fs::read(path)?)?;
    let admin_metadata = fs::metadata(&document.admin_url_file)?;
    if !admin_metadata.is_file() || admin_metadata.len() == 0 || admin_metadata.len() > 64 * 1024 {
        return Err("PostgreSQL administrator URL file must be a non-empty regular file no larger than 64 KiB".into());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if admin_metadata.permissions().mode() & 0o077 != 0 {
            return Err(
                "PostgreSQL administrator URL file must not be group/world accessible".into(),
            );
        }
    }
    document.descriptor()?;
    Ok(document)
}

#[cfg(test)]
mod configuration_tests {
    use super::*;

    #[test]
    fn postgres_resource_provider_requires_schema_one_and_private_indirect_secret() {
        let directory = tempfile::tempdir().unwrap();
        let admin = directory.path().join("admin.url");
        let ca = directory.path().join("ca.crt");
        let descriptor = directory.path().join("provider.json");
        fs::write(
            &admin,
            "postgresql://admin:secret@postgres.internal:5432/postgres?sslmode=require",
        )
        .unwrap();
        fs::write(&ca, "test-ca").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&admin, fs::Permissions::from_mode(0o600)).unwrap();
        }
        let document = serde_json::json!({
            "schema_version": 1,
            "provider_id": "postgresql-capacity",
            "host": "postgres.internal",
            "port": 5432,
            "tls_mode": "verify-full",
            "admin_url_file": admin,
            "ca_file": ca,
        });
        fs::write(&descriptor, serde_json::to_vec(&document).unwrap()).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&descriptor, fs::Permissions::from_mode(0o600)).unwrap();
        }
        let parsed = read_postgres_resource_provider(&descriptor).unwrap();
        assert_eq!(
            parsed.descriptor().unwrap().provider_id,
            "postgresql-capacity"
        );

        let mut invalid = document;
        invalid["schema_version"] = serde_json::json!(2);
        fs::write(&descriptor, serde_json::to_vec(&invalid).unwrap()).unwrap();
        assert!(read_postgres_resource_provider(&descriptor).is_err());
    }
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
    #[cfg(unix)]
    let workload_file_ownership = WorkloadFileOwnership::standard_v3();
    // Windows relies on the Agent state root's service-account ACL. A
    // numeric Unix owner cannot be represented, so retain exact inherited
    // ACLs without widening access and never pretend UID enforcement ran.
    #[cfg(not(unix))]
    let workload_file_ownership = WorkloadFileOwnership::current_process();
    validate_agent_workload_file_ownership(workload_file_ownership)?;
    let pipeline_bootstrap = if arguments.legacy_release_providers {
        PipelineBootstrapConfig::from_legacy_development_env()?
    } else {
        PipelineBootstrapConfig::from_remote_agent_env()?
    };
    let identity_store = IdentityStore::new(&arguments.identity_dir);
    let ledger_path = arguments
        .ledger
        .clone()
        .unwrap_or_else(|| arguments.identity_dir.join("execution-ledger.sqlite3"));
    let mut internal_state_roots = vec![
        arguments.identity_dir.clone(),
        parent_directory(&ledger_path, "ledger")?,
    ];
    if let Some(path) = arguments.registry_credentials.as_deref() {
        internal_state_roots.push(parent_directory(path, "registry credentials")?);
    }
    if let Some(path) = arguments.runtime_policy.as_deref() {
        internal_state_roots.push(parent_directory(path, "runtime policy")?);
    }
    if let Some(path) = arguments.postgres_resource_provider.as_deref() {
        internal_state_roots.push(parent_directory(path, "PostgreSQL provider descriptor")?);
    }
    if let Some(path) = arguments.resource_secret_dir.as_deref() {
        internal_state_roots.push(path.to_path_buf());
    }
    internal_state_roots.extend_from_slice(pipeline_bootstrap.internal_state_roots());
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
        let migration_inventory = runtime.migration_container_inventory(4_096).await?;
        let migration_reconciliation =
            reconcile_migration_containers(&mut ledger, &runtime, migration_inventory).await?;
        for warning in &migration_reconciliation.warnings {
            eprintln!("migration reconciliation: {warning}");
        }
        if !migration_reconciliation.safe_to_start_worker {
            return Err(format!(
                "migration reconciliation did not produce complete, safe facts (inspected={}, tombstoned={}, removed={}); refusing to claim jobs",
                migration_reconciliation.inspected,
                migration_reconciliation.tombstoned,
                migration_reconciliation.removed,
            )
            .into());
        }
        let runtime_facts = runtime.runtime_facts().await?;
        let local_runtime_provider = if let Some(path) = arguments.runtime_policy.as_deref() {
            LocalRuntimeContextProvider::from_json_file(path, runtime_facts)?
        } else {
            LocalRuntimeContextProvider::standard_only(
                runtime_facts,
                arguments.workload_export_dir.join("runtime-contexts"),
            )
        }
        .with_workload_file_ownership(workload_file_ownership)?
        .with_workload_export_boundary(
            arguments.workload_export_dir.clone(),
            internal_state_roots.clone(),
        )?
        .with_event_connections(pipeline_bootstrap.event_connection_urls().clone());
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
        let pipeline_provider =
            pipeline_bootstrap.build_release_provider(provider_state_database)?;
        let mut executor = JobExecutor::new(runtime)
            .with_pipeline_provider(std::sync::Arc::new(pipeline_provider))
            .with_artifact_fetcher(std::sync::Arc::new(artifact_fetcher))
            .with_runtime_context(
                std::sync::Arc::clone(&runtime_provider),
                std::sync::Arc::clone(&credential_supervisor),
            );
        if arguments.postgres_resource_provider.is_some() != arguments.resource_secret_dir.is_some()
        {
            return Err("--postgres-resource-provider and --resource-secret-dir must be configured together".into());
        }
        if let (Some(provider_file), Some(secret_root)) = (
            arguments.postgres_resource_provider.as_ref(),
            arguments.resource_secret_dir.as_ref(),
        ) {
            let provider_document = read_postgres_resource_provider(provider_file)?;
            internal_state_roots.push(parent_directory(
                &provider_document.admin_url_file,
                "PostgreSQL administrator URL",
            )?);
            // Re-run the full boundary validation now that the strict provider
            // document has revealed its indirect administrator secret path.
            orchestrator_agent::validate_isolated_workload_roots(
                &arguments.workload_export_dir,
                &internal_state_roots,
            )?;
            let provider = provider_document.descriptor()?;
            let mut admin_url_bytes = fs::read(&provider_document.admin_url_file)?;
            while admin_url_bytes
                .last()
                .is_some_and(|byte| matches!(byte, b'\n' | b'\r'))
            {
                admin_url_bytes.pop();
            }
            if admin_url_bytes
                .iter()
                .any(|byte| *byte == b'\n' || *byte == b'\r' || *byte == 0)
            {
                return Err(
                    "PostgreSQL administrator URL file must contain exactly one bounded text value"
                        .into(),
                );
            }
            let admin_url = SecretMaterial::new(admin_url_bytes)?;
            let receipts = ledger_path.with_file_name("resource-postgres-receipts.sqlite3");
            let live = LivePostgreSqlExecutor::new(PostgreSqlAdminConfigV1 {
                provider: provider.clone(),
                admin_url,
                tls_trust: provider_document.tls_trust(),
                state_database: receipts,
            })?;
            let secret_store = FileResourceSecretStore::new_isolated_with_ownership(
                secret_root,
                arguments.workload_export_dir.join("resource-outputs"),
                workload_file_ownership,
            )?;
            let manager = LocalResourceClaimManager::new(
                provider,
                live,
                secret_store,
                ledger_path.with_file_name("resource-claims.sqlite3"),
            )?;
            executor = executor.with_resource_claims(std::sync::Arc::new(manager));
        }
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

fn parent_directory(
    path: &std::path::Path,
    name: &str,
) -> Result<PathBuf, Box<dyn std::error::Error>> {
    path.parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .map(PathBuf::from)
        .ok_or_else(|| format!("{name} path must have an absolute parent directory").into())
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
