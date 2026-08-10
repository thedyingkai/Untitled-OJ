use async_trait::async_trait;
use orchestrator_runtime::{
    ContainerRuntime, ContainerSpec, DeploymentRuntimeObservationV1, DockerRuntimeFacts,
    JUDGE_SANDBOX_V1_PROFILE_SHA256, MANAGED_EVENT_CONNECTION_FILE,
    MANAGED_SERVICE_CREDENTIAL_FILE, MANAGED_SERVICE_GATEWAY_CA_FILE, ManagedApiBinding,
    ManagedEventBinding, ManagedEventSubscription, ManagedServiceContextSpec, OciImageReference,
    RuntimeContext, RuntimeContract, RuntimeProfile, WorkloadCredential,
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
            if let Err(error) = runtime.remove_managed_volume(&volume).await {
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
        })
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
        create_private_directory(self.context_root()?)?;
        create_private_directory(&paths.context_directory)?;
        if context.contract.id == RuntimeProfile::JudgeSandboxV1 {
            create_private_directory(Path::new(&context.scratch_directory))?;
        }
        materialize_service_context(spec, context, credential, &self.event_connections)?;
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
        create_private_directory(self.context_root()?)?;
        create_private_directory(Path::new(&context.service_context_directory))?;
        materialize_service_context_fields(
            &spec.deployment_id,
            &spec.service_id,
            managed,
            context,
            None,
            &self.event_connections,
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
    )
}

fn materialize_service_context_fields(
    deployment_id: &str,
    service_id: &str,
    managed: &ManagedServiceContextSpec,
    context: &RuntimeContext,
    credential: Option<&WorkloadCredential>,
    event_connections: &BTreeMap<String, String>,
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
    create_private_directory(directory)?;

    let credential_path = directory.join("token");
    let gateway_ca_path = directory.join("ca.pem");
    let event_context_path = directory.join("events.json");
    let event_connection_path = directory.join("event-redis.url");
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
    let apply = (|| {
        match managed.gateway_ca_pem.as_deref() {
            Some(pem) => atomic_private_write(&gateway_ca_path, pem.as_bytes())?,
            None => remove_file_if_present(&gateway_ca_path)?,
        }
        atomic_private_write(&context_path, &bytes)?;
        match event_materialization.as_ref() {
            Some((connection, document)) => {
                atomic_private_write(&event_connection_path, connection)?;
                atomic_private_write(&event_context_path, document)?;
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
        ] {
            let restored = match previous {
                Some(bytes) => atomic_private_write(path, bytes),
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

fn atomic_private_write(path: &Path, bytes: &[u8]) -> Result<(), RuntimePolicyError> {
    let parent = path.parent().ok_or_else(|| {
        RuntimePolicyError::Materialization(format!(
            "managed file {} has no parent directory",
            path.display()
        ))
    })?;
    create_private_directory(parent)?;
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
    }
    temporary.persist(path).map_err(|error| {
        RuntimePolicyError::Materialization(format!(
            "atomically replace {}: {}",
            path.display(),
            error.error
        ))
    })?;
    #[cfg(unix)]
    fs::File::open(parent)
        .and_then(|directory| directory.sync_all())
        .map_err(|error| {
            RuntimePolicyError::Materialization(format!(
                "sync managed directory {}: {error}",
                parent.display()
            ))
        })?;
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

fn create_private_directory(path: &Path) -> Result<(), RuntimePolicyError> {
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

#[cfg(test)]
mod tests {
    use super::*;
    use orchestrator_runtime::{
        ContainerSpec, ManagedApiBinding, ManagedServiceContextSpec, ManagedVolumeSpec,
        OciImageReference, RuntimeError, RuntimeInstance,
    };
    use std::collections::HashMap;
    use std::sync::Mutex as StdMutex;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tempfile::TempDir;

    const DIGEST: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

    #[derive(Default)]
    struct MockCredentialExchanger {
        calls: AtomicUsize,
        initial_calls: AtomicUsize,
        refresh_calls: AtomicUsize,
    }

    #[derive(Default)]
    struct RecoveryVolumeRuntime {
        removed: StdMutex<Vec<String>>,
    }

    #[async_trait]
    impl ContainerRuntime for RecoveryVolumeRuntime {
        async fn remove_managed_volume(
            &self,
            spec: &ManagedVolumeSpec,
        ) -> Result<(), RuntimeError> {
            spec.validate()?;
            self.removed.lock().unwrap().push(spec.name.clone());
            Ok(())
        }

        async fn pull_image(&self, _image: &OciImageReference) -> Result<(), RuntimeError> {
            unreachable!("recovery touches only the persisted managed volume")
        }

        async fn create_container(
            &self,
            _spec: &ContainerSpec,
        ) -> Result<RuntimeInstance, RuntimeError> {
            unreachable!("recovery touches only the persisted managed volume")
        }

        async fn start_container(&self, _container_id: &str) -> Result<(), RuntimeError> {
            unreachable!("recovery touches only the persisted managed volume")
        }

        async fn stop_container(
            &self,
            _container_id: &str,
            _timeout_seconds: i32,
        ) -> Result<(), RuntimeError> {
            unreachable!("recovery touches only the persisted managed volume")
        }

        async fn restart_container(
            &self,
            _container_id: &str,
            _timeout_seconds: i32,
        ) -> Result<(), RuntimeError> {
            unreachable!("recovery touches only the persisted managed volume")
        }

        async fn remove_container(
            &self,
            _container_id: &str,
            _force: bool,
        ) -> Result<(), RuntimeError> {
            unreachable!("recovery touches only the persisted managed volume")
        }

        async fn inspect_container(
            &self,
            _container_id: &str,
        ) -> Result<RuntimeInstance, RuntimeError> {
            unreachable!("recovery touches only the persisted managed volume")
        }
    }

    #[async_trait]
    impl WorkloadCredentialExchanger for MockCredentialExchanger {
        async fn exchange_workload_credential(
            &self,
            request: WorkloadCredentialExchangeRequest<'_>,
        ) -> Result<WorkloadCredential, RuntimePolicyError> {
            assert!(!request.deployment_id.is_empty());
            match (request.job_id, request.lease_token) {
                (Some(job_id), Some(lease_token)) => {
                    assert!(!job_id.is_empty());
                    assert!(!lease_token.is_empty());
                    self.initial_calls.fetch_add(1, Ordering::SeqCst);
                }
                (None, None) => {
                    self.refresh_calls.fetch_add(1, Ordering::SeqCst);
                }
                _ => panic!("job_id and lease_token must be both present or both absent"),
            }
            let call = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
            Ok(WorkloadCredential {
                access_token: format!("refreshed-token-{call}"),
                expires_at_ms: crate::now_ms() + 15 * 60_000,
            })
        }
    }

    fn supported_facts() -> DockerRuntimeFacts {
        DockerRuntimeFacts {
            engine: "docker".to_string(),
            server_version: "28.0.1".to_string(),
            operating_system: "Linux".to_string(),
            os_type: "linux".to_string(),
            architecture: "x86_64".to_string(),
            cgroup_version: "2".to_string(),
            memory_limit: true,
            pids_limit: true,
            rootless: false,
            apparmor: true,
            seccomp: true,
            security_options: vec![
                "name=apparmor".to_string(),
                "name=seccomp,profile=builtin".to_string(),
            ],
        }
    }

    fn write_policy(root: &TempDir) -> PathBuf {
        let context_root = root.path().join("contexts");
        let policy = serde_json::json!({
            "schema_version": 1,
            "allowed_profiles": ["standard-container-v1", "judge-sandbox-v1"],
            "service_context_root": context_root,
            "judge_sandbox": {
                "profile_sha256": JUDGE_SANDBOX_V1_PROFILE_SHA256,
                "context_root": context_root,
                "allowed_images": [format!("ghcr.io/acme/judge-worker@sha256:{DIGEST}")],
            }
        });
        let path = root.path().join("runtime-policy.json");
        fs::write(&path, serde_json::to_vec(&policy).unwrap()).unwrap();
        path
    }

    fn judge_spec() -> ContainerSpec {
        ContainerSpec {
            deployment_id: "deployment/with unsafe-looking input".to_string(),
            service_id: "judge-worker".to_string(),
            generation: 1,
            image: OciImageReference::parse(&format!("ghcr.io/acme/judge-worker@sha256:{DIGEST}"))
                .unwrap(),
            runtime_contract: RuntimeContract::judge_sandbox_v1(),
            runtime_context: None,
            managed_service_context: Some(ManagedServiceContextSpec {
                generation: 3,
                node_id: "node-1".to_string(),
                gateway_origin: "https://gateway.internal".to_string(),
                gateway_ca_pem: Some("fixture-ca".to_string()),
                bindings: BTreeMap::from([(
                    "storage_get".to_string(),
                    ManagedApiBinding {
                        binding_id: "binding-1".to_string(),
                        api_id: "storage.object.get".to_string(),
                        timeout_ms: 300_000,
                        context_generation: 3,
                    },
                )]),
                events: None,
            }),
            command: Vec::new(),
            environment: Vec::new(),
            labels: HashMap::from([
                (
                    "ojos.catalog_signature_verified".to_string(),
                    "true".to_string(),
                ),
                ("ojos.service_contract_version".to_string(), "2".to_string()),
                (
                    "ojos.release_checksum".to_string(),
                    format!("sha256:{DIGEST}"),
                ),
            ]),
            published_endpoint: None,
        }
    }

    fn standard_managed_spec() -> ContainerSpec {
        let mut spec = judge_spec();
        spec.service_id = "problem-service".to_string();
        spec.runtime_contract = RuntimeContract::standard_v1();
        spec
    }

    fn event_only_managed_spec() -> ContainerSpec {
        let mut spec = standard_managed_spec();
        let managed = spec.managed_service_context.as_mut().unwrap();
        managed.generation = 4;
        managed.gateway_ca_pem = None;
        managed.bindings.clear();
        managed.events = Some(ManagedEventBinding {
            connection_id: "shared-events".to_string(),
            stream: "ojos:events:v1".to_string(),
            publish_types: vec!["io.example.fixture.v1".to_string()],
            subscriptions: vec![],
            generation: 4,
        });
        spec
    }

    #[tokio::test]
    async fn restart_recovery_removes_ambiguous_owned_volume_once_before_context_tree() {
        let root = TempDir::new().unwrap();
        let provider =
            LocalRuntimeContextProvider::from_json_file(&write_policy(&root), supported_facts())
                .unwrap();
        let mut spec = judge_spec();
        let context = provider.plan_context(&spec).unwrap().unwrap();
        spec.runtime_context = Some(context.clone());
        let volume = spec.managed_volume_spec().unwrap().unwrap();
        let mut ledger = crate::AgentLedger::open_in_memory().unwrap();
        ledger
            .begin(
                "job-volume-crash",
                &orchestrator_control_plane::JobKind::Install,
                "payload-hash",
                "lease-token",
                1,
            )
            .unwrap();
        ledger
            .begin_runtime_context("job-volume-crash", &spec.deployment_id, &context, 2)
            .unwrap();
        ledger
            .begin_managed_volume(&spec.deployment_id, "job-volume-crash", &volume, 3)
            .unwrap();
        ledger.recover_interrupted(4).unwrap();
        let runtime = RecoveryVolumeRuntime::default();

        assert_eq!(
            recover_pending_runtime_contexts(&mut ledger, &provider, &runtime)
                .await
                .unwrap(),
            1
        );
        assert_eq!(runtime.removed.lock().unwrap().as_slice(), [volume.name]);
        let cleaned = ledger
            .runtime_context_for_deployment(&spec.deployment_id)
            .unwrap()
            .unwrap();
        assert_eq!(cleaned.state, "CLEANED");
        assert_eq!(cleaned.managed_volume_state, "CLEANED");
        assert!(!cleaned.managed_volume_owned);

        assert_eq!(
            recover_pending_runtime_contexts(&mut ledger, &provider, &runtime)
                .await
                .unwrap(),
            0
        );
        assert_eq!(runtime.removed.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn materializes_only_deterministic_agent_owned_paths_and_compensates() {
        let root = TempDir::new().unwrap();
        let provider =
            LocalRuntimeContextProvider::from_json_file(&write_policy(&root), supported_facts())
                .unwrap();
        let spec = judge_spec();
        let context = provider.plan_context(&spec).unwrap().unwrap();
        let credential = WorkloadCredential {
            access_token: "first-token".to_string(),
            expires_at_ms: i64::MAX,
        };
        provider
            .materialize_context(&spec, &context, &credential)
            .await
            .unwrap();
        assert!(Path::new(&context.scratch_directory).is_dir());
        assert!(!context.scratch_directory.contains("unsafe-looking"));
        assert_eq!(
            context.cache_volume_name.len(),
            "ojos-judge-cache-".len() + 32
        );
        let context_json =
            fs::read_to_string(Path::new(&context.service_context_directory).join("context.json"))
                .unwrap();
        assert!(context_json.contains(MANAGED_SERVICE_CREDENTIAL_FILE));
        assert!(!context_json.contains("first-token"));
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&context_json).unwrap()["generation"],
            3
        );
        assert_ne!(
            serde_json::from_str::<serde_json::Value>(&context_json).unwrap()["generation"],
            spec.generation
        );
        assert_eq!(
            fs::read_to_string(Path::new(&context.service_context_directory).join("token"))
                .unwrap(),
            "first-token"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(Path::new(&context.service_context_directory))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o700
            );
            for file in ["context.json", "token", "ca.pem"] {
                assert_eq!(
                    fs::metadata(Path::new(&context.service_context_directory).join(file))
                        .unwrap()
                        .permissions()
                        .mode()
                        & 0o777,
                    0o600
                );
            }
        }
        provider
            .rotate_workload_credential(
                &context,
                &WorkloadCredential {
                    access_token: "second-token".to_string(),
                    expires_at_ms: i64::MAX,
                },
            )
            .await
            .unwrap();
        assert_eq!(
            fs::read_to_string(Path::new(&context.service_context_directory).join("token"))
                .unwrap(),
            "second-token"
        );
        assert_eq!(
            provider.runtime_facts().allowed_contracts,
            vec![
                RuntimeContract::standard_v1(),
                RuntimeContract::judge_sandbox_v1()
            ]
        );
        provider.compensate(&context).await.unwrap();
        assert!(!Path::new(&context.scratch_directory).exists());
    }

    #[tokio::test]
    async fn standard_managed_context_materializes_and_rotates_token_without_sandbox_paths() {
        let root = TempDir::new().unwrap();
        let provider: Arc<dyn RuntimeContextProvider> = Arc::new(
            LocalRuntimeContextProvider::from_json_file(&write_policy(&root), supported_facts())
                .unwrap(),
        );
        let spec = standard_managed_spec();
        let context = provider.plan_context(&spec).unwrap().unwrap();
        assert_eq!(context.contract, RuntimeContract::standard_v1());
        assert!(context.scratch_directory.is_empty());
        assert!(context.cache_volume_name.is_empty());

        let exchanger = Arc::new(MockCredentialExchanger::default());
        let supervisor =
            WorkloadCredentialSupervisor::new(exchanger.clone(), Arc::clone(&provider));
        let initial = supervisor
            .issue_initial(&spec.deployment_id, "job-standard", "lease-standard")
            .await
            .unwrap();
        assert_eq!(exchanger.initial_calls.load(Ordering::SeqCst), 1);
        assert_eq!(exchanger.refresh_calls.load(Ordering::SeqCst), 0);
        provider
            .materialize_context(&spec, &context, &initial)
            .await
            .unwrap();
        let service_directory = Path::new(&context.service_context_directory);
        assert_eq!(
            fs::read_to_string(service_directory.join("token")).unwrap(),
            "refreshed-token-1"
        );
        assert!(!service_directory.parent().unwrap().join("work").exists());

        provider
            .rotate_workload_credential(
                &context,
                &WorkloadCredential {
                    access_token: "standard-token-2".to_string(),
                    expires_at_ms: i64::MAX,
                },
            )
            .await
            .unwrap();
        assert_eq!(
            fs::read_to_string(service_directory.join("token")).unwrap(),
            "standard-token-2"
        );
        provider.compensate(&context).await.unwrap();
        assert!(!service_directory.exists());
    }

    #[tokio::test]
    async fn optional_unbound_standard_context_has_empty_token_and_can_bind_later() {
        let root = TempDir::new().unwrap();
        let provider =
            LocalRuntimeContextProvider::from_json_file(&write_policy(&root), supported_facts())
                .unwrap();
        let mut spec = standard_managed_spec();
        let mut bound = {
            let managed = spec.managed_service_context.as_mut().unwrap();
            managed.generation = 1;
            managed.bindings.clear();
            managed.clone()
        };
        let context = provider.plan_context(&spec).unwrap().unwrap();
        provider
            .materialize_unbound_context(&spec, &context)
            .await
            .unwrap();
        let service_directory = Path::new(&context.service_context_directory);
        let document: serde_json::Value =
            serde_json::from_slice(&fs::read(service_directory.join("context.json")).unwrap())
                .unwrap();
        assert_eq!(document["bindings"], serde_json::json!({}));
        assert_eq!(document["generation"], 1);
        assert_eq!(fs::read(service_directory.join("token")).unwrap(), b"");

        bound.generation = 2;
        bound.bindings.insert(
            "storage_get".to_string(),
            ManagedApiBinding {
                binding_id: "binding-first".to_string(),
                api_id: "storage.object.get".to_string(),
                timeout_ms: 300_000,
                context_generation: 2,
            },
        );
        provider
            .reconfigure_context(
                &spec.deployment_id,
                &spec.service_id,
                &bound,
                &context,
                &WorkloadCredential {
                    access_token: "first-bound-token".to_string(),
                    expires_at_ms: i64::MAX,
                },
            )
            .await
            .unwrap();
        assert_eq!(
            fs::read_to_string(service_directory.join("token")).unwrap(),
            "first-bound-token"
        );
        let rebound: serde_json::Value =
            serde_json::from_slice(&fs::read(service_directory.join("context.json")).unwrap())
                .unwrap();
        assert_eq!(rebound["generation"], 2);
        assert!(rebound["bindings"]["storage_get"].is_object());
    }

    #[tokio::test]
    async fn event_only_context_is_atomic_secret_free_in_ledger_and_restart_idempotent() {
        let root = TempDir::new().unwrap();
        let policy = write_policy(&root);
        let local_connections = BTreeMap::from([(
            "shared-events".to_string(),
            "redis://event-user:event-secret@127.0.0.1:6379/4".to_string(),
        )]);
        let provider = LocalRuntimeContextProvider::from_json_file(&policy, supported_facts())
            .unwrap()
            .with_event_connections(local_connections.clone());
        let spec = event_only_managed_spec();
        let context = provider.plan_context(&spec).unwrap().unwrap();
        provider
            .materialize_unbound_context(&spec, &context)
            .await
            .unwrap();

        let directory = Path::new(&context.service_context_directory);
        let context_bytes = fs::read(directory.join("context.json")).unwrap();
        let event_bytes = fs::read(directory.join("events.json")).unwrap();
        let connection_bytes = fs::read(directory.join("event-redis.url")).unwrap();
        let token_bytes = fs::read(directory.join("token")).unwrap();
        let event_document: serde_json::Value = serde_json::from_slice(&event_bytes).unwrap();
        assert_eq!(event_document["connection_id"], "shared-events");
        assert_eq!(event_document["stream"], "ojos:events:v1");
        assert_eq!(event_document["generation"], 4);
        assert_eq!(
            event_document["connection_file"],
            MANAGED_EVENT_CONNECTION_FILE
        );
        let connection_text =
            std::str::from_utf8(&connection_bytes).expect("event connection must be UTF-8");
        let event_text = std::str::from_utf8(&event_bytes).expect("event context must be UTF-8");
        assert!(connection_text.contains("event-secret"));
        assert!(!event_text.contains("event-secret"));
        assert!(
            !serde_json::to_string(&spec)
                .unwrap()
                .contains("event-secret")
        );
        assert!(
            token_bytes.is_empty(),
            "event-only context needs no workload JWT"
        );
        assert_eq!(
            provider.runtime_facts().redis_connection_ids,
            vec!["shared-events".to_string()]
        );

        let mut ledger = crate::AgentLedger::open_in_memory().unwrap();
        ledger
            .begin(
                "job-event-context",
                &orchestrator_control_plane::JobKind::Install,
                "payload-hash",
                "lease",
                1,
            )
            .unwrap();
        ledger
            .begin_runtime_context("job-event-context", &spec.deployment_id, &context, 2)
            .unwrap();
        ledger
            .mark_runtime_context_prepared(&spec.deployment_id, "job-event-context", 3)
            .unwrap();
        ledger
            .mark_runtime_context_creating(&spec.deployment_id, "job-event-context", 4)
            .unwrap();
        ledger
            .bind_runtime_context(
                &spec.deployment_id,
                "job-event-context",
                "container-event-context",
                5,
            )
            .unwrap();
        ledger
            .activate_runtime_context(&spec.deployment_id, "job-event-context", 6)
            .unwrap();
        ledger
            .record_binding_context_transition(
                &spec.deployment_id,
                "job-event-context",
                None,
                spec.managed_service_context.as_ref(),
                false,
                7,
            )
            .unwrap();
        let persisted = ledger
            .runtime_context_for_deployment(&spec.deployment_id)
            .unwrap()
            .unwrap()
            .managed_context
            .unwrap();
        let persisted = serde_json::to_string(&persisted).unwrap();
        assert!(persisted.contains("shared-events"));
        assert!(!persisted.contains("event-secret"));
        assert!(!persisted.contains("redis://"));

        // Resolution failures happen before any generation is committed. The
        // currently readable context therefore remains byte-for-byte intact.
        let mut invalid = spec.managed_service_context.clone().unwrap();
        invalid.generation = 5;
        invalid.events.as_mut().unwrap().generation = 5;
        invalid.events.as_mut().unwrap().connection_id = "missing-events".to_string();
        let error = provider
            .reconfigure_context(
                &spec.deployment_id,
                &spec.service_id,
                &invalid,
                &context,
                &WorkloadCredential {
                    access_token: "unused-event-only-token".to_string(),
                    expires_at_ms: i64::MAX,
                },
            )
            .await
            .unwrap_err();
        assert!(error.to_string().contains("missing-events"));
        assert_eq!(
            fs::read(directory.join("context.json")).unwrap(),
            context_bytes
        );
        assert_eq!(
            fs::read(directory.join("events.json")).unwrap(),
            event_bytes
        );
        assert_eq!(
            fs::read(directory.join("event-redis.url")).unwrap(),
            connection_bytes
        );
        assert_eq!(fs::read(directory.join("token")).unwrap(), token_bytes);

        // A restarted Agent reconstructs the provider from protected local
        // configuration and can replay the same credential-free Job spec.
        let restarted = LocalRuntimeContextProvider::from_json_file(&policy, supported_facts())
            .unwrap()
            .with_event_connections(local_connections);
        restarted
            .materialize_unbound_context(&spec, &context)
            .await
            .unwrap();
        assert_eq!(
            fs::read(directory.join("events.json")).unwrap(),
            event_bytes
        );
        assert_eq!(
            fs::read(directory.join("event-redis.url")).unwrap(),
            connection_bytes
        );
    }

    #[tokio::test]
    async fn standard_managed_context_supervisor_refreshes_token_without_agent_restart() {
        let root = TempDir::new().unwrap();
        let provider: Arc<dyn RuntimeContextProvider> = Arc::new(
            LocalRuntimeContextProvider::from_json_file(&write_policy(&root), supported_facts())
                .unwrap(),
        );
        let spec = standard_managed_spec();
        let context = provider.plan_context(&spec).unwrap().unwrap();
        provider
            .materialize_context(
                &spec,
                &context,
                &WorkloadCredential {
                    access_token: "initial-standard-token".to_string(),
                    expires_at_ms: crate::now_ms() + 15 * 60_000,
                },
            )
            .await
            .unwrap();
        let exchanger = Arc::new(MockCredentialExchanger::default());
        let supervisor = WorkloadCredentialSupervisor::new(exchanger.clone(), provider);
        // At exactly five minutes remaining the refresh is immediately due.
        supervisor
            .start_refresh(
                &spec.deployment_id,
                context.clone(),
                crate::now_ms() + 5 * 60_000,
            )
            .await
            .unwrap();
        let token_path = Path::new(&context.service_context_directory).join("token");
        for _ in 0..100 {
            if fs::read_to_string(&token_path).unwrap() == "refreshed-token-1" {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        assert_eq!(fs::read_to_string(token_path).unwrap(), "refreshed-token-1");
        assert_eq!(exchanger.initial_calls.load(Ordering::SeqCst), 0);
        assert_eq!(exchanger.refresh_calls.load(Ordering::SeqCst), 1);
        supervisor.shutdown_all().await;
    }

    #[test]
    fn rejects_rootless_runtime_and_unknown_policy_fields() {
        let root = TempDir::new().unwrap();
        let path = write_policy(&root);
        let mut facts = supported_facts();
        facts.rootless = true;
        assert!(matches!(
            LocalRuntimeContextProvider::from_json_file(&path, facts),
            Err(RuntimePolicyError::UnsupportedRuntime { .. })
        ));

        let invalid = serde_json::json!({
            "schema_version": 1,
            "allowed_profiles": ["standard-container-v1", "judge-sandbox-v1"],
            "judge_sandbox": {
                "context_root": root.path().join("contexts"),
                "arbitrary_host_path": "/etc",
            }
        });
        fs::write(&path, serde_json::to_vec(&invalid).unwrap()).unwrap();
        assert!(matches!(
            LocalRuntimeContextProvider::from_json_file(&path, supported_facts()),
            Err(RuntimePolicyError::InvalidPolicy(_))
        ));
    }

    #[test]
    fn judge_profile_accepts_no_apparmor_lsm_with_explicit_unconfined_contract() {
        let root = TempDir::new().unwrap();
        let path = write_policy(&root);
        let mut facts = supported_facts();
        facts.apparmor = false;
        facts
            .security_options
            .retain(|option| !option.contains("apparmor"));

        let provider = LocalRuntimeContextProvider::from_json_file(&path, facts)
            .expect("explicit apparmor=unconfined does not require a host AppArmor LSM");
        let reported = provider.runtime_facts();
        assert!(!reported.docker.apparmor);
        assert!(provider.plan_context(&judge_spec()).unwrap().is_some());
    }

    #[test]
    fn policy_decode_rejects_non_utf8_and_oversized_input_without_echoing_bytes() {
        let invalid_utf8 = [b'{', b'"', 0xff, 0xfe, b'"', b'}'];
        let error = decode_runtime_policy(&invalid_utf8)
            .expect_err("runtime policy bytes must use strict UTF-8")
            .to_string();
        assert_eq!(
            error,
            "invalid Agent runtime policy: policy contains binary/invalid UTF-8 input (6 bytes; first invalid byte 2)"
        );
        assert!(!error.contains('\u{fffd}'));
        assert!(error.len() <= 128);

        let oversized = vec![b' '; MAX_RUNTIME_POLICY_BYTES as usize + 1];
        let error = decode_runtime_policy(&oversized)
            .expect_err("runtime policy decoding must enforce its byte ceiling")
            .to_string();
        assert_eq!(
            error,
            "invalid Agent runtime policy: policy must be a non-empty regular JSON file no larger than 64 KiB"
        );
        assert!(error.len() <= 128);
    }

    #[tokio::test]
    async fn standard_only_provider_rejects_judge_contract_without_side_effects() {
        let provider = LocalRuntimeContextProvider::standard_only(
            supported_facts(),
            std::env::temp_dir().join("ojos-standard-only-contexts"),
        );
        assert!(matches!(
            provider.plan_context(&judge_spec()),
            Err(RuntimePolicyError::ProfileNotAllowed(
                RuntimeProfile::JudgeSandboxV1
            ))
        ));
    }

    #[test]
    fn judge_profile_requires_exact_local_artifact_authorization_and_store_proof() {
        let root = TempDir::new().unwrap();
        let provider =
            LocalRuntimeContextProvider::from_json_file(&write_policy(&root), supported_facts())
                .unwrap();
        let mut wrong_image = judge_spec();
        wrong_image.image = OciImageReference::parse(&format!(
            "ghcr.io/acme/judge-worker@sha256:{}",
            "b".repeat(64)
        ))
        .unwrap();
        assert!(
            provider
                .plan_context(&wrong_image)
                .unwrap_err()
                .to_string()
                .contains("not explicitly authorized")
        );

        let mut unsigned = judge_spec();
        unsigned.labels.remove("ojos.catalog_signature_verified");
        assert!(
            provider
                .plan_context(&unsigned)
                .unwrap_err()
                .to_string()
                .contains("signature-verified Store v2")
        );
        assert_eq!(
            provider.runtime_facts().judge_sandbox_allowed_images,
            vec![format!("ghcr.io/acme/judge-worker@sha256:{DIGEST}")]
        );
    }

    #[test]
    fn credential_refresh_is_scheduled_at_five_minutes_remaining() {
        assert_eq!(refresh_delay_ms(1_000_000, 100_000), 600_000);
        assert_eq!(refresh_delay_ms(350_000, 100_000), 0);
        assert_eq!(refresh_delay_ms(50_000, 100_000), 0);
    }

    #[tokio::test]
    async fn standard_managed_context_recovers_active_ledger_without_persisted_token() {
        let root = TempDir::new().unwrap();
        let provider: Arc<dyn RuntimeContextProvider> = Arc::new(
            LocalRuntimeContextProvider::from_json_file(&write_policy(&root), supported_facts())
                .unwrap(),
        );
        let spec = standard_managed_spec();
        let context = provider.plan_context(&spec).unwrap().unwrap();
        provider
            .materialize_context(
                &spec,
                &context,
                &WorkloadCredential {
                    access_token: "expired-process-token".to_string(),
                    expires_at_ms: i64::MAX,
                },
            )
            .await
            .unwrap();
        let mut ledger = crate::AgentLedger::open_in_memory().unwrap();
        ledger
            .begin(
                "job-context",
                &orchestrator_control_plane::JobKind::Install,
                "hash",
                "lease",
                10,
            )
            .unwrap();
        ledger
            .begin_runtime_context("job-context", &spec.deployment_id, &context, 11)
            .unwrap();
        ledger
            .mark_runtime_context_prepared(&spec.deployment_id, "job-context", 12)
            .unwrap();
        ledger
            .mark_runtime_context_creating(&spec.deployment_id, "job-context", 13)
            .unwrap();
        ledger
            .bind_runtime_context(&spec.deployment_id, "job-context", "container-1", 14)
            .unwrap();
        ledger
            .activate_runtime_context(&spec.deployment_id, "job-context", 15)
            .unwrap();
        ledger
            .record_binding_context_transition(
                &spec.deployment_id,
                "job-context",
                None,
                spec.managed_service_context.as_ref(),
                false,
                16,
            )
            .unwrap();
        ledger
            .finish(
                "job-context",
                &crate::StoredCompletion {
                    status: orchestrator_control_plane::CompletionStatus::Succeeded,
                    result: serde_json::json!({"installed": true}),
                    error_message: String::new(),
                    events: vec![],
                },
                17,
            )
            .unwrap();

        let exchanger = Arc::new(MockCredentialExchanger::default());
        let supervisor = WorkloadCredentialSupervisor::new(exchanger.clone(), provider);
        assert_eq!(supervisor.recover_active(&ledger).await.unwrap(), 1);
        assert_eq!(exchanger.calls.load(Ordering::SeqCst), 1);
        assert_eq!(exchanger.initial_calls.load(Ordering::SeqCst), 0);
        assert_eq!(exchanger.refresh_calls.load(Ordering::SeqCst), 1);
        assert_eq!(
            fs::read_to_string(Path::new(&context.service_context_directory).join("token"))
                .unwrap(),
            "refreshed-token-1"
        );
        supervisor.shutdown_all().await;
    }

    #[tokio::test]
    async fn restart_recovery_skips_active_contexts_without_api_bindings() {
        let root = TempDir::new().unwrap();
        let provider: Arc<dyn RuntimeContextProvider> = Arc::new(
            LocalRuntimeContextProvider::from_json_file(&write_policy(&root), supported_facts())
                .unwrap(),
        );
        let spec = event_only_managed_spec();
        let context = provider.plan_context(&spec).unwrap().unwrap();
        let mut ledger = crate::AgentLedger::open_in_memory().unwrap();
        ledger
            .begin(
                "job-event-only-context",
                &orchestrator_control_plane::JobKind::Install,
                "hash",
                "lease",
                10,
            )
            .unwrap();
        ledger
            .begin_runtime_context("job-event-only-context", &spec.deployment_id, &context, 11)
            .unwrap();
        ledger
            .mark_runtime_context_prepared(&spec.deployment_id, "job-event-only-context", 12)
            .unwrap();
        ledger
            .mark_runtime_context_creating(&spec.deployment_id, "job-event-only-context", 13)
            .unwrap();
        ledger
            .bind_runtime_context(
                &spec.deployment_id,
                "job-event-only-context",
                "container-event-only",
                14,
            )
            .unwrap();
        ledger
            .activate_runtime_context(&spec.deployment_id, "job-event-only-context", 15)
            .unwrap();
        ledger
            .record_binding_context_transition(
                &spec.deployment_id,
                "job-event-only-context",
                None,
                spec.managed_service_context.as_ref(),
                false,
                16,
            )
            .unwrap();

        let exchanger = Arc::new(MockCredentialExchanger::default());
        let supervisor = WorkloadCredentialSupervisor::new(exchanger.clone(), provider);
        assert_eq!(supervisor.recover_active(&ledger).await.unwrap(), 0);
        assert_eq!(exchanger.calls.load(Ordering::SeqCst), 0);
        assert!(supervisor.status().await.is_empty());
    }
}
