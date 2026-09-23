use async_trait::async_trait;
use orchestrator_runtime::{
    ContainerRuntime, ContainerSpec, DeploymentRuntimeObservationV1, DockerRuntimeFacts,
    JUDGE_SANDBOX_V1_PROFILE_SHA256, MANAGED_EVENT_CONNECTION_FILE,
    MANAGED_SERVICE_CREDENTIAL_FILE, MANAGED_SERVICE_GATEWAY_CA_FILE, ManagedApiBinding,
    ManagedEventBinding, ManagedEventSubscription, ManagedServiceContextSpec, OciImageReference,
    RuntimeContext, RuntimeContract, RuntimeProfile, WorkloadCredential, WorkloadFileOwnership,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs;
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use tokio::sync::Mutex;
use tokio::task::JoinHandle;

const RUNTIME_POLICY_SCHEMA_VERSION: u32 = 1;
const MAX_RUNTIME_POLICY_BYTES: u64 = 64 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum RuntimePolicyError {
    #[error("invalid Agent runtime policy: {0}")]
    InvalidPolicy(String),
    #[error("runtime profile {0} is not allowed by this Node")]
    ProfileNotAllowed(RuntimeProfile),
    #[error("Node runtime facts do not satisfy {profile}: {reason}")]
    UnsupportedRuntime {
        profile: RuntimeProfile,
        reason: String,
    },
    #[error("cannot materialize runtime context: {0}")]
    Materialization(String),
    #[error("cannot compensate runtime context: {0}")]
    Compensation(String),
    #[error("runtime facts publication failed: {0}")]
    Publication(String),
    #[error("workload credential exchange failed: {0}")]
    Credential(String),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct RuntimePolicyDocument {
    schema_version: u32,
    allowed_profiles: BTreeSet<RuntimeProfile>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    service_context_root: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    judge_sandbox: Option<JudgeSandboxLocalPolicy>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct JudgeSandboxLocalPolicy {
    profile_sha256: String,
    context_root: PathBuf,
    /// Exact signed Release artifacts this Node administrator authorizes for
    /// the privileged judge sandbox. Floating tags and repository wildcards
    /// are deliberately impossible to express.
    allowed_images: BTreeSet<String>,
}

/// The exact capability report a future control-plane endpoint must accept.
/// It deliberately advertises only closed runtime contracts already accepted
/// by both local policy and observed Docker facts.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct NodeRuntimeFactsV1 {
    pub schema_version: u32,
    #[serde(default)]
    pub report_id: String,
    /// Agent-clock lower bound for this inventory snapshot. It is captured
    /// before Docker enumeration starts, so a lifecycle completion carrying a
    /// watermark at or after this value is causally newer and cannot be
    /// overwritten by this report when its container is absent.
    pub observed_at_ms: i64,
    pub agent_version: String,
    pub runtime_policy_sha256: String,
    pub allowed_contracts: Vec<RuntimeContract>,
    #[serde(default)]
    pub judge_sandbox_allowed_images: Vec<String>,
    /// Agent-local Redis connection identifiers safe to publish. URLs and
    /// credentials remain only in the protected Agent configuration.
    #[serde(default)]
    pub redis_connection_ids: Vec<String>,
    pub docker: DockerRuntimeFacts,
    #[serde(default)]
    pub inventory_complete: bool,
    #[serde(default)]
    pub inventory_error: String,
    #[serde(default)]
    pub deployment_observations: Vec<DeploymentRuntimeObservationV1>,
    #[serde(default)]
    pub credential_statuses: Vec<CredentialRefreshStatus>,
}

/// mTLS transport contract for replacing the authenticated Node's latest
/// runtime facts. The Agent publishes at startup and every 30 seconds; these
/// facts are never projected from or into operator-editable Node labels.
#[async_trait]
pub trait NodeRuntimeFactsPublisher: Send + Sync {
    async fn publish_runtime_facts(
        &self,
        node_id: &str,
        facts: &NodeRuntimeFactsV1,
    ) -> Result<(), RuntimePolicyError>;
}

#[async_trait]
pub trait WorkloadCredentialExchanger: Send + Sync {
    async fn exchange_workload_credential(
        &self,
        request: WorkloadCredentialExchangeRequest<'_>,
    ) -> Result<WorkloadCredential, RuntimePolicyError>;
}

pub struct WorkloadCredentialExchangeRequest<'a> {
    pub deployment_id: &'a str,
    pub job_id: Option<&'a str>,
    pub lease_token: Option<&'a str>,
}

impl std::fmt::Debug for WorkloadCredentialExchangeRequest<'_> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("WorkloadCredentialExchangeRequest")
            .field("deployment_id", &self.deployment_id)
            .field("job_id", &self.job_id)
            .field("lease_token", &self.lease_token.map(|_| "[REDACTED]"))
            .finish()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CredentialRefreshStatus {
    pub deployment_id: String,
    pub expires_at_ms: i64,
    pub last_success_at_ms: i64,
    pub last_error: String,
}

#[derive(Clone)]
pub struct WorkloadCredentialSupervisor {
    exchanger: Arc<dyn WorkloadCredentialExchanger>,
    context_provider: Arc<dyn RuntimeContextProvider>,
    tasks: Arc<Mutex<HashMap<String, JoinHandle<()>>>>,
    status: Arc<Mutex<BTreeMap<String, CredentialRefreshStatus>>>,
}

impl WorkloadCredentialSupervisor {
    pub fn new(
        exchanger: Arc<dyn WorkloadCredentialExchanger>,
        context_provider: Arc<dyn RuntimeContextProvider>,
    ) -> Self {
        Self {
            exchanger,
            context_provider,
            tasks: Arc::new(Mutex::new(HashMap::new())),
            status: Arc::new(Mutex::new(BTreeMap::new())),
        }
    }

    pub async fn issue_initial(
        &self,
        deployment_id: &str,
        job_id: &str,
        lease_token: &str,
    ) -> Result<WorkloadCredential, RuntimePolicyError> {
        validate_deployment_id(deployment_id)?;
        let credential = self
            .exchanger
            .exchange_workload_credential(WorkloadCredentialExchangeRequest {
                deployment_id,
                job_id: Some(job_id),
                lease_token: Some(lease_token),
            })
            .await?;
        validate_issued_credential(&credential, crate::now_ms())?;
        Ok(credential)
    }

    pub async fn start_refresh(
        &self,
        deployment_id: &str,
        context: RuntimeContext,
        current_expires_at_ms: i64,
    ) -> Result<(), RuntimePolicyError> {
        validate_deployment_id(deployment_id)?;
        context
            .validate()
            .map_err(|error| RuntimePolicyError::Credential(error.to_string()))?;
        self.stop_refresh(deployment_id).await;
        let deployment = deployment_id.to_string();
        self.status.lock().await.insert(
            deployment.clone(),
            CredentialRefreshStatus {
                deployment_id: deployment.clone(),
                expires_at_ms: current_expires_at_ms,
                last_success_at_ms: crate::now_ms(),
                last_error: String::new(),
            },
        );
        let exchanger = Arc::clone(&self.exchanger);
        let provider = Arc::clone(&self.context_provider);
        let status = Arc::clone(&self.status);
        let task_deployment = deployment.clone();
        let task = tokio::spawn(async move {
            let mut expires_at_ms = current_expires_at_ms;
            loop {
                tokio::time::sleep(std::time::Duration::from_millis(refresh_delay_ms(
                    expires_at_ms,
                    crate::now_ms(),
                )))
                .await;
                match exchanger
                    .exchange_workload_credential(WorkloadCredentialExchangeRequest {
                        deployment_id: &task_deployment,
                        job_id: None,
                        lease_token: None,
                    })
                    .await
                    .and_then(|credential| {
                        validate_issued_credential(&credential, crate::now_ms())?;
                        Ok(credential)
                    }) {
                    Ok(credential) => {
                        match provider
                            .rotate_workload_credential(&context, &credential)
                            .await
                        {
                            Ok(()) => {
                                expires_at_ms = credential.expires_at_ms;
                                status.lock().await.insert(
                                    task_deployment.clone(),
                                    CredentialRefreshStatus {
                                        deployment_id: task_deployment.clone(),
                                        expires_at_ms,
                                        last_success_at_ms: crate::now_ms(),
                                        last_error: String::new(),
                                    },
                                );
                            }
                            Err(error) => {
                                update_refresh_error(
                                    &status,
                                    &task_deployment,
                                    expires_at_ms,
                                    &error.to_string(),
                                )
                                .await;
                                tokio::time::sleep(std::time::Duration::from_secs(30)).await;
                            }
                        }
                    }
                    Err(error) => {
                        update_refresh_error(
                            &status,
                            &task_deployment,
                            expires_at_ms,
                            &error.to_string(),
                        )
                        .await;
                        tokio::time::sleep(std::time::Duration::from_secs(30)).await;
                    }
                }
            }
        });
        self.tasks.lock().await.insert(deployment, task);
        Ok(())
    }

    pub async fn recover_active(
        &self,
        ledger: &crate::AgentLedger,
    ) -> Result<usize, RuntimePolicyError> {
        let active = ledger.active_runtime_contexts().map_err(|error| {
            RuntimePolicyError::Credential(format!(
                "read active runtime contexts during Agent recovery: {error}"
            ))
        })?;
        let credential_bound = active.iter().filter(|run| {
            run.managed_context
                .as_ref()
                .is_some_and(|managed| !managed.bindings.is_empty())
        });
        let mut recovered = 0;
        for run in credential_bound {
            let credential = self
                .exchanger
                .exchange_workload_credential(WorkloadCredentialExchangeRequest {
                    deployment_id: &run.deployment_id,
                    job_id: None,
                    lease_token: None,
                })
                .await?;
            validate_issued_credential(&credential, crate::now_ms())?;
            self.context_provider
                .rotate_workload_credential(&run.context, &credential)
                .await?;
            self.start_refresh(
                &run.deployment_id,
                run.context.clone(),
                credential.expires_at_ms,
            )
            .await?;
            recovered += 1;
        }
        Ok(recovered)
    }

    pub async fn stop_refresh(&self, deployment_id: &str) {
        if let Some(task) = self.tasks.lock().await.remove(deployment_id) {
            task.abort();
            // Wait until the cancelled task can no longer be inside a
            // credential-file replacement. Callers revoke or reconfigure the
            // same mounted context immediately after this returns, so merely
            // signalling cancellation would permit an older generation to
            // win the final atomic rename.
            let _ = task.await;
        }
        self.status.lock().await.remove(deployment_id);
    }

    pub async fn shutdown_all(&self) {
        let mut tasks = self.tasks.lock().await;
        for (_, task) in tasks.drain() {
            task.abort();
        }
        self.status.lock().await.clear();
    }

    pub async fn status(&self) -> Vec<CredentialRefreshStatus> {
        self.status.lock().await.values().cloned().collect()
    }

    pub async fn status_for(&self, deployment_id: &str) -> Option<CredentialRefreshStatus> {
        self.status.lock().await.get(deployment_id).cloned()
    }
}

pub async fn recover_pending_runtime_contexts(
    ledger: &mut crate::AgentLedger,
    provider: &dyn RuntimeContextProvider,
    runtime: &dyn ContainerRuntime,
) -> Result<usize, RuntimePolicyError> {
    let pending = ledger.pending_runtime_context_cleanups().map_err(|error| {
        RuntimePolicyError::Compensation(format!(
            "read pending runtime context cleanup ledger: {error}"
        ))
    })?;
    for run in &pending {
        ledger
            .begin_runtime_context_cleanup(&run.deployment_id, crate::now_ms())
            .map_err(|error| RuntimePolicyError::Compensation(error.to_string()))?;
        let volume = ledger
            .begin_managed_volume_cleanup(&run.deployment_id, crate::now_ms())
            .map_err(|error| RuntimePolicyError::Compensation(error.to_string()))?;
        if let Some(volume) = volume {
            let cleanup = if volume.lifecycle == orchestrator_runtime::RETAIN_VOLUME_LIFECYCLE {
                Ok(())
            } else {
                runtime.remove_managed_volume(&volume).await
            };
            if let Err(error) = cleanup {
                ledger
                    .mark_managed_volume_cleanup_needed(&run.deployment_id, crate::now_ms())
                    .map_err(|ledger_error| {
                        RuntimePolicyError::Compensation(format!(
                            "{error}; additionally failed to persist managed volume cleanup retry: {ledger_error}"
                        ))
                    })?;
                ledger
                    .mark_runtime_context_cleanup_needed(
                        &run.deployment_id,
                        &error.to_string(),
                        crate::now_ms(),
                    )
                    .map_err(|ledger_error| {
                        RuntimePolicyError::Compensation(format!(
                            "{error}; additionally failed to persist context cleanup retry: {ledger_error}"
                        ))
                    })?;
                return Err(RuntimePolicyError::Compensation(format!(
                    "recover owned managed volume {}: {error}",
                    volume.name
                )));
            }
            ledger
                .finish_managed_volume_cleanup(&run.deployment_id, crate::now_ms())
                .map_err(|error| RuntimePolicyError::Compensation(error.to_string()))?;
        }
        if let Err(error) = provider.compensate(&run.context).await {
            ledger
                .mark_runtime_context_cleanup_needed(
                    &run.deployment_id,
                    &error.to_string(),
                    crate::now_ms(),
                )
                .map_err(|ledger_error| {
                    RuntimePolicyError::Compensation(format!(
                        "{error}; additionally failed to persist cleanup retry: {ledger_error}"
                    ))
                })?;
            return Err(error);
        }
        ledger
            .finish_runtime_context_cleanup(&run.deployment_id, crate::now_ms())
            .map_err(|error| RuntimePolicyError::Compensation(error.to_string()))?;
    }
    Ok(pending.len())
}

async fn update_refresh_error(
    statuses: &Mutex<BTreeMap<String, CredentialRefreshStatus>>,
    deployment_id: &str,
    expires_at_ms: i64,
    error: &str,
) {
    let mut statuses = statuses.lock().await;
    let last_success_at_ms = statuses
        .get(deployment_id)
        .map(|status| status.last_success_at_ms)
        .unwrap_or_default();
    statuses.insert(
        deployment_id.to_string(),
        CredentialRefreshStatus {
            deployment_id: deployment_id.to_string(),
            expires_at_ms,
            last_success_at_ms,
            last_error: bounded_status_error(error),
        },
    );
}

fn bounded_status_error(value: &str) -> String {
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

fn refresh_delay_ms(expires_at_ms: i64, now_ms: i64) -> u64 {
    expires_at_ms
        .saturating_sub(now_ms)
        .saturating_sub(5 * 60_000)
        .max(0) as u64
}

fn validate_deployment_id(deployment_id: &str) -> Result<(), RuntimePolicyError> {
    if deployment_id.trim().is_empty()
        || deployment_id.len() > 256
        || deployment_id.chars().any(char::is_control)
    {
        return Err(RuntimePolicyError::Credential(
            "deployment_id is empty or exceeds protocol bounds".to_string(),
        ));
    }
    Ok(())
}

fn validate_issued_credential(
    credential: &WorkloadCredential,
    now_ms: i64,
) -> Result<(), RuntimePolicyError> {
    credential
        .validate_at(now_ms)
        .map_err(|error| RuntimePolicyError::Credential(error.to_string()))?;
    let ttl_ms = credential.expires_at_ms.saturating_sub(now_ms);
    if !(14 * 60_000..=16 * 60_000).contains(&ttl_ms) {
        return Err(RuntimePolicyError::Credential(format!(
            "control plane must issue a 15 minute credential (observed ttl_ms={ttl_ms})"
        )));
    }
    Ok(())
}

#[async_trait]
pub trait RuntimeContextProvider: Send + Sync {
    fn plan_context(
        &self,
        spec: &ContainerSpec,
    ) -> Result<Option<RuntimeContext>, RuntimePolicyError>;

    async fn materialize_context(
        &self,
        spec: &ContainerSpec,
        context: &RuntimeContext,
        credential: &WorkloadCredential,
    ) -> Result<(), RuntimePolicyError>;

    async fn materialize_unbound_context(
        &self,
        _spec: &ContainerSpec,
        _context: &RuntimeContext,
    ) -> Result<(), RuntimePolicyError> {
        Err(RuntimePolicyError::Materialization(
            "runtime context provider does not support an unbound mounted context".to_string(),
        ))
    }

    async fn reconfigure_context(
        &self,
        _deployment_id: &str,
        _service_id: &str,
        _managed: &ManagedServiceContextSpec,
        _context: &RuntimeContext,
        _credential: &WorkloadCredential,
    ) -> Result<(), RuntimePolicyError> {
        Err(RuntimePolicyError::Materialization(
            "runtime context provider does not support in-place binding reconfiguration"
                .to_string(),
        ))
    }

    async fn rotate_workload_credential(
        &self,
        context: &RuntimeContext,
        credential: &WorkloadCredential,
    ) -> Result<(), RuntimePolicyError>;

    async fn revoke_workload_credential(
        &self,
        _context: &RuntimeContext,
    ) -> Result<(), RuntimePolicyError> {
        Err(RuntimePolicyError::Materialization(
            "runtime context provider does not support workload credential revocation".to_string(),
        ))
    }

    async fn compensate(&self, context: &RuntimeContext) -> Result<(), RuntimePolicyError>;

    fn runtime_facts(&self) -> NodeRuntimeFactsV1;
}

#[derive(Debug, Clone)]
pub struct LocalRuntimeContextProvider {
    policy: RuntimePolicyDocument,
    policy_sha256: String,
    docker_facts: DockerRuntimeFacts,
    event_connections: BTreeMap<String, String>,
    workload_file_ownership: WorkloadFileOwnership,
    workload_export_root: Option<PathBuf>,
}

impl LocalRuntimeContextProvider {
    pub fn standard_only(docker_facts: DockerRuntimeFacts, service_context_root: PathBuf) -> Self {
        let policy = RuntimePolicyDocument {
            schema_version: RUNTIME_POLICY_SCHEMA_VERSION,
            allowed_profiles: BTreeSet::from([RuntimeProfile::StandardV1]),
            service_context_root: Some(service_context_root),
            judge_sandbox: None,
        };
        let policy_sha256 = digest_policy(&policy).expect("static standard policy serializes");
        Self {
            policy,
            policy_sha256,
            docker_facts,
            event_connections: BTreeMap::new(),
            workload_file_ownership: WorkloadFileOwnership::current_process(),
            workload_export_root: None,
        }
    }

    pub fn from_json_file(
        path: &Path,
        docker_facts: DockerRuntimeFacts,
    ) -> Result<Self, RuntimePolicyError> {
        let file = fs::File::open(path).map_err(|error| {
            RuntimePolicyError::InvalidPolicy(format!(
                "cannot open Agent runtime policy ({:?})",
                error.kind()
            ))
        })?;
        let metadata = file.metadata().map_err(|error| {
            RuntimePolicyError::InvalidPolicy(format!(
                "cannot inspect Agent runtime policy ({:?})",
                error.kind()
            ))
        })?;
        if !metadata.is_file() || metadata.len() == 0 || metadata.len() > MAX_RUNTIME_POLICY_BYTES {
            return Err(RuntimePolicyError::InvalidPolicy(
                "policy must be a non-empty regular JSON file no larger than 64 KiB".to_string(),
            ));
        }
        // Bound the read independently of metadata so a concurrent file replacement/growth
        // cannot make policy loading allocate or decode an unbounded byte stream.
        let mut reader = file.take(MAX_RUNTIME_POLICY_BYTES + 1);
        let mut bytes = Vec::with_capacity(metadata.len() as usize);
        reader.read_to_end(&mut bytes).map_err(|error| {
            RuntimePolicyError::InvalidPolicy(format!(
                "cannot read Agent runtime policy ({:?})",
                error.kind()
            ))
        })?;
        let policy = decode_runtime_policy(&bytes)?;
        validate_policy(&policy, &docker_facts)?;
        let policy_sha256 = digest_policy(&policy)?;
        Ok(Self {
            policy,
            policy_sha256,
            docker_facts,
            event_connections: BTreeMap::new(),
            workload_file_ownership: WorkloadFileOwnership::current_process(),
            workload_export_root: None,
        })
    }

    /// Select the identity that must own every service-context directory and
    /// file. Production callers must use `standard_v3`; the default current
    /// process policy exists so in-process unit tests need no elevated user.
    pub fn with_workload_file_ownership(
        mut self,
        ownership: WorkloadFileOwnership,
    ) -> Result<Self, RuntimePolicyError> {
        validate_agent_workload_file_ownership(ownership)?;
        self.workload_file_ownership = ownership;
        Ok(self)
    }

    /// Bind managed runtime contexts to one dedicated workload-export root.
    ///
    /// Docker resolves bind sources in the daemon's mount namespace.  A
    /// production DinD daemon may therefore receive this export root read-only,
    /// but must never receive any Agent identity, ledger or provider state root.
    /// Requiring the fixed `runtime-contexts` child also prevents a policy file
    /// from redirecting a workload bind into Agent-internal state.
    pub fn with_workload_export_boundary(
        mut self,
        export_root: PathBuf,
        internal_state_roots: Vec<PathBuf>,
    ) -> Result<Self, RuntimePolicyError> {
        validate_isolated_workload_roots(&export_root, &internal_state_roots)?;
        let expected_context_root = export_root.join("runtime-contexts");
        if self.context_root()? != expected_context_root {
            return Err(RuntimePolicyError::InvalidPolicy(format!(
                "service_context_root must equal the dedicated workload export path {}",
                expected_context_root.display()
            )));
        }
        create_private_directory(&export_root, self.workload_file_ownership)?;
        validate_isolated_workload_roots(&export_root, &internal_state_roots)?;
        self.workload_export_root = Some(export_root);
        Ok(self)
    }

    pub fn with_event_connections(mut self, connections: BTreeMap<String, String>) -> Self {
        self.event_connections = connections;
        self
    }

    fn judge_policy(&self) -> Result<&JudgeSandboxLocalPolicy, RuntimePolicyError> {
        if !self
            .policy
            .allowed_profiles
            .contains(&RuntimeProfile::JudgeSandboxV1)
        {
            return Err(RuntimePolicyError::ProfileNotAllowed(
                RuntimeProfile::JudgeSandboxV1,
            ));
        }
        self.policy.judge_sandbox.as_ref().ok_or_else(|| {
            RuntimePolicyError::InvalidPolicy(
                "judge-sandbox-v1 is allowed but judge_sandbox settings are missing".to_string(),
            )
        })
    }

    fn context_root(&self) -> Result<&Path, RuntimePolicyError> {
        self.policy
            .service_context_root
            .as_deref()
            .or_else(|| {
                self.policy
                    .judge_sandbox
                    .as_ref()
                    .map(|policy| policy.context_root.as_path())
            })
            .ok_or_else(|| {
                RuntimePolicyError::InvalidPolicy(
                    "service_context_root is required for managed workloads".to_string(),
                )
            })
    }

    fn validate_owned_context(
        &self,
        context: &RuntimeContext,
    ) -> Result<OwnedRuntimePaths, RuntimePolicyError> {
        self.validate_workload_export_boundary()?;
        context
            .validate()
            .map_err(|error| RuntimePolicyError::Compensation(error.to_string()))?;
        if context.runtime_policy_sha256 != self.policy_sha256 {
            return Err(RuntimePolicyError::Compensation(
                "context belongs to a different Agent runtime policy".to_string(),
            ));
        }
        let service_directory = Path::new(&context.service_context_directory);
        let context_directory = service_directory.parent().ok_or_else(|| {
            RuntimePolicyError::Compensation(
                "service context has no Agent-owned deployment directory".to_string(),
            )
        })?;
        if service_directory.file_name().and_then(|name| name.to_str()) != Some("service") {
            return Err(RuntimePolicyError::Compensation(
                "service context must end in the fixed service directory".to_string(),
            ));
        }
        let component = context_directory
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| {
                RuntimePolicyError::Compensation(
                    "service context has no Agent-owned deployment component".to_string(),
                )
            })?;
        if component.len() != 32
            || !component
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err(RuntimePolicyError::Compensation(
                "context deployment component is not a 128-bit lowercase digest".to_string(),
            ));
        }
        let expected_directory = self.context_root()?.join(component);
        if context_directory != expected_directory
            || service_directory != expected_directory.join("service")
        {
            return Err(RuntimePolicyError::Compensation(
                "context paths do not match the deterministic Agent policy expansion".to_string(),
            ));
        }
        match context.contract.id {
            RuntimeProfile::StandardV1 => {
                if !context.scratch_directory.is_empty() || !context.cache_volume_name.is_empty() {
                    return Err(RuntimePolicyError::Compensation(
                        "standard-container-v1 context contains sandbox-only paths".to_string(),
                    ));
                }
            }
            RuntimeProfile::JudgeSandboxV1 => {
                self.judge_policy()?;
                if Path::new(&context.scratch_directory) != expected_directory.join("work")
                    || context.cache_volume_name != format!("ojos-judge-cache-{component}")
                {
                    return Err(RuntimePolicyError::Compensation(
                        "judge context paths do not match the deterministic Agent policy expansion"
                            .to_string(),
                    ));
                }
            }
        }
        Ok(OwnedRuntimePaths {
            context_directory: expected_directory,
        })
    }

    fn validate_workload_export_boundary(&self) -> Result<(), RuntimePolicyError> {
        if let Some(export_root) = self.workload_export_root.as_deref() {
            validate_private_directory(export_root, self.workload_file_ownership)?;
        }
        Ok(())
    }
}

fn decode_runtime_policy(bytes: &[u8]) -> Result<RuntimePolicyDocument, RuntimePolicyError> {
    if bytes.is_empty() || bytes.len() > MAX_RUNTIME_POLICY_BYTES as usize {
        return Err(RuntimePolicyError::InvalidPolicy(
            "policy must be a non-empty regular JSON file no larger than 64 KiB".to_string(),
        ));
    }
    let document = std::str::from_utf8(bytes).map_err(|error| {
        RuntimePolicyError::InvalidPolicy(format!(
            "policy contains binary/invalid UTF-8 input ({} bytes; first invalid byte {})",
            bytes.len(),
            error.valid_up_to()
        ))
    })?;
    serde_json::from_str(document).map_err(|error| {
        RuntimePolicyError::InvalidPolicy(format!(
            "strict JSON decode failed at line {} column {}",
            error.line(),
            error.column()
        ))
    })
}

#[derive(Debug)]
struct OwnedRuntimePaths {
    context_directory: PathBuf,
}

#[async_trait]
impl RuntimeContextProvider for LocalRuntimeContextProvider {
    fn plan_context(
        &self,
        spec: &ContainerSpec,
    ) -> Result<Option<RuntimeContext>, RuntimePolicyError> {
        spec.runtime_contract
            .validate()
            .map_err(|error| RuntimePolicyError::InvalidPolicy(error.to_string()))?;
        if !self
            .policy
            .allowed_profiles
            .contains(&spec.runtime_contract.id)
        {
            return Err(RuntimePolicyError::ProfileNotAllowed(
                spec.runtime_contract.id,
            ));
        }
        if spec.managed_service_context.is_none() {
            if spec.runtime_contract.id == RuntimeProfile::JudgeSandboxV1 {
                return Err(RuntimePolicyError::InvalidPolicy(
                    "judge-sandbox-v1 requires managed_service_context".to_string(),
                ));
            }
            return Ok(None);
        }
        spec.managed_service_context
            .as_ref()
            .expect("checked above")
            .validate()
            .map_err(|error| RuntimePolicyError::InvalidPolicy(error.to_string()))?;
        if spec.runtime_contract.id == RuntimeProfile::JudgeSandboxV1 {
            validate_judge_runtime_facts(&self.docker_facts)?;
            let local = self.judge_policy()?;
            let image = spec.image.to_string();
            if !local.allowed_images.contains(&image) {
                return Err(RuntimePolicyError::InvalidPolicy(format!(
                    "judge-sandbox-v1 image {image} is not explicitly authorized by this Node"
                )));
            }
            if spec
                .labels
                .get("ojos.catalog_signature_verified")
                .map(String::as_str)
                != Some("true")
                || spec
                    .labels
                    .get("ojos.service_contract_version")
                    .map(String::as_str)
                    != Some("2")
                || !spec
                    .labels
                    .get("ojos.release_checksum")
                    .is_some_and(|value| valid_sha256_text(value))
            {
                return Err(RuntimePolicyError::InvalidPolicy(
                    "judge-sandbox-v1 accepts only a signature-verified Store v2 Release with a metadata checksum"
                        .to_string(),
                ));
            }
        }
        let component = deployment_component(&spec.deployment_id);
        let context_directory = self.context_root()?.join(&component);
        let scratch_directory = context_directory.join("work");
        let context = RuntimeContext {
            contract: spec.runtime_contract.clone(),
            runtime_policy_sha256: self.policy_sha256.clone(),
            scratch_directory: if spec.runtime_contract.id == RuntimeProfile::JudgeSandboxV1 {
                path_text(&scratch_directory)?
            } else {
                String::new()
            },
            cache_volume_name: if spec.runtime_contract.id == RuntimeProfile::JudgeSandboxV1 {
                format!("ojos-judge-cache-{component}")
            } else {
                String::new()
            },
            service_context_directory: path_text(&context_directory.join("service"))?,
        };
        context
            .validate()
            .map_err(|error| RuntimePolicyError::InvalidPolicy(error.to_string()))?;
        Ok(Some(context))
    }

    async fn materialize_context(
        &self,
        spec: &ContainerSpec,
        context: &RuntimeContext,
        credential: &WorkloadCredential,
    ) -> Result<(), RuntimePolicyError> {
        let paths = self
            .validate_owned_context(context)
            .map_err(|error| RuntimePolicyError::Materialization(error.to_string()))?;
        if context.contract.id == RuntimeProfile::JudgeSandboxV1 {
            self.judge_policy()?;
            validate_judge_runtime_facts(&self.docker_facts)?;
        }
        create_private_directory(self.context_root()?, self.workload_file_ownership)?;
        create_private_directory(&paths.context_directory, self.workload_file_ownership)?;
        if context.contract.id == RuntimeProfile::JudgeSandboxV1 {
            create_private_directory(
                Path::new(&context.scratch_directory),
                self.workload_file_ownership,
            )?;
        }
        materialize_service_context(
            spec,
            context,
            credential,
            &self.event_connections,
            self.workload_file_ownership,
        )?;
        Ok(())
    }

    async fn materialize_unbound_context(
        &self,
        spec: &ContainerSpec,
        context: &RuntimeContext,
    ) -> Result<(), RuntimePolicyError> {
        self.validate_owned_context(context)?;
        let managed = spec.managed_service_context.as_ref().ok_or_else(|| {
            RuntimePolicyError::Materialization("managed service context is missing".to_string())
        })?;
        if !managed.bindings.is_empty() || context.contract.id == RuntimeProfile::JudgeSandboxV1 {
            return Err(RuntimePolicyError::Materialization(
                "only standard-container-v1 with zero active optional bindings may materialize an unbound context"
                    .to_string(),
            ));
        }
        create_private_directory(self.context_root()?, self.workload_file_ownership)?;
        create_private_directory(
            Path::new(&context.service_context_directory),
            self.workload_file_ownership,
        )?;
        materialize_service_context_fields(
            &spec.deployment_id,
            &spec.service_id,
            managed,
            context,
            None,
            &self.event_connections,
            self.workload_file_ownership,
        )
    }

    async fn reconfigure_context(
        &self,
        deployment_id: &str,
        service_id: &str,
        managed: &ManagedServiceContextSpec,
        context: &RuntimeContext,
        credential: &WorkloadCredential,
    ) -> Result<(), RuntimePolicyError> {
        self.validate_owned_context(context)?;
        materialize_service_context_fields(
            deployment_id,
            service_id,
            managed,
            context,
            Some(credential),
            &self.event_connections,
            self.workload_file_ownership,
        )
    }

    async fn rotate_workload_credential(
        &self,
        context: &RuntimeContext,
        credential: &WorkloadCredential,
    ) -> Result<(), RuntimePolicyError> {
        self.validate_owned_context(context)?;
        credential
            .validate_at(crate::now_ms())
            .map_err(|error| RuntimePolicyError::Materialization(error.to_string()))?;
        atomic_private_write(
            &Path::new(&context.service_context_directory).join("token"),
            credential.access_token.as_bytes(),
            self.workload_file_ownership,
        )
    }

    async fn revoke_workload_credential(
        &self,
        context: &RuntimeContext,
    ) -> Result<(), RuntimePolicyError> {
        self.validate_owned_context(context)?;
        // Preserve the deployment context tree, work/cache directories and
        // container bind mount. An atomic empty token makes every subsequent
        // SDK reload fail closed without changing the mounted inode tree.
        atomic_private_write(
            &Path::new(&context.service_context_directory).join("token"),
            b"",
            self.workload_file_ownership,
        )
    }

    async fn compensate(&self, context: &RuntimeContext) -> Result<(), RuntimePolicyError> {
        let paths = self.validate_owned_context(context)?;
        remove_owned_tree(&paths.context_directory)?;
        Ok(())
    }

    fn runtime_facts(&self) -> NodeRuntimeFactsV1 {
        NodeRuntimeFactsV1 {
            schema_version: RUNTIME_POLICY_SCHEMA_VERSION,
            report_id: String::new(),
            observed_at_ms: crate::now_ms(),
            agent_version: env!("CARGO_PKG_VERSION").to_string(),
            runtime_policy_sha256: self.policy_sha256.clone(),
            allowed_contracts: self
                .policy
                .allowed_profiles
                .iter()
                .copied()
                .map(RuntimeContract::for_profile)
                .collect(),
            judge_sandbox_allowed_images: self
                .policy
                .judge_sandbox
                .as_ref()
                .map(|policy| policy.allowed_images.iter().cloned().collect())
                .unwrap_or_default(),
            redis_connection_ids: self.event_connections.keys().cloned().collect(),
            docker: self.docker_facts.clone(),
            inventory_complete: false,
            inventory_error: "deployment inventory has not been sampled".to_string(),
            deployment_observations: Vec::new(),
            credential_statuses: Vec::new(),
        }
    }
}

fn validate_policy(
    policy: &RuntimePolicyDocument,
    docker_facts: &DockerRuntimeFacts,
) -> Result<(), RuntimePolicyError> {
    if policy.schema_version != RUNTIME_POLICY_SCHEMA_VERSION {
        return Err(RuntimePolicyError::InvalidPolicy(format!(
            "schema_version must be {RUNTIME_POLICY_SCHEMA_VERSION}"
        )));
    }
    if policy.allowed_profiles.is_empty()
        || !policy
            .allowed_profiles
            .contains(&RuntimeProfile::StandardV1)
    {
        return Err(RuntimePolicyError::InvalidPolicy(
            "allowed_profiles must include standard-container-v1".to_string(),
        ));
    }
    let context_root = policy
        .service_context_root
        .as_deref()
        .or_else(|| {
            policy
                .judge_sandbox
                .as_ref()
                .map(|settings| settings.context_root.as_path())
        })
        .ok_or_else(|| {
            RuntimePolicyError::InvalidPolicy(
                "service_context_root is required for managed workloads".to_string(),
            )
        })?;
    validate_absolute_path("service_context_root", context_root)?;
    if let (Some(service_root), Some(judge)) = (
        policy.service_context_root.as_deref(),
        policy.judge_sandbox.as_ref(),
    ) && service_root != judge.context_root
    {
        return Err(RuntimePolicyError::InvalidPolicy(
            "service_context_root and judge_sandbox.context_root must match".to_string(),
        ));
    }
    match (
        policy
            .allowed_profiles
            .contains(&RuntimeProfile::JudgeSandboxV1),
        policy.judge_sandbox.as_ref(),
    ) {
        (true, Some(local)) => {
            validate_judge_runtime_facts(docker_facts)?;
            if local.profile_sha256 != JUDGE_SANDBOX_V1_PROFILE_SHA256 {
                return Err(RuntimePolicyError::InvalidPolicy(format!(
                    "judge-sandbox-v1 profile_sha256 must be {JUDGE_SANDBOX_V1_PROFILE_SHA256}"
                )));
            }
            validate_absolute_path("context_root", &local.context_root)?;
            if local.allowed_images.is_empty() || local.allowed_images.len() > 128 {
                return Err(RuntimePolicyError::InvalidPolicy(
                    "judge-sandbox-v1 allowed_images must contain between 1 and 128 exact OCI digests"
                        .to_string(),
                ));
            }
            for image in &local.allowed_images {
                let parsed = OciImageReference::parse(image).map_err(|error| {
                    RuntimePolicyError::InvalidPolicy(format!(
                        "judge-sandbox-v1 allowed image {image:?} is invalid: {error}"
                    ))
                })?;
                if parsed.to_string() != *image {
                    return Err(RuntimePolicyError::InvalidPolicy(format!(
                        "judge-sandbox-v1 allowed image {image:?} is not canonical repository@sha256"
                    )));
                }
            }
        }
        (true, None) => {
            return Err(RuntimePolicyError::InvalidPolicy(
                "judge-sandbox-v1 requires judge_sandbox settings".to_string(),
            ));
        }
        (false, Some(_)) => {
            return Err(RuntimePolicyError::InvalidPolicy(
                "judge_sandbox settings are forbidden unless judge-sandbox-v1 is allowed"
                    .to_string(),
            ));
        }
        (false, None) => {}
    }
    Ok(())
}

fn valid_sha256_text(value: &str) -> bool {
    value.strip_prefix("sha256:").is_some_and(|digest| {
        digest.len() == 64
            && digest
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    })
}

fn validate_judge_runtime_facts(facts: &DockerRuntimeFacts) -> Result<(), RuntimePolicyError> {
    let reject = |reason: &str| RuntimePolicyError::UnsupportedRuntime {
        profile: RuntimeProfile::JudgeSandboxV1,
        reason: reason.to_string(),
    };
    if facts.engine != "docker" {
        return Err(reject("Docker Engine is required"));
    }
    if facts.os_type != "linux" {
        return Err(reject("Linux Docker Engine is required"));
    }
    if facts.cgroup_version != "2" {
        return Err(reject("delegated cgroup v2 is required"));
    }
    if !facts.memory_limit || !facts.pids_limit {
        return Err(reject("Docker memory and pids controllers are required"));
    }
    if facts.rootless {
        return Err(reject(
            "rootless Docker cannot provide the fixed cgroup/nsjail contract",
        ));
    }
    // judge-sandbox-v1 always creates the container with the exact
    // `apparmor=unconfined` HostConfig option and verifies that option again
    // after create. Docker Desktop and nested Docker can truthfully report no
    // AppArmor LSM while still accepting and preserving that explicit
    // unconfined option. Requiring the host to advertise AppArmor would reject
    // the intended unconfined execution semantics before Docker can prove the
    // actual container configuration. Engines that reject the option still
    // fail closed during create, and the runtime adapter rejects HostConfig
    // drift after create.
    if !facts.seccomp {
        return Err(reject("Docker seccomp support is required"));
    }
    Ok(())
}

#[derive(Serialize)]
struct ServiceContextDocument<'a> {
    schema_version: u32,
    deployment: ServiceDeploymentDocument<'a>,
    gateway: ServiceGatewayDocument<'a>,
    bindings: BTreeMap<&'a str, ServiceBindingDocument<'a>>,
    credential_file: &'static str,
    generation: u64,
}

#[derive(Serialize)]
struct ServiceDeploymentDocument<'a> {
    id: &'a str,
    service: &'a str,
    node: &'a str,
}

#[derive(Serialize)]
struct ServiceGatewayDocument<'a> {
    origin: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    ca_file: Option<&'static str>,
}

#[derive(Serialize)]
struct ServiceBindingDocument<'a> {
    binding_id: &'a str,
    api_id: &'a str,
    base_path: String,
    timeout_ms: u64,
}

#[derive(Serialize)]
struct EventContextDocument<'a> {
    schema_version: u32,
    deployment: ServiceDeploymentDocument<'a>,
    connection_id: &'a str,
    connection_file: &'static str,
    stream: &'a str,
    publish_types: &'a [String],
    subscriptions: &'a [ManagedEventSubscription],
    generation: u64,
}

fn materialize_service_context(
    spec: &ContainerSpec,
    context: &RuntimeContext,
    credential: &WorkloadCredential,
    event_connections: &BTreeMap<String, String>,
    ownership: WorkloadFileOwnership,
) -> Result<(), RuntimePolicyError> {
    let managed = spec.managed_service_context.as_ref().ok_or_else(|| {
        RuntimePolicyError::Materialization(
            "judge-sandbox-v1 requires managed_service_context".to_string(),
        )
    })?;
    materialize_service_context_fields(
        &spec.deployment_id,
        &spec.service_id,
        managed,
        context,
        Some(credential),
        event_connections,
        ownership,
    )
}

fn materialize_service_context_fields(
    deployment_id: &str,
    service_id: &str,
    managed: &ManagedServiceContextSpec,
    context: &RuntimeContext,
    credential: Option<&WorkloadCredential>,
    event_connections: &BTreeMap<String, String>,
    ownership: WorkloadFileOwnership,
) -> Result<(), RuntimePolicyError> {
    managed
        .validate()
        .map_err(|error| RuntimePolicyError::Materialization(error.to_string()))?;
    if let Some(credential) = credential {
        credential
            .validate_at(crate::now_ms())
            .map_err(|error| RuntimePolicyError::Materialization(error.to_string()))?;
    } else if !managed.bindings.is_empty() {
        return Err(RuntimePolicyError::Materialization(
            "active API bindings require a workload credential".to_string(),
        ));
    }
    let directory = Path::new(&context.service_context_directory);
    create_private_directory(directory, ownership)?;

    let credential_path = directory.join("token");
    let gateway_ca_path = directory.join("ca.pem");
    let event_context_path = directory.join("events.json");
    let event_connection_path = directory.join("event-redis.url");
    let workload_public_key_path = directory.join("workload-public-key.pem");
    let ca_file = if managed.gateway_ca_pem.is_some() {
        Some(MANAGED_SERVICE_GATEWAY_CA_FILE)
    } else {
        None
    };

    let bindings = managed
        .bindings
        .iter()
        .map(|(name, binding): (&String, &ManagedApiBinding)| {
            (
                name.as_str(),
                ServiceBindingDocument {
                    binding_id: &binding.binding_id,
                    api_id: &binding.api_id,
                    base_path: format!("/internal/apis/{}", binding.api_id),
                    timeout_ms: binding.timeout_ms,
                },
            )
        })
        .collect();
    let document = ServiceContextDocument {
        schema_version: 1,
        deployment: ServiceDeploymentDocument {
            id: deployment_id,
            service: service_id,
            node: &managed.node_id,
        },
        gateway: ServiceGatewayDocument {
            origin: managed.gateway_origin.trim_end_matches('/'),
            ca_file,
        },
        bindings,
        credential_file: MANAGED_SERVICE_CREDENTIAL_FILE,
        generation: managed.generation,
    };
    let bytes = serde_json::to_vec(&document).map_err(|error| {
        RuntimePolicyError::Materialization(format!("encode service context: {error}"))
    })?;
    let context_path = directory.join("context.json");
    let event_materialization = managed
        .events
        .as_ref()
        .map(|events: &ManagedEventBinding| {
            let connection = event_connections
                .get(&events.connection_id)
                .map(String::as_str)
                .filter(|value| {
                    !value.trim().is_empty()
                        && value.len() <= 64 * 1024
                        && !value.chars().any(char::is_whitespace)
                })
                .ok_or_else(|| {
                    RuntimePolicyError::Materialization(format!(
                        "Agent-local Redis connection {} is missing or invalid",
                        events.connection_id
                    ))
                })?;
            let document = EventContextDocument {
                schema_version: 1,
                deployment: ServiceDeploymentDocument {
                    id: deployment_id,
                    service: service_id,
                    node: &managed.node_id,
                },
                connection_id: &events.connection_id,
                connection_file: MANAGED_EVENT_CONNECTION_FILE,
                stream: &events.stream,
                publish_types: &events.publish_types,
                subscriptions: &events.subscriptions,
                generation: events.generation,
            };
            let bytes = serde_json::to_vec(&document).map_err(|error| {
                RuntimePolicyError::Materialization(format!(
                    "encode managed event context: {error}"
                ))
            })?;
            Ok::<_, RuntimePolicyError>((connection.as_bytes().to_vec(), bytes))
        })
        .transpose()?;

    // All fallible encoding/validation is complete before committing files.
    // CA and context are prepared first; the credential is the commit marker.
    // Any write failure restores the byte-exact prior generation.
    let previous_ca = read_optional_file(&gateway_ca_path)?;
    let previous_context = read_optional_file(&context_path)?;
    let previous_token = read_optional_file(&credential_path)?;
    let previous_event_context = read_optional_file(&event_context_path)?;
    let previous_event_connection = read_optional_file(&event_connection_path)?;
    let previous_workload_public_key = read_optional_file(&workload_public_key_path)?;
    let apply = (|| {
        match managed.gateway_ca_pem.as_deref() {
            Some(pem) => atomic_private_write(&gateway_ca_path, pem.as_bytes(), ownership)?,
            None => remove_file_if_present(&gateway_ca_path)?,
        }
        atomic_private_write(&context_path, &bytes, ownership)?;
        match managed.workload_verifier.as_ref() {
            Some(verifier) => atomic_private_write(
                &workload_public_key_path,
                verifier.public_key_pem.as_bytes(),
                ownership,
            )?,
            None => remove_file_if_present(&workload_public_key_path)?,
        }
        match event_materialization.as_ref() {
            Some((connection, document)) => {
                atomic_private_write(&event_connection_path, connection, ownership)?;
                atomic_private_write(&event_context_path, document, ownership)?;
            }
            None => {
                remove_file_if_present(&event_context_path)?;
                remove_file_if_present(&event_connection_path)?;
            }
        }
        atomic_private_write(
            &credential_path,
            credential
                .map(|credential| credential.access_token.as_bytes())
                .unwrap_or_default(),
            ownership,
        )?;
        Ok::<(), RuntimePolicyError>(())
    })();
    if let Err(error) = apply {
        let mut rollback_errors = Vec::new();
        for (path, previous) in [
            (&gateway_ca_path, previous_ca.as_deref()),
            (&context_path, previous_context.as_deref()),
            (&credential_path, previous_token.as_deref()),
            (&event_context_path, previous_event_context.as_deref()),
            (&event_connection_path, previous_event_connection.as_deref()),
            (
                &workload_public_key_path,
                previous_workload_public_key.as_deref(),
            ),
        ] {
            let restored = match previous {
                Some(bytes) => atomic_private_write(path, bytes, ownership),
                None => remove_file_if_present(path),
            };
            if let Err(restore) = restored {
                rollback_errors.push(restore.to_string());
            }
        }
        if rollback_errors.is_empty() {
            return Err(error);
        }
        return Err(RuntimePolicyError::Compensation(format!(
            "service context apply failed ({error}); byte-exact rollback failed: {}",
            rollback_errors.join("; ")
        )));
    }
    Ok(())
}

fn read_optional_file(path: &Path) -> Result<Option<Vec<u8>>, RuntimePolicyError> {
    match fs::read(path) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(RuntimePolicyError::Materialization(format!(
            "read prior managed file {}: {error}",
            path.display()
        ))),
    }
}

fn remove_file_if_present(path: &Path) -> Result<(), RuntimePolicyError> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(RuntimePolicyError::Materialization(format!(
            "remove managed file {}: {error}",
            path.display()
        ))),
    }
}

fn atomic_private_write(
    path: &Path,
    bytes: &[u8],
    ownership: WorkloadFileOwnership,
) -> Result<(), RuntimePolicyError> {
    let parent = path.parent().ok_or_else(|| {
        RuntimePolicyError::Materialization(format!(
            "managed file {} has no parent directory",
            path.display()
        ))
    })?;
    create_private_directory(parent, ownership)?;
    let mut temporary = tempfile::NamedTempFile::new_in(parent).map_err(|error| {
        RuntimePolicyError::Materialization(format!(
            "create temporary managed file beside {}: {error}",
            path.display()
        ))
    })?;
    temporary.write_all(bytes).map_err(|error| {
        RuntimePolicyError::Materialization(format!(
            "write temporary managed file for {}: {error}",
            path.display()
        ))
    })?;
    temporary.flush().map_err(|error| {
        RuntimePolicyError::Materialization(format!(
            "flush temporary managed file for {}: {error}",
            path.display()
        ))
    })?;
    temporary.as_file().sync_all().map_err(|error| {
        RuntimePolicyError::Materialization(format!(
            "sync temporary managed file for {}: {error}",
            path.display()
        ))
    })?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        temporary
            .as_file()
            .set_permissions(fs::Permissions::from_mode(0o600))
            .map_err(|error| {
                RuntimePolicyError::Materialization(format!(
                    "set private permissions for {}: {error}",
                    path.display()
                ))
            })?;
        temporary.as_file().sync_all().map_err(|error| {
            RuntimePolicyError::Materialization(format!(
                "sync private permissions for {}: {error}",
                path.display()
            ))
        })?;
        verify_unix_file_ownership(temporary.as_file(), path, ownership, 0o600)?;
    }
    temporary.persist(path).map_err(|error| {
        RuntimePolicyError::Materialization(format!(
            "atomically replace {}: {}",
            path.display(),
            error.error
        ))
    })?;
    #[cfg(unix)]
    {
        let published = fs::File::open(path).map_err(|error| {
            RuntimePolicyError::Materialization(format!(
                "inspect published managed file {}: {error}",
                path.display()
            ))
        })?;
        verify_unix_file_ownership(&published, path, ownership, 0o600)?;
        fs::File::open(parent)
            .and_then(|directory| directory.sync_all())
            .map_err(|error| {
                RuntimePolicyError::Materialization(format!(
                    "sync managed directory {}: {error}",
                    parent.display()
                ))
            })?;
    }
    Ok(())
}

fn digest_policy(policy: &RuntimePolicyDocument) -> Result<String, RuntimePolicyError> {
    let canonical = serde_json::to_vec(policy).map_err(|error| {
        RuntimePolicyError::InvalidPolicy(format!("cannot encode canonical policy: {error}"))
    })?;
    Ok(format!("sha256:{:x}", Sha256::digest(canonical)))
}

fn deployment_component(deployment_id: &str) -> String {
    let digest = format!("{:x}", Sha256::digest(deployment_id.as_bytes()));
    digest[..32].to_string()
}

fn validate_absolute_path(name: &str, path: &Path) -> Result<(), RuntimePolicyError> {
    if !path.is_absolute()
        || path
            .components()
            .any(|component| matches!(component, Component::ParentDir))
    {
        return Err(RuntimePolicyError::InvalidPolicy(format!(
            "{name} must be an absolute normalized path"
        )));
    }
    Ok(())
}

/// Validate that a daemon-visible export root cannot alias or contain any
/// Agent-internal state.  Existing ancestors are canonicalized so symlinked
/// roots cannot bypass the lexical check; neither configured root itself may
/// be a symlink.
pub fn validate_isolated_workload_roots(
    export_root: &Path,
    internal_state_roots: &[PathBuf],
) -> Result<(), RuntimePolicyError> {
    validate_absolute_path("workload_export_root", export_root)?;
    if internal_state_roots.is_empty() {
        return Err(RuntimePolicyError::InvalidPolicy(
            "workload export isolation requires at least one Agent-internal state root".to_string(),
        ));
    }
    let export = canonicalize_configured_root("workload_export_root", export_root)?;
    for internal_root in internal_state_roots {
        validate_absolute_path("internal_state_root", internal_root)?;
        let internal = canonicalize_configured_root("internal_state_root", internal_root)?;
        if roots_overlap(&export, &internal) {
            return Err(RuntimePolicyError::InvalidPolicy(format!(
                "workload export root {} overlaps Agent-internal state root {}",
                export_root.display(),
                internal_root.display()
            )));
        }
    }
    Ok(())
}

fn roots_overlap(left: &Path, right: &Path) -> bool {
    left == right || left.starts_with(right) || right.starts_with(left)
}

fn canonicalize_configured_root(name: &str, path: &Path) -> Result<PathBuf, RuntimePolicyError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => Err(
            RuntimePolicyError::InvalidPolicy(format!("{name} must not be a symlink")),
        ),
        Ok(metadata) if !metadata.is_dir() => Err(RuntimePolicyError::InvalidPolicy(format!(
            "{name} must be a directory"
        ))),
        Ok(_) => fs::canonicalize(path).map_err(|error| {
            RuntimePolicyError::InvalidPolicy(format!(
                "cannot canonicalize {name} {}: {error}",
                path.display()
            ))
        }),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            canonicalize_missing_root(name, path)
        }
        Err(error) => Err(RuntimePolicyError::InvalidPolicy(format!(
            "cannot inspect {name} {}: {error}",
            path.display()
        ))),
    }
}

fn canonicalize_missing_root(name: &str, path: &Path) -> Result<PathBuf, RuntimePolicyError> {
    let mut missing = Vec::new();
    let mut ancestor = path;
    loop {
        match fs::symlink_metadata(ancestor) {
            Ok(metadata) => {
                if metadata.file_type().is_symlink() || !metadata.is_dir() {
                    return Err(RuntimePolicyError::InvalidPolicy(format!(
                        "existing ancestor of {name} must be a real directory"
                    )));
                }
                let mut canonical = fs::canonicalize(ancestor).map_err(|error| {
                    RuntimePolicyError::InvalidPolicy(format!(
                        "cannot canonicalize existing ancestor of {name}: {error}"
                    ))
                })?;
                for component in missing.iter().rev() {
                    canonical.push(component);
                }
                return Ok(canonical);
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let component = ancestor.file_name().ok_or_else(|| {
                    RuntimePolicyError::InvalidPolicy(format!(
                        "{name} has no existing directory ancestor"
                    ))
                })?;
                missing.push(component.to_os_string());
                ancestor = ancestor.parent().ok_or_else(|| {
                    RuntimePolicyError::InvalidPolicy(format!(
                        "{name} has no existing directory ancestor"
                    ))
                })?;
            }
            Err(error) => {
                return Err(RuntimePolicyError::InvalidPolicy(format!(
                    "cannot inspect existing ancestor of {name}: {error}"
                )));
            }
        }
    }
}

fn validate_private_directory(
    path: &Path,
    _ownership: WorkloadFileOwnership,
) -> Result<(), RuntimePolicyError> {
    let metadata = fs::symlink_metadata(path).map_err(|error| {
        RuntimePolicyError::Materialization(format!(
            "inspect private workload export directory {}: {error}",
            path.display()
        ))
    })?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(RuntimePolicyError::Materialization(format!(
            "workload export root {} is not a real directory",
            path.display()
        )));
    }
    #[cfg(unix)]
    {
        let directory = fs::File::open(path).map_err(|error| {
            RuntimePolicyError::Materialization(format!(
                "open private workload export directory {}: {error}",
                path.display()
            ))
        })?;
        verify_unix_file_ownership(&directory, path, _ownership, 0o700)?;
    }
    Ok(())
}

fn create_private_directory(
    path: &Path,
    ownership: WorkloadFileOwnership,
) -> Result<(), RuntimePolicyError> {
    validate_agent_workload_file_ownership(ownership)?;
    if fs::symlink_metadata(path).is_ok_and(|metadata| metadata.file_type().is_symlink()) {
        return Err(RuntimePolicyError::Materialization(format!(
            "private workload directory {} must not be a symlink",
            path.display()
        )));
    }
    fs::create_dir_all(path).map_err(|error| {
        RuntimePolicyError::Materialization(format!("create {}: {error}", path.display()))
    })?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700)).map_err(|error| {
            RuntimePolicyError::Materialization(format!(
                "set private permissions on {}: {error}",
                path.display()
            ))
        })?;
        let directory = fs::File::open(path).map_err(|error| {
            RuntimePolicyError::Materialization(format!(
                "inspect private directory {}: {error}",
                path.display()
            ))
        })?;
        verify_unix_file_ownership(&directory, path, ownership, 0o700)?;
    }
    validate_private_directory(path, ownership)?;
    Ok(())
}

/// Prove the Agent can create files with the selected workload identity
/// without CHOWN. Production calls this before certificate, ledger, or Docker
/// work so a unit/user mismatch cannot perform partial runtime mutations.
pub fn validate_agent_workload_file_ownership(
    ownership: WorkloadFileOwnership,
) -> Result<(), RuntimePolicyError> {
    #[cfg(unix)]
    {
        if let Some((uid, gid)) = ownership.unix_ids() {
            // SAFETY: geteuid/getegid take no pointers and have no preconditions.
            let (effective_uid, effective_gid) = unsafe { (libc::geteuid(), libc::getegid()) };
            if effective_uid != uid || effective_gid != gid {
                return Err(RuntimePolicyError::Materialization(format!(
                    "workload files require Agent effective identity {uid}:{gid}, observed {effective_uid}:{effective_gid}; refusing CAP_CHOWN fallback"
                )));
            }
        }
        Ok(())
    }
    #[cfg(not(unix))]
    {
        if ownership.unix_ids().is_some() {
            return Err(RuntimePolicyError::Materialization(
                "explicit Unix workload ownership is unsupported on this platform".to_string(),
            ));
        }
        Ok(())
    }
}

#[cfg(unix)]
fn verify_unix_file_ownership(
    file: &fs::File,
    path: &Path,
    ownership: WorkloadFileOwnership,
    expected_mode: u32,
) -> Result<(), RuntimePolicyError> {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    let metadata = file.metadata().map_err(|error| {
        RuntimePolicyError::Materialization(format!(
            "inspect workload-owned path {}: {error}",
            path.display()
        ))
    })?;
    // CurrentProcess resolves to the actual creator, while an explicit Unix
    // policy was already checked against euid/egid before any mutation.
    let expected = match ownership.unix_ids() {
        Some(ids) => ids,
        None => {
            // SAFETY: geteuid/getegid take no pointers and have no preconditions.
            unsafe { (libc::geteuid(), libc::getegid()) }
        }
    };
    if (metadata.uid(), metadata.gid()) != expected {
        return Err(RuntimePolicyError::Materialization(format!(
            "workload-owned path {} has owner {}:{}, expected {}:{}",
            path.display(),
            metadata.uid(),
            metadata.gid(),
            expected.0,
            expected.1
        )));
    }
    let actual_mode = metadata.permissions().mode() & 0o777;
    if actual_mode != expected_mode {
        return Err(RuntimePolicyError::Materialization(format!(
            "workload-owned path {} has mode {actual_mode:04o}, expected {expected_mode:04o}",
            path.display()
        )));
    }
    Ok(())
}

fn remove_owned_tree(path: &Path) -> Result<(), RuntimePolicyError> {
    match fs::remove_dir_all(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(RuntimePolicyError::Compensation(format!(
            "remove {}: {error}",
            path.display()
        ))),
    }
}

fn path_text(path: &Path) -> Result<String, RuntimePolicyError> {
    path.to_str().map(str::to_string).ok_or_else(|| {
        RuntimePolicyError::Materialization(format!(
            "Agent-local path {} is not valid UTF-8",
            path.display()
        ))
    })
}
