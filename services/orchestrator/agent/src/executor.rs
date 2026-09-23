use crate::resource_claim::{
    ResourceClaimFailureCodeV1, ResourceClaimPipelineExecutor, ResourceClaimPipelineHandle,
    ResourceClaimStatusV1,
};
use crate::{
    AgentLedger, ArtifactFetcher, HttpReleasePipelineProvider, LeasedJob, LedgerError,
    MigrationDecision, PipelineProviderError, ReleasePipelineProvider, RuntimeContextProvider,
    RuntimePolicyError, WorkloadCredentialSupervisor,
};
use orchestrator_control_plane::{CompletionStatus, JobKind, NewJobEvent};
use orchestrator_runtime::{
    BindingContextApplyPayload, ContainerRuntime, ContainerSpec, HealthGateDecision,
    HealthGatePolicy, MIGRATION_CHECKSUM_LABEL, MIGRATION_IDENTITY_LABEL, MIGRATION_JOB_ID_LABEL,
    MIGRATION_MANAGED_BY, MIGRATION_MANAGED_BY_LABEL, MIGRATION_RESOURCE_CLAIMS_LABEL,
    MIGRATION_RUNTIME_ROLE, MIGRATION_RUNTIME_ROLE_LABEL, MIGRATION_SERVICE_LABEL,
    MIGRATION_VERSION_LABEL, OciMigrationStep, ReleasePipelinePayload, ReleaseProviderRevision,
    ReleaseReplacementPayload, ReplacementProviderSaga, ResourcePurgePayloadV1,
    ResourceSecretFileMount, RuntimeContext, RuntimeError, RuntimeInstallPayload, RuntimeInstance,
    RuntimeObservedState, RuntimeProfile, RuntimeReplacement, TypedProvisionerStep,
    WorkloadCredential, evaluate_health_gate, migration_identity_sha256,
    migration_resource_claims_sha256,
};
use serde::Deserialize;
use serde::Serialize;
use serde_json::{Value, json};
use std::collections::BTreeSet;
use std::future::Future;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::watch;
use tokio::time::Instant;

#[derive(Debug, Clone, PartialEq)]
pub struct ExecutionOutcome {
    pub status: CompletionStatus,
    pub result: Value,
    pub error_message: String,
    pub events: Vec<NewJobEvent>,
}

impl ExecutionOutcome {
    fn success(result: Value) -> Self {
        Self {
            status: CompletionStatus::Succeeded,
            result,
            error_message: String::new(),
            events: vec![],
        }
    }

    fn success_with_events(result: Value, events: Vec<NewJobEvent>) -> Self {
        Self {
            status: CompletionStatus::Succeeded,
            result,
            error_message: String::new(),
            events,
        }
    }

    fn failed(message: impl Into<String>) -> Self {
        Self {
            status: CompletionStatus::Failed,
            result: Value::Null,
            error_message: message.into(),
            events: vec![],
        }
    }
}

pub struct JobExecutor<R> {
    runtime: Arc<R>,
    pipeline_provider: Arc<dyn ReleasePipelineProvider>,
    artifact_fetcher: Option<Arc<dyn ArtifactFetcher>>,
    runtime_context_provider: Option<Arc<dyn RuntimeContextProvider>>,
    workload_credentials: Option<Arc<WorkloadCredentialSupervisor>>,
    resource_claims: Option<ResourceClaimPipelineHandle>,
}

#[derive(Clone)]
struct MaterializedRuntimeContext {
    deployment_id: String,
    context: RuntimeContext,
    credential_expires_at_ms: i64,
    credential_active: bool,
}

/// Tracks the Agent-local deployment binding created by ResourceClaim ensure.
///
/// `release_pipeline` deliberately owns this guard outside the async pipeline
/// body so every non-success return, including a propagated LedgerError, crosses
/// the same RETAIN compensation boundary.  A successful pipeline disarms by
/// keeping the binding for the installed runtime; compensation only removes the
/// deployment binding and never purges the retained provider resource.
struct ReleasePipelineClaimGuard {
    manager: Option<ResourceClaimPipelineHandle>,
    deployment_id: String,
    armed: bool,
}

impl ReleasePipelineClaimGuard {
    fn new(manager: Option<ResourceClaimPipelineHandle>, deployment_id: String) -> Self {
        Self {
            manager,
            deployment_id,
            armed: false,
        }
    }

    /// Arm before calling `ensure`: if a local persistence error makes the
    /// bind result uncertain, idempotent release still probes the exact
    /// deployment binding and safely becomes a no-op when none was committed.
    fn arm(&mut self) {
        self.armed = true;
    }

    async fn release(&mut self) -> Result<(), &'static str> {
        if !self.armed {
            return Ok(());
        }
        let Some(manager) = self.manager.as_ref() else {
            return Err("the Agent-local ResourceClaim manager is unavailable");
        };
        let releases = manager
            .release_deployment(&self.deployment_id)
            .await
            .map_err(|_| "the ResourceClaim deployment release failed")?;
        let unsafe_state = releases.iter().any(|release| {
            if release.provider_released {
                release.claim.status != ResourceClaimStatusV1::Retained
            } else {
                release.claim.status != ResourceClaimStatusV1::Ready
            }
        });
        if unsafe_state {
            return Err("the ResourceClaim binding/provider state is not safely retained");
        }
        self.armed = false;
        Ok(())
    }

    async fn finish(
        &mut self,
        execution: Result<ExecutionOutcome, LedgerError>,
    ) -> Result<ExecutionOutcome, LedgerError> {
        match execution {
            Ok(outcome) if outcome.status == CompletionStatus::Succeeded => Ok(outcome),
            Ok(mut outcome) => {
                if let Err(reason) = self.release().await {
                    mark_resource_claim_compensation_unknown(&mut outcome, reason);
                }
                Ok(outcome)
            }
            Err(error) => match self.release().await {
                Ok(()) => Err(error),
                Err(reason) => {
                    let mut outcome = needs_attention_outcome(format!(
                        "release pipeline stopped on an Agent ledger error and ResourceClaim RETAIN compensation could not be proven: {reason}"
                    ));
                    mark_resource_claim_compensation_evidence(&mut outcome);
                    Ok(outcome)
                }
            },
        }
    }
}

impl<R> Clone for JobExecutor<R> {
    fn clone(&self) -> Self {
        Self {
            runtime: Arc::clone(&self.runtime),
            pipeline_provider: Arc::clone(&self.pipeline_provider),
            artifact_fetcher: self.artifact_fetcher.clone(),
            runtime_context_provider: self.runtime_context_provider.clone(),
            workload_credentials: self.workload_credentials.clone(),
            resource_claims: self.resource_claims.clone(),
        }
    }
}

impl<R> JobExecutor<R>
where
    R: ContainerRuntime,
{
    pub fn new(runtime: R) -> Self {
        Self {
            runtime: Arc::new(runtime),
            pipeline_provider: Arc::new(HttpReleasePipelineProvider::from_env()),
            artifact_fetcher: None,
            runtime_context_provider: None,
            workload_credentials: None,
            resource_claims: None,
        }
    }

    pub fn from_shared(runtime: Arc<R>) -> Self {
        Self {
            runtime,
            pipeline_provider: Arc::new(HttpReleasePipelineProvider::from_env()),
            artifact_fetcher: None,
            runtime_context_provider: None,
            workload_credentials: None,
            resource_claims: None,
        }
    }

    pub fn with_pipeline_provider(mut self, provider: Arc<dyn ReleasePipelineProvider>) -> Self {
        self.pipeline_provider = provider;
        self
    }

    pub fn with_artifact_fetcher(mut self, fetcher: Arc<dyn ArtifactFetcher>) -> Self {
        self.artifact_fetcher = Some(fetcher);
        self
    }

    pub fn with_runtime_context(
        mut self,
        provider: Arc<dyn RuntimeContextProvider>,
        credentials: Arc<WorkloadCredentialSupervisor>,
    ) -> Self {
        self.runtime_context_provider = Some(provider);
        self.workload_credentials = Some(credentials);
        self
    }

    pub fn with_resource_claims(
        mut self,
        resource_claims: Arc<dyn ResourceClaimPipelineExecutor>,
    ) -> Self {
        self.resource_claims = Some(ResourceClaimPipelineHandle::new(resource_claims));
        self
    }

    pub async fn execute(
        &self,
        job: &LeasedJob,
        ledger: &mut AgentLedger,
    ) -> Result<ExecutionOutcome, LedgerError> {
        let (_cancel_guard, cancellation) = watch::channel(false);
        self.execute_with_cancellation(job, ledger, cancellation)
            .await
    }

    pub async fn execute_with_cancellation(
        &self,
        job: &LeasedJob,
        ledger: &mut AgentLedger,
        cancellation: watch::Receiver<bool>,
    ) -> Result<ExecutionOutcome, LedgerError> {
        match job.kind {
            JobKind::Install => self.install(job, ledger, cancellation).await,
            JobKind::ReleasePipeline => {
                let deployment_id = job
                    .payload
                    .get("install")
                    .and_then(|install| install.get("spec"))
                    .and_then(|spec| spec.get("deployment_id"))
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                let mut claim_guard =
                    ReleasePipelineClaimGuard::new(self.resource_claims.clone(), deployment_id);
                let execution = self
                    .release_pipeline(job, ledger, cancellation, &mut claim_guard)
                    .await;
                claim_guard.finish(execution).await
            }
            JobKind::Upgrade => {
                self.replace_release(job, ledger, cancellation, "upgrade")
                    .await
            }
            JobKind::Start => self.start(job, ledger).await,
            JobKind::Stop => self.stop(job, ledger).await,
            JobKind::Restart => self.restart(job, ledger).await,
            JobKind::Uninstall => self.uninstall(job, ledger).await,
            JobKind::Rollback => {
                self.replace_release(job, ledger, cancellation, "rollback")
                    .await
            }
            JobKind::Health => self.health(job, ledger).await,
            JobKind::BindingContextApply => self.binding_context_apply(job, ledger).await,
            JobKind::ResourcePurge => self.resource_purge(job).await,
            JobKind::Inventory => Ok(ExecutionOutcome::failed(
                "inventory jobs are not part of the v1 mutation executor",
            )),
            JobKind::TopologyApply | JobKind::ContributionProjection | JobKind::ExternalHealth => {
                Ok(ExecutionOutcome::failed(
                    "control-plane-only jobs cannot run on a Node Agent",
                ))
            }
            JobKind::NodeDrain | JobKind::NodeRemove => Ok(ExecutionOutcome::failed(
                "Node lifecycle jobs are control-plane-only and cannot run on a Node Agent",
            )),
        }
    }

    async fn resource_purge(&self, job: &LeasedJob) -> Result<ExecutionOutcome, LedgerError> {
        let payload: ResourcePurgePayloadV1 = match decode_payload(job) {
            Ok(payload) => payload,
            Err(outcome) => return Ok(outcome),
        };
        if let Err(error) = payload.validate() {
            return Ok(ExecutionOutcome::failed(format!(
                "invalid resource purge payload: {error}"
            )));
        }
        let Some(manager) = &self.resource_claims else {
            return Ok(ExecutionOutcome::failed(
                "resource purge requires an Agent-local resource provider",
            ));
        };
        let claim = match manager.purge(&payload).await {
            Ok(claim) => claim,
            Err(crate::resource_claim::ResourceClaimError::ExecutionOutcomeUnknown) => {
                return Ok(needs_attention_outcome(
                    "resource purge ended without a proven provider outcome",
                ));
            }
            Err(error) => {
                return Ok(ExecutionOutcome::failed(format!(
                    "resource purge rejected before completion: {error}"
                )));
            }
        };
        let result = json!({
            "schema_version": "ojos.dev/resource-purge-result/v1",
            "claim_id": claim.identity.claim_id,
            "claim_digest": claim.claim_digest,
            "generation": claim.generation,
            "status": claim.status,
            "purge_audit_intent_digest": claim.purge_audit_intent_digest,
        });
        Ok(match claim.status {
            ResourceClaimStatusV1::Deleted => ExecutionOutcome::success(result),
            ResourceClaimStatusV1::NeedsAttention => ExecutionOutcome {
                status: CompletionStatus::NeedsAttention,
                result,
                error_message: "resource provider outcome is unknown and requires reconciliation"
                    .to_string(),
                events: vec![],
            },
            ResourceClaimStatusV1::Purging
                if claim.failure.as_ref().is_some_and(|failure| {
                    failure.retryable
                        && failure.code == ResourceClaimFailureCodeV1::ProviderUnavailable
                }) =>
            {
                ExecutionOutcome {
                    status: CompletionStatus::RetryableFailure,
                    result,
                    error_message:
                        "resource provider was unavailable before purge completion was observed"
                            .to_string(),
                    events: vec![],
                }
            }
            _ => ExecutionOutcome {
                status: CompletionStatus::Failed,
                result,
                error_message: "resource purge did not reach the proven DELETED state".to_string(),
                events: vec![],
            },
        })
    }

    async fn binding_context_apply(
        &self,
        job: &LeasedJob,
        ledger: &mut AgentLedger,
    ) -> Result<ExecutionOutcome, LedgerError> {
        const APPLY_STEP: u32 = 900_000;
        let payload: BindingContextApplyPayload = match decode_payload(job) {
            Ok(payload) => payload,
            Err(outcome) => return Ok(outcome),
        };
        if let Err(error) = payload.validate() {
            return Ok(runtime_error_outcome(&error, false));
        }
        let Some(current) = ledger.runtime_context_for_deployment(&payload.deployment_id)? else {
            return Ok(ExecutionOutcome::failed(format!(
                "deployment {} has no Agent-managed runtime context",
                payload.deployment_id
            )));
        };
        if current.state != "ACTIVE" {
            return Ok(ExecutionOutcome::failed(format!(
                "deployment {} runtime context is {}, not ACTIVE",
                payload.deployment_id, current.state
            )));
        }
        let current_cas = if current.binding_context_state == "REVOKED" {
            current.previous_managed_context.as_ref()
        } else {
            current.managed_context.as_ref()
        };
        if current_cas != payload.previous_context.as_ref() {
            return Ok(ExecutionOutcome::failed(format!(
                "deployment {} binding context CAS is stale; Agent generation is {:?}, payload previous generation is {:?}",
                payload.deployment_id,
                current
                    .managed_context
                    .as_ref()
                    .map(|context| context.generation),
                payload
                    .previous_context
                    .as_ref()
                    .map(|context| context.generation),
            )));
        }
        if payload.context.is_none() {
            let Some(provider) = self.runtime_context_provider.as_ref() else {
                return Ok(ExecutionOutcome::failed(
                    "Node has no Agent-local managed service context provider",
                ));
            };
            if let Some(credentials) = self.workload_credentials.as_ref() {
                credentials.stop_refresh(&payload.deployment_id).await;
            }
            return match provider.revoke_workload_credential(&current.context).await {
                Ok(()) => {
                    ledger.record_binding_context_transition(
                        &payload.deployment_id,
                        &job.job_id,
                        payload.previous_context.as_ref(),
                        None,
                        true,
                        crate::now_ms(),
                    )?;
                    Ok(ExecutionOutcome::success(json!({
                        "deployment_id": payload.deployment_id,
                        "context_generation": 0,
                        "previous_generation": payload.previous_context.as_ref().map(|context| context.generation),
                        "revoked": true,
                        "container_id": current.container_id,
                        "runtime_context_directory_preserved": true,
                    })))
                }
                Err(error) => Ok(runtime_policy_outcome(&error)),
            };
        }
        let desired = payload.context.as_ref().expect("context was checked");
        let Some(provider) = self.runtime_context_provider.as_ref() else {
            return Ok(ExecutionOutcome::failed(
                "Node has no Agent-local managed service context provider",
            ));
        };
        let Some(credentials) = self.workload_credentials.as_ref() else {
            return Ok(ExecutionOutcome::failed(
                "Node has no workload credential exchanger for context reconfiguration",
            ));
        };
        ledger.step_started(
            &job.job_id,
            APPLY_STEP,
            "apply_binding_service_context",
            crate::now_ms(),
        )?;
        let previous_refresh = credentials.status_for(&payload.deployment_id).await;
        // Quiesce the old generation before the new context/token commit. A
        // refresh already in flight must not atomically replace the freshly
        // materialized token after the topology generation changes.
        credentials.stop_refresh(&payload.deployment_id).await;
        let result = async {
            let credential = credentials
                .issue_initial(&payload.deployment_id, &job.job_id, &job.lease_token)
                .await?;
            provider
                .reconfigure_context(
                    &payload.deployment_id,
                    &payload.service_id,
                    desired,
                    &current.context,
                    &credential,
                )
                .await?;
            ledger
                .record_binding_context_transition(
                    &payload.deployment_id,
                    &job.job_id,
                    payload.previous_context.as_ref(),
                    Some(desired),
                    false,
                    crate::now_ms(),
                )
                .map_err(|error| {
                    RuntimePolicyError::Compensation(format!(
                        "persist binding context generation after atomic file commit: {error}"
                    ))
                })?;
            credentials
                .start_refresh(
                    &payload.deployment_id,
                    current.context.clone(),
                    credential.expires_at_ms,
                )
                .await?;
            Ok::<_, RuntimePolicyError>(credential)
        }
        .await;
        match result {
            Ok(credential) => {
                ledger.step_succeeded(
                    &job.job_id,
                    APPLY_STEP,
                    &json!({
                        "deployment_id": payload.deployment_id,
                        "previous_generation": payload.previous_context.as_ref().map(|context| context.generation),
                        "context_generation": desired.generation,
                        "credential_expires_at_ms": credential.expires_at_ms,
                        "prior_and_new_recorded_in_job_ledger": true,
                    }),
                    crate::now_ms(),
                )?;
                Ok(ExecutionOutcome::success(json!({
                    "deployment_id": payload.deployment_id,
                    "context_generation": desired.generation,
                    "credential_expires_at_ms": credential.expires_at_ms,
                })))
            }
            Err(mut error) => {
                if !matches!(&error, RuntimePolicyError::Compensation(_))
                    && let Some(previous) = previous_refresh
                    && let Err(resume) = credentials
                        .start_refresh(
                            &payload.deployment_id,
                            current.context.clone(),
                            previous.expires_at_ms,
                        )
                        .await
                {
                    error = RuntimePolicyError::Compensation(format!(
                        "{error}; additionally failed to resume the previous workload credential refresh: {resume}"
                    ));
                }
                ledger.step_failed(&job.job_id, APPLY_STEP, &error.to_string(), crate::now_ms())?;
                Ok(runtime_policy_outcome(&error))
            }
        }
    }

    async fn prepare_runtime_context(
        &self,
        job: &LeasedJob,
        ledger: &mut AgentLedger,
        spec: &ContainerSpec,
    ) -> Result<Option<MaterializedRuntimeContext>, ContextPreparationError> {
        const MATERIALIZE_STEP: u32 = 800_000;
        spec.runtime_contract.validate().map_err(|error| {
            ContextPreparationError::Outcome(runtime_error_outcome(&error, false))
        })?;
        if spec.managed_service_context.is_none() {
            if spec.runtime_contract.id == RuntimeProfile::JudgeSandboxV1 {
                return Err(ContextPreparationError::Outcome(ExecutionOutcome::failed(
                    "judge-sandbox-v1 requires managed_service_context",
                )));
            }
            // Legacy standard-container-v1 releases do not have a Service
            // Contract v2 identity/binding context and remain runnable.
            return Ok(None);
        }
        let provider = self.runtime_context_provider.as_ref().ok_or_else(|| {
            ContextPreparationError::Outcome(ExecutionOutcome::failed(
                "Node has no Agent-local managed service context provider",
            ))
        })?;
        let has_active_bindings = spec
            .managed_service_context
            .as_ref()
            .is_some_and(|managed| !managed.bindings.is_empty());
        let credentials = if has_active_bindings {
            Some(self.workload_credentials.as_ref().ok_or_else(|| {
                ContextPreparationError::Outcome(ExecutionOutcome::failed(
                    "Node has no workload credential exchanger for this bound managed deployment",
                ))
            })?)
        } else {
            None
        };
        let context = provider
            .plan_context(spec)
            .map_err(|error| ContextPreparationError::Outcome(runtime_policy_outcome(&error)))?
            .ok_or_else(|| {
                ContextPreparationError::Outcome(ExecutionOutcome::failed(
                    "managed service context provider returned no materialized context plan",
                ))
            })?;
        ledger.begin_runtime_context(
            &job.job_id,
            &spec.deployment_id,
            &context,
            crate::now_ms(),
        )?;
        let mut effective_spec = spec.clone();
        effective_spec.runtime_context = Some(context.clone());
        self.prepare_managed_volume(job, ledger, &effective_spec, &context)
            .await?;
        ledger.step_started(
            &job.job_id,
            MATERIALIZE_STEP,
            "materialize_managed_service_context",
            crate::now_ms(),
        )?;

        let result = async {
            if let Some(credentials) = credentials {
                let credential = credentials
                    .issue_initial(&spec.deployment_id, &job.job_id, &job.lease_token)
                    .await?;
                provider
                    .materialize_context(spec, &context, &credential)
                    .await?;
                Ok::<Option<WorkloadCredential>, RuntimePolicyError>(Some(credential))
            } else {
                provider.materialize_unbound_context(spec, &context).await?;
                Ok(None)
            }
        }
        .await;
        match result {
            Ok(credential) => {
                let credential_expires_at_ms = credential
                    .as_ref()
                    .map(|credential| credential.expires_at_ms)
                    .unwrap_or(0);
                ledger.mark_runtime_context_prepared(
                    &spec.deployment_id,
                    &job.job_id,
                    crate::now_ms(),
                )?;
                ledger.step_succeeded(
                    &job.job_id,
                    MATERIALIZE_STEP,
                    &json!({
                        "deployment_id": spec.deployment_id,
                        "runtime_contract": context.contract,
                        "runtime_policy_sha256": context.runtime_policy_sha256,
                        "credential_expires_at_ms": credential_expires_at_ms,
                        "credential_active": credential.is_some(),
                        "credential_persisted_in_ledger": false,
                    }),
                    crate::now_ms(),
                )?;
                Ok(Some(MaterializedRuntimeContext {
                    deployment_id: spec.deployment_id.clone(),
                    context,
                    credential_expires_at_ms,
                    credential_active: credential.is_some(),
                }))
            }
            Err(error) => {
                ledger.step_failed(
                    &job.job_id,
                    MATERIALIZE_STEP,
                    &error.to_string(),
                    crate::now_ms(),
                )?;
                ledger.mark_runtime_context_cleanup_needed(
                    &spec.deployment_id,
                    &error.to_string(),
                    crate::now_ms(),
                )?;
                let mut outcome = runtime_policy_outcome(&error);
                if let Err(compensation) = self
                    .cleanup_runtime_context(
                        job,
                        ledger,
                        MATERIALIZE_STEP + 1,
                        &spec.deployment_id,
                        &context,
                    )
                    .await
                {
                    match compensation {
                        ContextCleanupError::Ledger(error) => {
                            return Err(ContextPreparationError::Ledger(error));
                        }
                        ContextCleanupError::Policy(error) => {
                            outcome.status = CompletionStatus::NeedsAttention;
                            outcome.error_message = format!(
                                "{}; runtime context compensation failed: {error}",
                                outcome.error_message
                            );
                        }
                    }
                }
                Err(ContextPreparationError::Outcome(outcome))
            }
        }
    }

    async fn prepare_managed_volume(
        &self,
        job: &LeasedJob,
        ledger: &mut AgentLedger,
        spec: &ContainerSpec,
        context: &RuntimeContext,
    ) -> Result<(), ContextPreparationError> {
        const CREATE_VOLUME_STEP: u32 = 700_000;
        let volume = spec.managed_volume_spec().map_err(|error| {
            ContextPreparationError::Outcome(runtime_error_outcome(&error, false))
        })?;
        let Some(volume) = volume else {
            return Ok(());
        };
        ledger.begin_managed_volume(&spec.deployment_id, &job.job_id, &volume, crate::now_ms())?;
        let created = self
            .runtime_step(
                ledger,
                job,
                CREATE_VOLUME_STEP,
                "create_managed_release_volume",
                false,
                self.runtime.create_managed_volume(&volume),
            )
            .await;
        match created {
            Ok(()) => {
                ledger.mark_managed_volume_created(
                    &spec.deployment_id,
                    &job.job_id,
                    crate::now_ms(),
                )?;
                Ok(())
            }
            Err(StepError::Ledger(error)) => Err(ContextPreparationError::Ledger(error)),
            Err(StepError::Runtime(mut outcome)) => {
                if let Err(compensation) = self
                    .cleanup_runtime_context(
                        job,
                        ledger,
                        CREATE_VOLUME_STEP + 1,
                        &spec.deployment_id,
                        context,
                    )
                    .await
                {
                    match compensation {
                        ContextCleanupError::Ledger(error) => {
                            return Err(ContextPreparationError::Ledger(error));
                        }
                        ContextCleanupError::Policy(error) => {
                            outcome.status = CompletionStatus::NeedsAttention;
                            outcome.error_message = format!(
                                "{}; managed volume compensation failed: {error}",
                                outcome.error_message
                            );
                        }
                    }
                }
                Err(ContextPreparationError::Outcome(outcome))
            }
        }
    }

    async fn compensate_pre_container_failure(
        &self,
        job: &LeasedJob,
        ledger: &mut AgentLedger,
        mut outcome: ExecutionOutcome,
        materialized: Option<&MaterializedRuntimeContext>,
        step_index: u32,
        phase: &str,
    ) -> Result<ExecutionOutcome, LedgerError> {
        let Some(materialized) = materialized else {
            return Ok(outcome);
        };
        if let Err(error) = self
            .cleanup_runtime_context(
                job,
                ledger,
                step_index,
                &materialized.deployment_id,
                &materialized.context,
            )
            .await
        {
            match error {
                ContextCleanupError::Ledger(error) => return Err(error),
                ContextCleanupError::Policy(error) => {
                    outcome.status = CompletionStatus::NeedsAttention;
                    outcome.error_message = format!(
                        "{}; {phase} failed and pre-container compensation could not remove all owned resources: {error}",
                        outcome.error_message
                    );
                }
            }
        }
        Ok(outcome)
    }

    async fn cleanup_runtime_context(
        &self,
        job: &LeasedJob,
        ledger: &mut AgentLedger,
        step_index: u32,
        deployment_id: &str,
        context: &RuntimeContext,
    ) -> Result<(), ContextCleanupError> {
        let provider = self.runtime_context_provider.as_ref().ok_or_else(|| {
            ContextCleanupError::Policy(RuntimePolicyError::Compensation(
                "runtime context provider is not configured".to_string(),
            ))
        })?;
        if let Some(credentials) = self.workload_credentials.as_ref() {
            credentials.stop_refresh(deployment_id).await;
        }
        ledger
            .begin_runtime_context_cleanup(deployment_id, crate::now_ms())
            .map_err(ContextCleanupError::Ledger)?;
        let volume_removed = match self
            .cleanup_managed_volume(job, ledger, step_index, deployment_id)
            .await
        {
            Ok(removed) => removed,
            Err(error) => {
                let error_message = match &error {
                    ContextCleanupError::Ledger(error) => error.to_string(),
                    ContextCleanupError::Policy(error) => error.to_string(),
                };
                ledger
                    .mark_runtime_context_cleanup_needed(
                        deployment_id,
                        &error_message,
                        crate::now_ms(),
                    )
                    .map_err(ContextCleanupError::Ledger)?;
                return Err(error);
            }
        };
        let context_step = step_index.saturating_add(u32::from(volume_removed));
        ledger
            .step_started(
                &job.job_id,
                context_step,
                "compensate_runtime_context",
                crate::now_ms(),
            )
            .map_err(ContextCleanupError::Ledger)?;
        match provider.compensate(context).await {
            Ok(()) => {
                ledger
                    .finish_runtime_context_cleanup(deployment_id, crate::now_ms())
                    .map_err(ContextCleanupError::Ledger)?;
                ledger
                    .step_succeeded(
                        &job.job_id,
                        context_step,
                        &json!({
                            "deployment_id": deployment_id,
                            "credential_removed": true,
                            "context_removed": true,
                            "managed_volume_removed": volume_removed,
                        }),
                        crate::now_ms(),
                    )
                    .map_err(ContextCleanupError::Ledger)?;
                Ok(())
            }
            Err(error) => {
                ledger
                    .step_failed(
                        &job.job_id,
                        context_step,
                        &error.to_string(),
                        crate::now_ms(),
                    )
                    .map_err(ContextCleanupError::Ledger)?;
                ledger
                    .mark_runtime_context_cleanup_needed(
                        deployment_id,
                        &error.to_string(),
                        crate::now_ms(),
                    )
                    .map_err(ContextCleanupError::Ledger)?;
                Err(ContextCleanupError::Policy(error))
            }
        }
    }

    async fn cleanup_managed_volume(
        &self,
        job: &LeasedJob,
        ledger: &mut AgentLedger,
        step_index: u32,
        deployment_id: &str,
    ) -> Result<bool, ContextCleanupError> {
        let volume = ledger
            .begin_managed_volume_cleanup(deployment_id, crate::now_ms())
            .map_err(ContextCleanupError::Ledger)?;
        let Some(volume) = volume else {
            return Ok(false);
        };
        ledger
            .step_started(
                &job.job_id,
                step_index,
                if volume.lifecycle == orchestrator_runtime::RETAIN_VOLUME_LIFECYCLE {
                    "compensate_retain_managed_volume"
                } else {
                    "compensate_remove_managed_release_volume"
                },
                crate::now_ms(),
            )
            .map_err(ContextCleanupError::Ledger)?;
        if volume.lifecycle == orchestrator_runtime::RETAIN_VOLUME_LIFECYCLE {
            ledger
                .finish_managed_volume_cleanup(deployment_id, crate::now_ms())
                .map_err(ContextCleanupError::Ledger)?;
            ledger
                .step_succeeded(
                    &job.job_id,
                    step_index,
                    &json!({
                        "deployment_id": deployment_id,
                        "logical_name": volume.logical_name,
                        "lifecycle": volume.lifecycle,
                        "retained": true,
                        "detached": true,
                    }),
                    crate::now_ms(),
                )
                .map_err(ContextCleanupError::Ledger)?;
            return Ok(true);
        }
        match self.runtime.remove_managed_volume(&volume).await {
            Ok(()) => {
                ledger
                    .finish_managed_volume_cleanup(deployment_id, crate::now_ms())
                    .map_err(ContextCleanupError::Ledger)?;
                ledger
                    .step_succeeded(
                        &job.job_id,
                        step_index,
                        &json!({
                            "deployment_id": deployment_id,
                            "volume_name": volume.name,
                            "logical_name": volume.logical_name,
                            "lifecycle": volume.lifecycle,
                            "ownership_verified": true,
                        }),
                        crate::now_ms(),
                    )
                    .map_err(ContextCleanupError::Ledger)?;
                Ok(true)
            }
            Err(error) => {
                ledger
                    .step_failed(&job.job_id, step_index, &error.to_string(), crate::now_ms())
                    .map_err(ContextCleanupError::Ledger)?;
                ledger
                    .mark_managed_volume_cleanup_needed(deployment_id, crate::now_ms())
                    .map_err(ContextCleanupError::Ledger)?;
                Err(ContextCleanupError::Policy(
                    RuntimePolicyError::Compensation(format!(
                        "remove owned managed volume {}: {error}",
                        volume.name
                    )),
                ))
            }
        }
    }

    async fn activate_runtime_context(
        &self,
        job: &LeasedJob,
        ledger: &mut AgentLedger,
        spec: &ContainerSpec,
        materialized: &MaterializedRuntimeContext,
    ) -> Result<(), LedgerError> {
        ledger.activate_runtime_context(&spec.deployment_id, &job.job_id, crate::now_ms())?;
        ledger.record_binding_context_transition(
            &spec.deployment_id,
            &job.job_id,
            None,
            spec.managed_service_context.as_ref(),
            false,
            crate::now_ms(),
        )?;
        if materialized.credential_active {
            let credentials = self.workload_credentials.as_ref().ok_or_else(|| {
                LedgerError::InvalidState(
                    "materialized runtime context has no credential supervisor".to_string(),
                )
            })?;
            if let Err(error) = credentials
                .start_refresh(
                    &spec.deployment_id,
                    materialized.context.clone(),
                    materialized.credential_expires_at_ms,
                )
                .await
            {
                ledger.mark_runtime_context_needs_attention(
                    &spec.deployment_id,
                    &error.to_string(),
                    crate::now_ms(),
                )?;
                return Err(LedgerError::InvalidState(format!(
                    "start workload credential refresh for deployment {}: {error}",
                    spec.deployment_id
                )));
            }
        }
        Ok(())
    }

    async fn install(
        &self,
        job: &LeasedJob,
        ledger: &mut AgentLedger,
        cancellation: watch::Receiver<bool>,
    ) -> Result<ExecutionOutcome, LedgerError> {
        let payload: InstallPayload = match decode_payload(job) {
            Ok(payload) => payload,
            Err(outcome) => return Ok(outcome),
        };
        self.install_payload(job, ledger, cancellation, payload)
            .await
    }

    async fn install_payload(
        &self,
        job: &LeasedJob,
        ledger: &mut AgentLedger,
        cancellation: watch::Receiver<bool>,
        mut payload: InstallPayload,
    ) -> Result<ExecutionOutcome, LedgerError> {
        if let Err(error) = payload
            .spec
            .runtime_contract
            .validate_health_gate(&payload.health_gate)
        {
            return Ok(runtime_error_outcome(&error, false));
        }
        let materialized = match self
            .prepare_runtime_context(job, ledger, &payload.spec)
            .await
        {
            Ok(context) => context,
            Err(ContextPreparationError::Ledger(error)) => return Err(error),
            Err(ContextPreparationError::Outcome(outcome)) => return Ok(outcome),
        };
        if let Some(materialized) = materialized.as_ref() {
            payload.spec.runtime_context = Some(materialized.context.clone());
        }
        if let Some(artifact) = payload.offline_oci_artifact.as_ref() {
            let Some(fetcher) = self.artifact_fetcher.as_ref() else {
                return self
                    .compensate_pre_container_failure(
                        job,
                        ledger,
                        ExecutionOutcome::failed(
                            "offline OCI artifact was assigned but no authenticated artifact fetcher is configured",
                        ),
                        materialized.as_ref(),
                        800_010,
                        "offline OCI artifact selection",
                    )
                    .await;
            };
            let downloaded = match fetcher.download(job, artifact).await {
                Ok(downloaded) => downloaded,
                Err(error) => {
                    return self
                        .compensate_pre_container_failure(
                            job,
                            ledger,
                            artifact_download_outcome(error),
                            materialized.as_ref(),
                            800_010,
                            "offline OCI artifact download",
                        )
                        .await;
                }
            };
            if let Err(error) = self
                .runtime_step(
                    ledger,
                    job,
                    1,
                    "import_oci_archive",
                    false,
                    self.runtime
                        .import_image_archive_path(downloaded.path(), &payload.spec.image),
                )
                .await
            {
                let outcome = match error {
                    StepError::Ledger(error) => return Err(error),
                    StepError::Runtime(outcome) => outcome,
                };
                return self
                    .compensate_pre_container_failure(
                        job,
                        ledger,
                        outcome,
                        materialized.as_ref(),
                        800_010,
                        "OCI archive import",
                    )
                    .await;
            }
        } else if let Err(error) = self
            .runtime_step(
                ledger,
                job,
                1,
                "pull_image",
                false,
                self.runtime.pull_image(&payload.spec.image),
            )
            .await
        {
            let outcome = match error {
                StepError::Ledger(error) => return Err(error),
                StepError::Runtime(outcome) => outcome,
            };
            return self
                .compensate_pre_container_failure(
                    job,
                    ledger,
                    outcome,
                    materialized.as_ref(),
                    800_010,
                    "OCI image pull",
                )
                .await;
        }
        if materialized.is_some() {
            ledger.mark_runtime_context_creating(
                &payload.spec.deployment_id,
                &job.job_id,
                crate::now_ms(),
            )?;
        }
        let instance = match self
            .runtime_step(
                ledger,
                job,
                2,
                "create_container",
                true,
                self.runtime.create_container(&payload.spec),
            )
            .await
        {
            Ok(instance) => instance,
            Err(error) => {
                if let Some(materialized) = materialized.as_ref() {
                    match &error {
                        StepError::Runtime(outcome)
                            if outcome.status != CompletionStatus::NeedsAttention =>
                        {
                            if let Err(compensation) = self
                                .cleanup_runtime_context(
                                    job,
                                    ledger,
                                    800_002,
                                    &payload.spec.deployment_id,
                                    &materialized.context,
                                )
                                .await
                            {
                                return match compensation {
                                    ContextCleanupError::Ledger(error) => Err(error),
                                    ContextCleanupError::Policy(cleanup) => {
                                        let mut outcome = match error {
                                            StepError::Runtime(outcome) => outcome,
                                            StepError::Ledger(_) => unreachable!(),
                                        };
                                        outcome.status = CompletionStatus::NeedsAttention;
                                        outcome.error_message = format!(
                                            "{}; runtime context compensation failed: {cleanup}",
                                            outcome.error_message
                                        );
                                        Ok(outcome)
                                    }
                                };
                            }
                        }
                        StepError::Runtime(outcome) => {
                            ledger.mark_runtime_context_needs_attention(
                                &payload.spec.deployment_id,
                                &outcome.error_message,
                                crate::now_ms(),
                            )?;
                        }
                        StepError::Ledger(_) => {}
                    }
                }
                return step_result(error);
            }
        };
        if materialized.is_some() {
            ledger.bind_runtime_context(
                &payload.spec.deployment_id,
                &job.job_id,
                &instance.container_id,
                crate::now_ms(),
            )?;
        }
        if payload.start
            && let Err(error) = self
                .runtime_step(
                    ledger,
                    job,
                    3,
                    "start_container",
                    true,
                    self.runtime.start_container(&instance.container_id),
                )
                .await
        {
            return self
                .compensate_uncommitted_container(
                    job,
                    ledger,
                    4,
                    &instance.container_id,
                    error,
                    payload.health_gate.compensation_timeout_ms,
                    "install",
                    materialized.as_ref(),
                )
                .await;
        }
        if !payload.start {
            let inspected = match self
                .runtime_step(
                    ledger,
                    job,
                    4,
                    "inspect_container",
                    true,
                    self.runtime.inspect_container(&instance.container_id),
                )
                .await
            {
                Ok(instance) => instance,
                Err(error) => {
                    return self
                        .compensate_uncommitted_container(
                            job,
                            ledger,
                            5,
                            &instance.container_id,
                            error,
                            payload.health_gate.compensation_timeout_ms,
                            "install",
                            materialized.as_ref(),
                        )
                        .await;
                }
            };
            if let Some(materialized) = materialized.as_ref() {
                self.activate_runtime_context(job, ledger, &payload.spec, materialized)
                    .await?;
            }
            return Ok(ExecutionOutcome::success(json!({ "instance": inspected })));
        }

        match self
            .wait_for_container_health(
                job,
                ledger,
                4,
                &instance.container_id,
                &payload.health_gate,
                cancellation,
                "install",
            )
            .await
        {
            Ok((inspected, events)) => {
                if let Some(materialized) = materialized.as_ref() {
                    self.activate_runtime_context(job, ledger, &payload.spec, materialized)
                        .await?;
                }
                Ok(ExecutionOutcome::success_with_events(
                    json!({ "instance": inspected }),
                    events,
                ))
            }
            Err(HealthGateError::Ledger(error)) => Err(error),
            Err(HealthGateError::Failed {
                outcome,
                compensation_step,
            }) => {
                self.compensate_uncommitted_container(
                    job,
                    ledger,
                    compensation_step,
                    &instance.container_id,
                    StepError::Runtime(outcome),
                    payload.health_gate.compensation_timeout_ms,
                    "install",
                    materialized.as_ref(),
                )
                .await
            }
        }
    }

    async fn release_pipeline(
        &self,
        job: &LeasedJob,
        ledger: &mut AgentLedger,
        cancellation: watch::Receiver<bool>,
        claim_guard: &mut ReleasePipelineClaimGuard,
    ) -> Result<ExecutionOutcome, LedgerError> {
        const AUTH_APPLY_STEP: u32 = 1_000_000;
        const AUTH_COMPENSATE_STEP: u32 = 1_000_001;
        const MIGRATION_BASE_STEP: u32 = 1_100_000;
        const GATEWAY_PUBLISH_STEP: u32 = 2_000_000;
        const GATEWAY_RUNTIME_COMPENSATE_STEP: u32 = 2_000_001;
        const GATEWAY_AUTH_COMPENSATE_STEP: u32 = 2_000_002;
        const RESOURCE_CLAIM_BASE_STEP: u32 = 900_000;

        let mut payload: ReleasePipelinePayload = match decode_payload(job) {
            Ok(payload) => payload,
            Err(outcome) => return Ok(outcome),
        };
        if let Err(message) = validate_pipeline_payload(&payload) {
            return Ok(ExecutionOutcome::failed(message));
        }

        let mut resource_outputs = std::collections::BTreeMap::new();
        if !payload.resource_claims.is_empty() {
            let Some(manager) = self.resource_claims.as_ref() else {
                return Ok(ExecutionOutcome::failed(
                    "ReleasePipeline carries resource claims but this Agent has no resource provider configured",
                ));
            };
            for (index, step) in payload.resource_claims.iter().enumerate() {
                claim_guard.arm();
                let step_index = RESOURCE_CLAIM_BASE_STEP.saturating_add(index as u32);
                ledger.step_started(
                    &job.job_id,
                    step_index,
                    "resource_claim_ensure",
                    crate::now_ms(),
                )?;
                let result = manager.ensure(step).await;
                let claim = match result {
                    Ok(claim) if claim.status == ResourceClaimStatusV1::Ready => claim,
                    Ok(claim) => {
                        let message = format!(
                            "resource claim {} did not become READY ({:?})",
                            step.claim_id, claim.status
                        );
                        ledger.step_failed(&job.job_id, step_index, &message, crate::now_ms())?;
                        return Ok(if claim.status == ResourceClaimStatusV1::NeedsAttention {
                            needs_attention_outcome(message)
                        } else {
                            ExecutionOutcome::failed(message)
                        });
                    }
                    Err(error) => {
                        let message =
                            format!("resource claim {} ensure failed: {error}", step.claim_id);
                        ledger.step_failed(&job.job_id, step_index, &message, crate::now_ms())?;
                        return Ok(needs_attention_outcome(message));
                    }
                };
                let output = claim.output_secret.as_ref().ok_or_else(|| {
                    LedgerError::InvalidState(format!(
                        "READY resource claim {} omitted output reference",
                        step.claim_id
                    ))
                })?;
                let output_path =
                    manager
                        .output_path(&output.reference)
                        .await
                        .map_err(|error| {
                            LedgerError::InvalidState(format!(
                                "resolve resource output for {}: {error}",
                                step.claim_id
                            ))
                        })?;
                resource_outputs.insert(
                    step.resource_name.clone(),
                    (
                        step.output_path_environment.clone(),
                        output.reference.clone(),
                        output_path,
                    ),
                );
                ledger.step_succeeded(
                    &job.job_id,
                    step_index,
                    &json!({
                        "claim_id": step.claim_id,
                        "status": "READY",
                        "output_reference": output.reference,
                        "secret_values_persisted": false,
                    }),
                    crate::now_ms(),
                )?;
            }
        }

        if let Some(materialization) = payload.materialization.as_ref() {
            const MATERIALIZE_STEP: u32 = 990_000;
            ledger.step_started(
                &job.job_id,
                MATERIALIZE_STEP,
                "materialize_runtime_config_and_secrets",
                crate::now_ms(),
            )?;
            match self
                .pipeline_provider
                .materialize_runtime(materialization)
                .await
            {
                Ok(environment) => {
                    ledger.step_succeeded(
                        &job.job_id,
                        MATERIALIZE_STEP,
                        &json!({
                            "environment_keys": environment
                                .iter()
                                .filter_map(|item| item.split_once('=').map(|(key, _)| key))
                                .collect::<Vec<_>>(),
                            "secret_values_persisted": false,
                        }),
                        crate::now_ms(),
                    )?;
                    payload.install.spec.environment = environment;
                }
                Err(error) => {
                    ledger.step_failed(
                        &job.job_id,
                        MATERIALIZE_STEP,
                        &error.to_string(),
                        crate::now_ms(),
                    )?;
                    return Ok(provider_error_outcome("runtime materialization", &error));
                }
            }
        }

        for (resource_name, (environment, _reference, path)) in &resource_outputs {
            let mount = ResourceSecretFileMount {
                resource_name: resource_name.clone(),
                host_source_path: strict_path_text(
                    path,
                    &format!("resource output for {resource_name}"),
                )?,
            };
            let destination = mount
                .container_destination()
                .map_err(|error| LedgerError::InvalidState(error.to_string()))?;
            payload
                .install
                .spec
                .environment
                .push(format!("{environment}={destination}"));
            payload.install.spec.resource_secret_file_mounts.push(mount);
        }
        payload
            .install
            .spec
            .resource_secret_file_mounts
            .sort_by(|left, right| left.resource_name.cmp(&right.resource_name));

        let mut auth_applied = false;
        if let Some(auth) = payload.auth.as_ref() {
            let apply = self
                .provider_step(
                    ledger,
                    job,
                    AUTH_APPLY_STEP,
                    "auth_apply",
                    self.pipeline_provider.apply_auth(auth),
                )
                .await;
            if let Err(error) = apply {
                let compensation = self
                    .provider_step(
                        ledger,
                        job,
                        AUTH_COMPENSATE_STEP,
                        "auth_compensate_after_apply_failure",
                        self.pipeline_provider.compensate_auth(&auth.service_name),
                    )
                    .await;
                return pipeline_provider_failure("auth apply", error, compensation.err(), false);
            }
            auth_applied = true;
        }

        let mut applied_provisioners: Vec<&TypedProvisionerStep> = Vec::new();
        for (index, provisioner) in payload.provisioners.iter().enumerate() {
            let step_index = 1_010_000_u32.saturating_add(index as u32);
            match self
                .provider_step(
                    ledger,
                    job,
                    step_index,
                    &format!("{}_apply", provisioner.provider_name()),
                    self.pipeline_provider.apply_provisioner(provisioner),
                )
                .await
            {
                Ok(()) => applied_provisioners.push(provisioner),
                Err(error) => {
                    let compensation_errors = self
                        .compensate_provisioners(job, ledger, &applied_provisioners)
                        .await;
                    let auth_compensation = if auth_applied {
                        self.provider_step(
                            ledger,
                            job,
                            AUTH_COMPENSATE_STEP,
                            "auth_compensate_after_provider_failure",
                            self.pipeline_provider
                                .compensate_auth(&payload.auth.as_ref().unwrap().service_name),
                        )
                        .await
                        .err()
                    } else {
                        None
                    };
                    let mut outcome = provider_error_outcome(provisioner.provider_name(), &error);
                    append_compensation_errors(
                        &mut outcome,
                        compensation_errors,
                        auth_compensation,
                    );
                    return Ok(outcome);
                }
            }
        }

        let mut migration_results = Vec::with_capacity(payload.migrations.len());
        let mut applied_migration = false;
        for (index, migration) in payload.migrations.iter().enumerate() {
            if *cancellation.borrow() {
                let resource_errors = self
                    .compensate_provisioners(job, ledger, &applied_provisioners)
                    .await;
                let compensation = if auth_applied {
                    self.provider_step(
                        ledger,
                        job,
                        AUTH_COMPENSATE_STEP,
                        "auth_compensate_after_cancellation",
                        self.pipeline_provider.compensate_auth(
                            payload
                                .auth
                                .as_ref()
                                .expect("auth_applied implies an auth step")
                                .service_name
                                .as_str(),
                        ),
                    )
                    .await
                    .err()
                } else {
                    None
                };
                return Ok(if !resource_errors.is_empty() || compensation.is_some() {
                    needs_attention_outcome(format!(
                        "pipeline cancellation compensation failed: {}{}",
                        resource_errors.join("; "),
                        compensation
                            .map(|error| format!("; auth: {error}"))
                            .unwrap_or_default()
                    ))
                } else {
                    ExecutionOutcome {
                        status: CompletionStatus::Cancelled,
                        result: json!({"cancelled_before_runtime_install": true}),
                        error_message: "release pipeline was cancelled before runtime install"
                            .to_string(),
                        events: vec![],
                    }
                });
            }
            let index = u32::try_from(index).unwrap_or(u32::MAX / 16);
            let base = MIGRATION_BASE_STEP.saturating_add(index.saturating_mul(16));
            let mut migration = migration.clone();
            let mut migration_mounts = Vec::new();
            for resource_name in &migration.resource_claims {
                let Some((environment, _reference, path)) = resource_outputs.get(resource_name)
                else {
                    return Ok(ExecutionOutcome::failed(format!(
                        "OCI migration references unresolved resource claim {resource_name}"
                    )));
                };
                let mount = ResourceSecretFileMount {
                    resource_name: resource_name.clone(),
                    host_source_path: strict_path_text(
                        path,
                        &format!("migration resource output for {resource_name}"),
                    )?,
                };
                let destination = mount
                    .container_destination()
                    .map_err(|error| LedgerError::InvalidState(error.to_string()))?;
                migration
                    .environment
                    .push(format!("{environment}={destination}"));
                migration_mounts.push(mount);
            }
            if migration.resource_claims.len() == 1 {
                let destination = migration_mounts[0]
                    .container_destination()
                    .map_err(|error| LedgerError::InvalidState(error.to_string()))?;
                migration
                    .environment
                    .push(format!("OJOS_RESOURCE_OUTPUT_FILE={destination}"));
            }
            migration_mounts.sort_by(|left, right| left.resource_name.cmp(&right.resource_name));
            match self
                .run_oci_migration(job, ledger, &migration, migration_mounts, base)
                .await
            {
                Ok(result) => {
                    applied_migration |= result
                        .get("status")
                        .and_then(Value::as_str)
                        .is_some_and(|status| status == "APPLIED");
                    migration_results.push(result);
                }
                Err(PipelineExecutionError::Ledger(error)) => return Err(error),
                Err(PipelineExecutionError::Outcome(mut outcome)) => {
                    let resource_errors = self
                        .compensate_provisioners(job, ledger, &applied_provisioners)
                        .await;
                    let compensation = if auth_applied {
                        self.provider_step(
                            ledger,
                            job,
                            AUTH_COMPENSATE_STEP,
                            "auth_compensate_after_migration_failure",
                            self.pipeline_provider.compensate_auth(
                                payload
                                    .auth
                                    .as_ref()
                                    .expect("auth_applied implies an auth step")
                                    .service_name
                                    .as_str(),
                            ),
                        )
                        .await
                        .err()
                    } else {
                        None
                    };
                    append_compensation_errors(&mut outcome, resource_errors, compensation);
                    return Ok(outcome);
                }
            }
        }

        let mut install_outcome = self
            .install_payload(job, ledger, cancellation.clone(), payload.install.clone())
            .await?;
        if install_outcome.status != CompletionStatus::Succeeded {
            let resource_errors = self
                .compensate_provisioners(job, ledger, &applied_provisioners)
                .await;
            let mut auth_compensation = None;
            if auth_applied {
                let compensation = self
                    .provider_step(
                        ledger,
                        job,
                        AUTH_COMPENSATE_STEP,
                        "auth_compensate_after_runtime_failure",
                        self.pipeline_provider.compensate_auth(
                            payload
                                .auth
                                .as_ref()
                                .expect("auth_applied implies an auth step")
                                .service_name
                                .as_str(),
                        ),
                    )
                    .await;
                if let Err(error) = compensation {
                    auth_compensation = Some(error);
                }
            }
            append_compensation_errors(&mut install_outcome, resource_errors, auth_compensation);
            if applied_migration {
                install_outcome.status = CompletionStatus::NeedsAttention;
                install_outcome.error_message = format!(
                    "{}; an OCI migration was durably applied before runtime installation failed",
                    install_outcome.error_message
                );
            }
            return Ok(install_outcome);
        }

        let instance: RuntimeInstance = install_outcome
            .result
            .get("instance")
            .cloned()
            .ok_or_else(|| LedgerError::InvalidState("pipeline install omitted instance".into()))
            .and_then(|value| serde_json::from_value(value).map_err(LedgerError::Json))?;

        if let Some(gateway) = payload.gateway.as_ref() {
            let publish = self
                .provider_step(
                    ledger,
                    job,
                    GATEWAY_PUBLISH_STEP,
                    "gateway_publish",
                    self.pipeline_provider.publish_gateway(gateway),
                )
                .await;
            if let Err(provider_error) = publish {
                let runtime_context =
                    ledger.runtime_context_for_container(&instance.container_id)?;
                let remove = self
                    .runtime_step(
                        ledger,
                        job,
                        GATEWAY_RUNTIME_COMPENSATE_STEP,
                        "remove_container_after_gateway_failure",
                        true,
                        self.runtime.remove_container(&instance.container_id, true),
                    )
                    .await;
                let context_cleanup_error = if remove.is_ok() {
                    if let Some(runtime_context) = runtime_context.as_ref() {
                        self.cleanup_runtime_context(
                            job,
                            ledger,
                            GATEWAY_RUNTIME_COMPENSATE_STEP + 2,
                            &runtime_context.deployment_id,
                            &runtime_context.context,
                        )
                        .await
                        .err()
                    } else {
                        None
                    }
                } else {
                    None
                };
                let resource_errors = self
                    .compensate_provisioners(job, ledger, &applied_provisioners)
                    .await;
                let auth_compensation = if auth_applied {
                    self.provider_step(
                        ledger,
                        job,
                        GATEWAY_AUTH_COMPENSATE_STEP,
                        "auth_compensate_after_gateway_failure",
                        self.pipeline_provider.compensate_auth(
                            payload
                                .auth
                                .as_ref()
                                .expect("auth_applied implies an auth step")
                                .service_name
                                .as_str(),
                        ),
                    )
                    .await
                    .err()
                } else {
                    None
                };
                let mut outcome = provider_error_outcome("gateway publish", &provider_error);
                if let Err(error) = remove {
                    outcome.status = CompletionStatus::NeedsAttention;
                    outcome.error_message = format!(
                        "{}; runtime compensation failed: {}",
                        outcome.error_message,
                        step_error_message(&error)
                    );
                }
                if let Some(error) = context_cleanup_error {
                    match error {
                        ContextCleanupError::Ledger(error) => return Err(error),
                        ContextCleanupError::Policy(error) => {
                            outcome.status = CompletionStatus::NeedsAttention;
                            outcome.error_message = format!(
                                "{}; Agent runtime context compensation failed: {error}",
                                outcome.error_message
                            );
                        }
                    }
                }
                append_compensation_errors(&mut outcome, resource_errors, auth_compensation);
                if applied_migration {
                    outcome.status = CompletionStatus::NeedsAttention;
                    outcome.error_message = format!(
                        "{}; an OCI migration was already applied",
                        outcome.error_message
                    );
                }
                return Ok(outcome);
            }
        }

        install_outcome.result = json!({
            "instance": instance,
            "pipeline": {
                "auth_applied": auth_applied,
                "provisioners": payload.provisioners.iter().map(TypedProvisionerStep::provider_name).collect::<Vec<_>>(),
                "migrations": migration_results,
                "gateway_published": payload.gateway.is_some(),
                "resource_claims": payload.resource_claims.iter().map(|claim| json!({
                    "claim_id": claim.claim_id,
                    "resource_name": claim.resource_name,
                    "status": "READY",
                })).collect::<Vec<_>>(),
            }
        });
        Ok(install_outcome)
    }

    async fn provider_step<F>(
        &self,
        ledger: &mut AgentLedger,
        job: &LeasedJob,
        step_index: u32,
        step_name: &str,
        future: F,
    ) -> Result<(), PipelineProviderError>
    where
        F: Future<Output = Result<(), PipelineProviderError>>,
    {
        if let Err(error) = ledger.step_started(&job.job_id, step_index, step_name, crate::now_ms())
        {
            return Err(PipelineProviderError::Ambiguous(format!(
                "persist provider step start: {error}"
            )));
        }
        match future.await {
            Ok(()) => {
                ledger
                    .step_succeeded(
                        &job.job_id,
                        step_index,
                        &json!({"status": "APPLIED"}),
                        crate::now_ms(),
                    )
                    .map_err(|error| {
                        PipelineProviderError::Ambiguous(format!(
                            "persist provider step success: {error}"
                        ))
                    })?;
                Ok(())
            }
            Err(error) => {
                ledger
                    .step_failed(&job.job_id, step_index, &error.to_string(), crate::now_ms())
                    .map_err(|ledger_error| {
                        PipelineProviderError::Ambiguous(format!(
                            "{error}; persist provider failure: {ledger_error}"
                        ))
                    })?;
                Err(error)
            }
        }
    }

    async fn compensate_provisioners(
        &self,
        job: &LeasedJob,
        ledger: &mut AgentLedger,
        applied: &[&TypedProvisionerStep],
    ) -> Vec<String> {
        let mut errors = Vec::new();
        for (index, provisioner) in applied.iter().rev().enumerate() {
            let result = self
                .provider_step(
                    ledger,
                    job,
                    1_020_000_u32.saturating_add(index as u32),
                    &format!("{}_compensate", provisioner.provider_name()),
                    self.pipeline_provider.compensate_provisioner(provisioner),
                )
                .await;
            if let Err(error) = result {
                errors.push(format!("{}: {error}", provisioner.provider_name()));
            }
        }
        errors
    }

    async fn run_oci_migration(
        &self,
        job: &LeasedJob,
        ledger: &mut AgentLedger,
        migration: &OciMigrationStep,
        resource_secret_file_mounts: Vec<ResourceSecretFileMount>,
        base_step: u32,
    ) -> Result<Value, PipelineExecutionError> {
        if migration.timeout_ms == 0 || migration.timeout_ms > 60 * 60_000 {
            return Err(PipelineExecutionError::Outcome(ExecutionOutcome::failed(
                "OCI migration timeout_ms must be between 1 and 3600000",
            )));
        }
        if !is_sha256_checksum(&migration.checksum) || migration.command.is_empty() {
            return Err(PipelineExecutionError::Outcome(ExecutionOutcome::failed(
                "OCI migration requires sha256:<64 lowercase hex> checksum and a non-empty command",
            )));
        }
        self.runtime_step(
            ledger,
            job,
            base_step,
            "migration_pull_image",
            false,
            self.runtime.pull_image(&migration.image),
        )
        .await
        .map_err(PipelineExecutionError::from_step)?;
        let migration_id = format!(
            "migration-{}-{}-{}",
            migration.service_name,
            migration.version,
            &migration.checksum[7..15]
        );
        let resource_claims_sha256 = migration_resource_claims_sha256(&migration.resource_claims)
            .map_err(|error| {
            PipelineExecutionError::Outcome(ExecutionOutcome::failed(error.to_string()))
        })?;
        let migration_identity_sha256 = migration_identity_sha256(
            &migration.service_name,
            &migration.version,
            &migration.checksum,
            &migration.image,
            &resource_claims_sha256,
        )
        .map_err(|error| {
            PipelineExecutionError::Outcome(ExecutionOutcome::failed(error.to_string()))
        })?;
        let spec = ContainerSpec {
            deployment_id: migration_id,
            service_id: migration.service_name.clone(),
            generation: 1,
            image: migration.image.clone(),
            runtime_contract: orchestrator_runtime::RuntimeContract::standard_v1(),
            runtime_context: None,
            resource_secret_file_mounts,
            retained_volume: None,
            managed_service_context: None,
            command: migration.command.clone(),
            environment: migration.environment.clone(),
            labels: std::collections::HashMap::from([
                (
                    MIGRATION_RUNTIME_ROLE_LABEL.to_string(),
                    MIGRATION_RUNTIME_ROLE.to_string(),
                ),
                (
                    MIGRATION_MANAGED_BY_LABEL.to_string(),
                    MIGRATION_MANAGED_BY.to_string(),
                ),
                (MIGRATION_JOB_ID_LABEL.to_string(), job.job_id.clone()),
                (
                    MIGRATION_SERVICE_LABEL.to_string(),
                    migration.service_name.clone(),
                ),
                (
                    MIGRATION_VERSION_LABEL.to_string(),
                    migration.version.clone(),
                ),
                (
                    MIGRATION_CHECKSUM_LABEL.to_string(),
                    migration.checksum.clone(),
                ),
                (
                    MIGRATION_RESOURCE_CLAIMS_LABEL.to_string(),
                    resource_claims_sha256.clone(),
                ),
                (
                    MIGRATION_IDENTITY_LABEL.to_string(),
                    migration_identity_sha256.clone(),
                ),
            ]),
            published_endpoint: None,
        };
        let ledger_started = if migration.dry_run {
            false
        } else {
            match ledger.begin_migration(
                &migration.service_name,
                &migration.version,
                &migration.checksum,
                &migration.image.to_string(),
                &resource_claims_sha256,
                &migration_identity_sha256,
                &job.job_id,
                crate::now_ms(),
            )? {
                MigrationDecision::AlreadyApplied(_) => {
                    return Ok(json!({
                        "version": migration.version,
                        "checksum": migration.checksum,
                        "image": migration.image,
                        "status": "ALREADY_APPLIED",
                    }));
                }
                MigrationDecision::Execute => true,
            }
        };

        let instance = match self
            .runtime_step(
                ledger,
                job,
                base_step + 1,
                "migration_create_container",
                true,
                self.runtime.create_container(&spec),
            )
            .await
        {
            Ok(instance) => instance,
            Err(error) => {
                if ledger_started {
                    let detail = step_error_message(&error);
                    ledger.mark_migration_needs_attention(
                        &migration.service_name,
                        &migration.version,
                        &job.job_id,
                        &format!("migration container creation outcome is unknown: {detail}"),
                        crate::now_ms(),
                    )?;
                }
                return Err(PipelineExecutionError::Outcome(needs_attention_outcome(
                    "migration container creation outcome is unknown; refusing automatic replay",
                )));
            }
        };
        if ledger_started
            && let Err(error) = ledger.set_migration_container(
                &migration.service_name,
                &migration.version,
                &job.job_id,
                &instance.container_id,
                crate::now_ms(),
            )
        {
            return Err(PipelineExecutionError::Outcome(needs_attention_outcome(
                format!(
                    "migration container {} was created but its identity could not be persisted: {error}; refusing cleanup or replay",
                    instance.container_id
                ),
            )));
        }

        if let Err(error) = self
            .runtime_step(
                ledger,
                job,
                base_step + 3,
                "migration_start_container",
                true,
                self.runtime.start_container(&instance.container_id),
            )
            .await
        {
            return self
                .migration_ambiguous_failure(
                    job,
                    ledger,
                    migration,
                    ledger_started,
                    &instance.container_id,
                    base_step + 4,
                    step_error_message(&error),
                )
                .await;
        }

        ledger.step_started(
            &job.job_id,
            base_step + 4,
            "migration_wait_container",
            crate::now_ms(),
        )?;
        let exit = tokio::time::timeout(
            Duration::from_millis(migration.timeout_ms),
            self.runtime.wait_container(&instance.container_id),
        )
        .await;
        let exit_code = match exit {
            Ok(Ok(code)) => {
                ledger.step_succeeded(
                    &job.job_id,
                    base_step + 4,
                    &json!({"exit_code": code}),
                    crate::now_ms(),
                )?;
                code
            }
            Ok(Err(error)) => {
                ledger.step_failed(
                    &job.job_id,
                    base_step + 4,
                    &error.to_string(),
                    crate::now_ms(),
                )?;
                return self
                    .migration_ambiguous_failure(
                        job,
                        ledger,
                        migration,
                        ledger_started,
                        &instance.container_id,
                        base_step + 5,
                        error.to_string(),
                    )
                    .await;
            }
            Err(_) => {
                let message = format!(
                    "OCI migration {} timed out after {}ms",
                    migration.version, migration.timeout_ms
                );
                ledger.step_failed(&job.job_id, base_step + 4, &message, crate::now_ms())?;
                return self
                    .migration_ambiguous_failure(
                        job,
                        ledger,
                        migration,
                        ledger_started,
                        &instance.container_id,
                        base_step + 5,
                        message,
                    )
                    .await;
            }
        };
        if exit_code != 0 {
            return self
                .migration_ambiguous_failure(
                    job,
                    ledger,
                    migration,
                    ledger_started,
                    &instance.container_id,
                    base_step + 5,
                    format!(
                        "OCI migration {} exited with status {exit_code}",
                        migration.version
                    ),
                )
                .await;
        }

        if ledger_started {
            ledger.finish_migration(
                &migration.service_name,
                &migration.version,
                &job.job_id,
                true,
                None,
                crate::now_ms(),
            )?;
        }
        self.runtime_step(
            ledger,
            job,
            base_step + 5,
            "migration_remove_container",
            true,
            self.runtime.remove_container(&instance.container_id, true),
        )
        .await
        .map_err(PipelineExecutionError::from_step)?;
        Ok(json!({
            "version": migration.version,
            "checksum": migration.checksum,
            "image": migration.image,
            "status": if migration.dry_run { "DRY_RUN_SUCCEEDED" } else { "APPLIED" },
        }))
    }

    #[allow(clippy::too_many_arguments)]
    async fn migration_ambiguous_failure(
        &self,
        job: &LeasedJob,
        ledger: &mut AgentLedger,
        migration: &OciMigrationStep,
        ledger_started: bool,
        container_id: &str,
        cleanup_step: u32,
        message: String,
    ) -> Result<Value, PipelineExecutionError> {
        if ledger_started {
            ledger.mark_migration_needs_attention(
                &migration.service_name,
                &migration.version,
                &job.job_id,
                &message,
                crate::now_ms(),
            )?;
            return Err(PipelineExecutionError::Outcome(needs_attention_outcome(
                format!(
                    "{message}; preserving registered migration container {container_id} as reconciliation evidence and refusing automatic replay"
                ),
            )));
        }
        // A dry-run has no durable migration fact. Its stopped or ambiguous
        // container is still cleaned best-effort, while real migration
        // containers above are preserved once the outcome becomes unknown.
        let cleanup = self
            .runtime_step(
                ledger,
                job,
                cleanup_step,
                "migration_remove_after_failure",
                true,
                self.runtime.remove_container(container_id, true),
            )
            .await;
        let detail = match cleanup {
            Ok(()) => message,
            Err(error) => format!(
                "{message}; migration container cleanup failed: {}",
                step_error_message(&error)
            ),
        };
        Err(PipelineExecutionError::Outcome(needs_attention_outcome(
            detail,
        )))
    }

    #[allow(clippy::too_many_arguments)]
    async fn wait_for_container_health(
        &self,
        job: &LeasedJob,
        ledger: &mut AgentLedger,
        first_step: u32,
        container_id: &str,
        policy: &HealthGatePolicy,
        mut cancellation: watch::Receiver<bool>,
        action: &str,
    ) -> Result<(RuntimeInstance, Vec<NewJobEvent>), HealthGateError> {
        let attempt = ledger
            .active_attempt(&job.job_id)
            .map_err(HealthGateError::Ledger)?;
        let event_base = u64::from(attempt) * 1_000_000;
        let deadline = Instant::now() + Duration::from_millis(policy.timeout_ms);
        let mut events = Vec::new();
        let mut probe = 0_u32;
        let mut last_observation = None;

        loop {
            probe = probe.saturating_add(1);
            let step_index = first_step.saturating_add(probe - 1);
            let inspected = match self
                .health_probe_step(
                    ledger,
                    job,
                    step_index,
                    probe,
                    container_id,
                    deadline,
                    &mut cancellation,
                )
                .await
            {
                Ok(instance) => instance,
                Err(HealthProbeError::Ledger(error)) => {
                    return Err(HealthGateError::Ledger(error));
                }
                Err(HealthProbeError::Cancelled) => {
                    let message = format!("{action} health wait was cancelled");
                    push_bounded_health_event(
                        &mut events,
                        health_control_event(
                            event_base + u64::from(probe),
                            "cancelled",
                            "WARN",
                            &message,
                            probe,
                            last_observation.as_ref(),
                        ),
                    );
                    return Err(HealthGateError::Failed {
                        outcome: ExecutionOutcome {
                            status: CompletionStatus::Cancelled,
                            result: json!({
                                "health_gate": "cancelled",
                                "probe_count": probe,
                                "last_health_observation": last_observation,
                            }),
                            error_message: message,
                            events,
                        },
                        compensation_step: step_index.saturating_add(1),
                    });
                }
                Err(HealthProbeError::TimedOut) => {
                    let message = format!(
                        "{action} container did not satisfy the health gate within {}ms",
                        policy.timeout_ms,
                    );
                    push_bounded_health_event(
                        &mut events,
                        health_control_event(
                            event_base + u64::from(probe),
                            "timeout",
                            "ERROR",
                            &message,
                            probe,
                            last_observation.as_ref(),
                        ),
                    );
                    return Err(HealthGateError::Failed {
                        outcome: retryable_health_failure(
                            &message,
                            probe,
                            last_observation,
                            events,
                        ),
                        compensation_step: step_index.saturating_add(1),
                    });
                }
                Err(HealthProbeError::Runtime(outcome)) => {
                    push_bounded_health_event(
                        &mut events,
                        health_control_event(
                            event_base + u64::from(probe),
                            "probe_error",
                            "ERROR",
                            "container health inspection failed",
                            probe,
                            last_observation.as_ref(),
                        ),
                    );
                    let result = with_last_health_observation(outcome.result, last_observation);
                    return Err(HealthGateError::Failed {
                        outcome: ExecutionOutcome {
                            result,
                            events,
                            ..outcome
                        },
                        compensation_step: step_index.saturating_add(1),
                    });
                }
            };

            let decision = evaluate_health_gate(&inspected, policy);
            let observation = bounded_health_observation(probe, &inspected, &decision);
            push_bounded_health_event(
                &mut events,
                health_probe_event(
                    event_base + u64::from(probe),
                    probe,
                    &observation,
                    &decision,
                ),
            );
            match decision {
                HealthGateDecision::Ready => return Ok((inspected, events)),
                HealthGateDecision::Failed(_) => {
                    let missing_healthcheck = inspected.health.eq_ignore_ascii_case("NONE");
                    let status = if missing_healthcheck {
                        CompletionStatus::Failed
                    } else {
                        CompletionStatus::RetryableFailure
                    };
                    let last_probe_reason = observation.probe_reason.clone();
                    return Err(HealthGateError::Failed {
                        outcome: ExecutionOutcome {
                            status,
                            result: json!({
                                "health_gate": "failed",
                                "probe_count": probe,
                                "last_health_observation": observation,
                                "last_probe_reason": last_probe_reason.clone(),
                            }),
                            error_message: last_probe_reason,
                            events,
                        },
                        compensation_step: step_index.saturating_add(1),
                    });
                }
                HealthGateDecision::Pending(_) => {
                    last_observation = Some(observation);
                }
            }

            let wake_at =
                (Instant::now() + Duration::from_millis(policy.poll_interval_ms)).min(deadline);
            tokio::select! {
                _ = cancellation_signal(&mut cancellation) => {
                    let message = format!("{action} health wait was cancelled");
                    push_bounded_health_event(&mut events, health_control_event(
                        event_base + u64::from(probe).saturating_add(1),
                        "cancelled",
                        "WARN",
                        &message,
                        probe,
                        last_observation.as_ref(),
                    ));
                    return Err(HealthGateError::Failed {
                        outcome: ExecutionOutcome {
                            status: CompletionStatus::Cancelled,
                            result: json!({
                                "health_gate": "cancelled",
                                "probe_count": probe,
                                "last_health_observation": last_observation,
                            }),
                            error_message: message,
                            events,
                        },
                        compensation_step: step_index.saturating_add(1),
                    });
                }
                _ = tokio::time::sleep_until(wake_at) => {
                    if Instant::now() >= deadline {
                        let message = format!(
                            "{action} container did not satisfy the health gate within {}ms",
                            policy.timeout_ms,
                        );
                        push_bounded_health_event(&mut events, health_control_event(
                            event_base + u64::from(probe).saturating_add(1),
                            "timeout",
                            "ERROR",
                            &message,
                            probe,
                            last_observation.as_ref(),
                        ));
                        return Err(HealthGateError::Failed {
                            outcome: retryable_health_failure(
                                &message,
                                probe,
                                last_observation,
                                events,
                            ),
                            compensation_step: step_index.saturating_add(1),
                        });
                    }
                }
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    async fn health_probe_step(
        &self,
        ledger: &mut AgentLedger,
        job: &LeasedJob,
        step_index: u32,
        probe: u32,
        container_id: &str,
        deadline: Instant,
        cancellation: &mut watch::Receiver<bool>,
    ) -> Result<RuntimeInstance, HealthProbeError> {
        let step_name = format!("health_probe_{probe}");
        ledger
            .step_started(&job.job_id, step_index, &step_name, crate::now_ms())
            .map_err(HealthProbeError::Ledger)?;
        let result = tokio::select! {
            _ = cancellation_signal(cancellation) => Err(HealthProbeError::Cancelled),
            _ = tokio::time::sleep_until(deadline) => Err(HealthProbeError::TimedOut),
            result = self.runtime.inspect_container(container_id) => {
                result
                    .map_err(|error| HealthProbeError::Runtime(runtime_error_outcome(&error, false)))
            }
        };
        match &result {
            Ok(instance) => {
                let output = serde_json::to_value(instance)
                    .map_err(|error| HealthProbeError::Ledger(LedgerError::Json(error)))?;
                ledger
                    .step_succeeded(&job.job_id, step_index, &output, crate::now_ms())
                    .map_err(HealthProbeError::Ledger)?;
            }
            Err(HealthProbeError::Cancelled) => ledger
                .step_failed(
                    &job.job_id,
                    step_index,
                    "health probe cancelled",
                    crate::now_ms(),
                )
                .map_err(HealthProbeError::Ledger)?,
            Err(HealthProbeError::TimedOut) => ledger
                .step_failed(
                    &job.job_id,
                    step_index,
                    "health probe exceeded the install health deadline",
                    crate::now_ms(),
                )
                .map_err(HealthProbeError::Ledger)?,
            Err(HealthProbeError::Runtime(outcome)) => ledger
                .step_failed(
                    &job.job_id,
                    step_index,
                    &outcome.error_message,
                    crate::now_ms(),
                )
                .map_err(HealthProbeError::Ledger)?,
            Err(HealthProbeError::Ledger(_)) => unreachable!("ledger errors are added above"),
        }
        result
    }

    #[allow(clippy::too_many_arguments)]
    async fn compensate_replacement_container(
        &self,
        job: &LeasedJob,
        ledger: &mut AgentLedger,
        step_index: u32,
        container_id: &str,
        original_error: StepError,
        compensation_timeout_ms: u64,
        action: &str,
        applied_migration: bool,
        runtime_context: Option<&MaterializedRuntimeContext>,
        payload: &ReleaseReplacementPayload,
        old_writer_state: ExclusiveOldWriterState,
    ) -> Result<ExecutionOutcome, LedgerError> {
        let mutation_result_unproven = matches!(
            &original_error,
            StepError::Runtime(outcome) if outcome.status == CompletionStatus::NeedsAttention
        );
        let mut outcome = self
            .compensate_uncommitted_container(
                job,
                ledger,
                step_index,
                container_id,
                original_error,
                compensation_timeout_ms,
                action,
                runtime_context,
            )
            .await?;
        if old_writer_state != ExclusiveOldWriterState::Unaffected {
            outcome = self
                .restore_exclusive_old_writer(
                    job,
                    ledger,
                    payload,
                    container_id,
                    old_writer_state,
                    outcome,
                )
                .await?;
            if mutation_result_unproven {
                outcome.status = CompletionStatus::NeedsAttention;
                outcome.error_message = format!(
                    "{}; a retained-volume writer mutation returned an unproven result and requires explicit reconciliation despite successful safety compensation",
                    outcome.error_message
                );
                if let Some(object) = outcome.result.as_object_mut() {
                    object.insert("mutation_result_unproven".to_string(), Value::Bool(true));
                    object.insert("manual_recovery_required".to_string(), Value::Bool(true));
                }
            }
        }
        if applied_migration {
            outcome.status = CompletionStatus::NeedsAttention;
            outcome.error_message = format!(
                "{}; a signed OCI migration was already applied, so the previous runtime may require reconciliation",
                outcome.error_message
            );
            if let Some(object) = outcome.result.as_object_mut() {
                object.insert("migration_applied".to_string(), Value::Bool(true));
            }
        }
        Ok(outcome)
    }

    async fn restore_exclusive_old_writer(
        &self,
        job: &LeasedJob,
        ledger: &mut AgentLedger,
        payload: &ReleaseReplacementPayload,
        candidate_container_id: &str,
        old_writer_state: ExclusiveOldWriterState,
        mut outcome: ExecutionOutcome,
    ) -> Result<ExecutionOutcome, LedgerError> {
        const INSPECT_STEP: u32 = 3_200_000;
        const START_STEP: u32 = 3_200_001;
        const HEALTH_STEP: u32 = 3_200_010;

        let candidate_absence_proven = outcome
            .result
            .get("compensated")
            .and_then(Value::as_bool)
            .unwrap_or(false)
            || outcome
                .result
                .get("container_compensated")
                .and_then(Value::as_bool)
                .unwrap_or(false);
        if !candidate_absence_proven {
            return Ok(exclusive_restore_needs_attention(
                outcome,
                payload,
                candidate_container_id,
                "the replacement container could not be proven absent; the old writer was not restarted to avoid two concurrent writers",
            ));
        }

        let deadline =
            Instant::now() + Duration::from_millis(payload.health_gate.compensation_timeout_ms);
        let mut must_start = old_writer_state == ExclusiveOldWriterState::StopProven;
        if old_writer_state == ExclusiveOldWriterState::StopUncertain {
            let inspected = self
                .runtime_step(
                    ledger,
                    job,
                    INSPECT_STEP,
                    "inspect_old_writer_after_uncertain_stop",
                    false,
                    bounded_runtime_call(
                        deadline,
                        "old writer inspection",
                        self.runtime.inspect_container(&payload.old_container_id),
                    ),
                )
                .await;
            match inspected {
                Ok(instance) => match evaluate_health_gate(&instance, &payload.health_gate) {
                    HealthGateDecision::Ready => {
                        annotate_exclusive_restore(
                            &mut outcome,
                            payload,
                            candidate_container_id,
                            "already_running",
                        );
                        return Ok(outcome);
                    }
                    HealthGateDecision::Pending(_)
                        if instance.observed_state == RuntimeObservedState::Running => {}
                    HealthGateDecision::Pending(_) => must_start = true,
                    HealthGateDecision::Failed(_)
                        if matches!(
                            instance.observed_state,
                            RuntimeObservedState::Created
                                | RuntimeObservedState::Exited
                                | RuntimeObservedState::Stopped
                        ) =>
                    {
                        must_start = true;
                    }
                    HealthGateDecision::Failed(reason) => {
                        return Ok(exclusive_restore_needs_attention(
                            outcome,
                            payload,
                            candidate_container_id,
                            format!(
                                "old writer inspection could not establish a restartable state: {reason}"
                            ),
                        ));
                    }
                },
                Err(StepError::Ledger(error)) => return Err(error),
                Err(StepError::Runtime(error)) => {
                    return Ok(exclusive_restore_needs_attention(
                        outcome,
                        payload,
                        candidate_container_id,
                        format!(
                            "old writer stop outcome and subsequent inspection were both unproven: {}",
                            error.error_message
                        ),
                    ));
                }
            }
        }

        let mut start_error = None;
        if must_start
            && let Err(error) = self
                .runtime_step(
                    ledger,
                    job,
                    START_STEP,
                    "restart_old_writer_after_replacement_failure",
                    true,
                    bounded_runtime_call(
                        deadline,
                        "old writer restart",
                        self.runtime.start_container(&payload.old_container_id),
                    ),
                )
                .await
        {
            match error {
                StepError::Ledger(error) => return Err(error),
                StepError::Runtime(error) => start_error = Some(error.error_message),
            }
        }

        let mut probe = 0_u32;
        loop {
            probe = probe.saturating_add(1);
            let inspected = self
                .runtime_step(
                    ledger,
                    job,
                    HEALTH_STEP.saturating_add(probe),
                    &format!("verify_restored_old_writer_{probe}"),
                    false,
                    bounded_runtime_call(
                        deadline,
                        "old writer health verification",
                        self.runtime.inspect_container(&payload.old_container_id),
                    ),
                )
                .await;
            let instance = match inspected {
                Ok(instance) => instance,
                Err(StepError::Ledger(error)) => return Err(error),
                Err(StepError::Runtime(error)) => {
                    let detail = start_error
                        .as_deref()
                        .map(|start| format!("restart response was unproven ({start}); "))
                        .unwrap_or_default();
                    return Ok(exclusive_restore_needs_attention(
                        outcome,
                        payload,
                        candidate_container_id,
                        format!(
                            "{detail}old writer health verification failed: {}",
                            error.error_message
                        ),
                    ));
                }
            };
            match evaluate_health_gate(&instance, &payload.health_gate) {
                HealthGateDecision::Ready => {
                    annotate_exclusive_restore(
                        &mut outcome,
                        payload,
                        candidate_container_id,
                        "restarted_and_healthy",
                    );
                    if let Some(start_error) = start_error.as_deref() {
                        outcome.status = CompletionStatus::NeedsAttention;
                        outcome.error_message = format!(
                            "{}; old writer {} is healthy but its restart response was unproven ({start_error}); candidate {} is proven absent and manual reconciliation is required",
                            outcome.error_message, payload.old_container_id, candidate_container_id,
                        );
                        if let Some(object) = outcome.result.as_object_mut() {
                            object
                                .insert("mutation_result_unproven".to_string(), Value::Bool(true));
                            object
                                .insert("manual_recovery_required".to_string(), Value::Bool(true));
                        }
                    }
                    return Ok(outcome);
                }
                HealthGateDecision::Failed(reason) => {
                    return Ok(exclusive_restore_needs_attention(
                        outcome,
                        payload,
                        candidate_container_id,
                        format!("old writer restart was not healthy: {reason}"),
                    ));
                }
                HealthGateDecision::Pending(reason) => {
                    if Instant::now() >= deadline {
                        return Ok(exclusive_restore_needs_attention(
                            outcome,
                            payload,
                            candidate_container_id,
                            format!("old writer restart health deadline elapsed: {reason}"),
                        ));
                    }
                    let wake_at = (Instant::now()
                        + Duration::from_millis(payload.health_gate.poll_interval_ms))
                    .min(deadline);
                    tokio::time::sleep_until(wake_at).await;
                }
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    async fn compensate_uncommitted_container(
        &self,
        job: &LeasedJob,
        ledger: &mut AgentLedger,
        step_index: u32,
        container_id: &str,
        original_error: StepError,
        compensation_timeout_ms: u64,
        action: &str,
        runtime_context: Option<&MaterializedRuntimeContext>,
    ) -> Result<ExecutionOutcome, LedgerError> {
        let mut original = match original_error {
            StepError::Ledger(error) => return Err(error),
            StepError::Runtime(outcome) => outcome,
        };
        match self
            .runtime_step(
                ledger,
                job,
                step_index,
                "compensate_remove_container",
                true,
                async {
                    match tokio::time::timeout(
                        Duration::from_millis(compensation_timeout_ms),
                        self.runtime.remove_container(container_id, true),
                    )
                    .await
                    {
                        Ok(result) => result,
                        Err(_) => Err(RuntimeError::EngineUnavailable(format!(
                            "container compensation exceeded {compensation_timeout_ms}ms"
                        ))),
                    }
                },
            )
            .await
        {
            Ok(()) => {
                if let Some(runtime_context) = runtime_context
                    && let Err(error) = self
                        .cleanup_runtime_context(
                            job,
                            ledger,
                            step_index.saturating_add(1),
                            &runtime_context.deployment_id,
                            &runtime_context.context,
                        )
                        .await
                {
                    return match error {
                        ContextCleanupError::Ledger(error) => Err(error),
                        ContextCleanupError::Policy(error) => Ok(ExecutionOutcome {
                            status: CompletionStatus::NeedsAttention,
                            result: json!({
                                "action": action,
                                "container_compensated": true,
                                "runtime_context_compensated": false,
                                "removed_container_id": container_id,
                            }),
                            error_message: format!(
                                "{action} container was removed but runtime context compensation failed: {error}"
                            ),
                            events: original.events,
                        }),
                    };
                }
                // Once the container is proven absent, retrying the deterministic
                // install is safe even when the original Docker response was
                // ambiguous.
                if original.status == CompletionStatus::NeedsAttention {
                    original.status = CompletionStatus::RetryableFailure;
                }
                let failure_result = original.result.clone();
                original.result = json!({
                    "action": action,
                    "compensated": true,
                    "removed_container_id": container_id,
                    "failure": failure_result,
                });
                Ok(original)
            }
            Err(StepError::Ledger(error)) => Err(error),
            Err(StepError::Runtime(compensation)) => Ok(ExecutionOutcome {
                status: CompletionStatus::NeedsAttention,
                result: json!({
                    "action": action,
                    "compensated": false,
                    "container_id": container_id,
                    "original_error": original.error_message,
                    "failure": original.result,
                    "compensation_error": compensation.error_message,
                }),
                error_message: format!(
                    "{action} failed and container compensation could not be proven: {}; compensation: {}",
                    original.error_message, compensation.error_message
                ),
                events: original.events,
            }),
        }
    }

    async fn start(
        &self,
        job: &LeasedJob,
        ledger: &mut AgentLedger,
    ) -> Result<ExecutionOutcome, LedgerError> {
        let payload: ContainerTarget = match decode_payload(job) {
            Ok(payload) => payload,
            Err(outcome) => return Ok(outcome),
        };
        if let Err(error) = self
            .runtime_step(
                ledger,
                job,
                1,
                "start_container",
                true,
                self.runtime.start_container(&payload.container_id),
            )
            .await
        {
            return step_result(error);
        }
        self.inspect_after_mutation(job, ledger, 2, &payload.container_id)
            .await
    }

    async fn stop(
        &self,
        job: &LeasedJob,
        ledger: &mut AgentLedger,
    ) -> Result<ExecutionOutcome, LedgerError> {
        let payload: TimedContainerTarget = match decode_payload(job) {
            Ok(payload) => payload,
            Err(outcome) => return Ok(outcome),
        };
        if let Err(error) = self
            .runtime_step(
                ledger,
                job,
                1,
                "stop_container",
                true,
                self.runtime
                    .stop_container(&payload.container_id, payload.timeout_seconds),
            )
            .await
        {
            return step_result(error);
        }
        self.inspect_after_mutation(job, ledger, 2, &payload.container_id)
            .await
    }

    async fn restart(
        &self,
        job: &LeasedJob,
        ledger: &mut AgentLedger,
    ) -> Result<ExecutionOutcome, LedgerError> {
        let payload: TimedContainerTarget = match decode_payload(job) {
            Ok(payload) => payload,
            Err(outcome) => return Ok(outcome),
        };
        if let Err(error) = self
            .runtime_step(
                ledger,
                job,
                1,
                "restart_container",
                true,
                self.runtime
                    .restart_container(&payload.container_id, payload.timeout_seconds),
            )
            .await
        {
            return step_result(error);
        }
        self.inspect_after_mutation(job, ledger, 2, &payload.container_id)
            .await
    }

    async fn uninstall(
        &self,
        job: &LeasedJob,
        ledger: &mut AgentLedger,
    ) -> Result<ExecutionOutcome, LedgerError> {
        let payload: RemoveContainerTarget = match decode_payload(job) {
            Ok(payload) => payload,
            Err(outcome) => return Ok(outcome),
        };
        let runtime_context = match ledger.runtime_context_for_container(&payload.container_id)? {
            Some(context) => Some(context),
            None if !payload.deployment_id.trim().is_empty() => {
                ledger.runtime_context_for_deployment(&payload.deployment_id)?
            }
            None => None,
        };

        let remove_step = if payload.force {
            1
        } else {
            let instance = match self
                .runtime_step(
                    ledger,
                    job,
                    1,
                    "inspect_container_before_remove",
                    false,
                    self.runtime.inspect_container(&payload.container_id),
                )
                .await
            {
                Ok(instance) => instance,
                Err(error) => return step_result(error),
            };

            if instance.observed_state == RuntimeObservedState::Running
                && let Err(error) = self
                    .runtime_step(
                        ledger,
                        job,
                        2,
                        "stop_container_before_remove",
                        true,
                        self.runtime
                            .stop_container(&payload.container_id, default_timeout_seconds()),
                    )
                    .await
            {
                return step_result(error);
            }
            3
        };

        if let Err(error) = self
            .runtime_step(
                ledger,
                job,
                remove_step,
                "remove_container",
                true,
                self.runtime
                    .remove_container(&payload.container_id, payload.force),
            )
            .await
        {
            return step_result(error);
        }
        if let Some(runtime_context) = runtime_context.as_ref()
            && let Err(error) = self
                .cleanup_runtime_context(
                    job,
                    ledger,
                    remove_step.saturating_add(1),
                    &runtime_context.deployment_id,
                    &runtime_context.context,
                )
                .await
        {
            return match error {
                ContextCleanupError::Ledger(error) => Err(error),
                ContextCleanupError::Policy(error) => Ok(ExecutionOutcome {
                    status: CompletionStatus::NeedsAttention,
                    result: json!({
                        "container_id": payload.container_id,
                        "removed": true,
                        "runtime_context_removed": false,
                    }),
                    error_message: format!(
                        "container was removed but Agent runtime context cleanup failed: {error}"
                    ),
                    events: vec![],
                }),
            };
        }
        if !payload.deployment_id.trim().is_empty()
            && let Some(manager) = self.resource_claims.as_ref()
        {
            match manager.release_deployment(&payload.deployment_id).await {
                Ok(claims) => {
                    if claims.iter().any(|release| {
                        (release.provider_released
                            && !matches!(
                                release.claim.status,
                                ResourceClaimStatusV1::Retained | ResourceClaimStatusV1::Deleted
                            ))
                            || (!release.provider_released
                                && release.claim.status != ResourceClaimStatusV1::Ready)
                    }) {
                        return Ok(needs_attention_outcome(
                            "container was removed but a ResourceClaim binding/provider state was not safe",
                        ));
                    }
                }
                Err(error) => {
                    return Ok(needs_attention_outcome(format!(
                        "container was removed but ResourceClaim RETAIN release failed: {error}"
                    )));
                }
            }
        }
        Ok(ExecutionOutcome::success(json!({
            "container_id": payload.container_id,
            "removed": true,
            "runtime_context_removed": runtime_context.is_some(),
        })))
    }

    async fn replace_release(
        &self,
        job: &LeasedJob,
        ledger: &mut AgentLedger,
        cancellation: watch::Receiver<bool>,
        action: &'static str,
    ) -> Result<ExecutionOutcome, LedgerError> {
        let mut payload: ReleaseReplacementPayload = match decode_payload(job) {
            Ok(payload) => payload,
            Err(outcome) => return Ok(outcome),
        };
        if let Err(message) = validate_replacement_payload(&payload) {
            return Ok(ExecutionOutcome::failed(message));
        }
        if *cancellation.borrow() {
            return Ok(replacement_cancelled(&payload, action, vec![]));
        }

        // Claims are reusable only when the old deployment already owns the
        // exact same stable identities. This gate runs before migration, image
        // pull, or any Docker mutation, so an upgrade can never silently
        // provision a second database.
        let mut replacement_resource_outputs = std::collections::BTreeMap::new();
        if !payload.resource_claims.is_empty() {
            let Some(manager) = self.resource_claims.as_ref() else {
                return Ok(ExecutionOutcome::failed(
                    "replacement carries ResourceClaims but this Agent has no resource provider configured",
                ));
            };
            let claims = match manager
                .reuse_for_replacement(&payload.old_deployment_id, &payload.resource_claims)
                .await
            {
                Ok(claims) => claims,
                Err(error) => {
                    return Ok(ExecutionOutcome::failed(format!(
                        "replacement ResourceClaim reuse rejected before migration/runtime: {error}"
                    )));
                }
            };
            for (step, claim) in payload.resource_claims.iter().zip(claims) {
                let output = claim.output_secret.as_ref().ok_or_else(|| {
                    LedgerError::InvalidState(format!(
                        "READY replacement claim {} omitted output reference",
                        step.claim_id
                    ))
                })?;
                let path = manager
                    .output_path(&output.reference)
                    .await
                    .map_err(|error| {
                        LedgerError::InvalidState(format!(
                            "resolve replacement output for {}: {error}",
                            step.claim_id
                        ))
                    })?;
                replacement_resource_outputs.insert(
                    step.resource_name.clone(),
                    (
                        step.output_path_environment.clone(),
                        output.reference.clone(),
                        path,
                    ),
                );
            }
        }

        if let Some(materialization) = payload.materialization.as_ref() {
            const REPLACEMENT_MATERIALIZE_STEP: u32 = 2_900_000;
            ledger.step_started(
                &job.job_id,
                REPLACEMENT_MATERIALIZE_STEP,
                "replacement_materialize_runtime_config_and_secrets",
                crate::now_ms(),
            )?;
            match self
                .pipeline_provider
                .materialize_runtime(materialization)
                .await
            {
                Ok(environment) => {
                    ledger.step_succeeded(
                        &job.job_id,
                        REPLACEMENT_MATERIALIZE_STEP,
                        &json!({
                            "environment_keys": environment
                                .iter()
                                .filter_map(|item| item.split_once('=').map(|(key, _)| key))
                                .collect::<Vec<_>>(),
                            "secret_values_persisted": false,
                        }),
                        crate::now_ms(),
                    )?;
                    payload.new_spec.environment = environment;
                }
                Err(error) => {
                    ledger.step_failed(
                        &job.job_id,
                        REPLACEMENT_MATERIALIZE_STEP,
                        &error.to_string(),
                        crate::now_ms(),
                    )?;
                    return Ok(replacement_context(
                        provider_error_outcome("replacement runtime materialization", &error),
                        &payload,
                        action,
                    ));
                }
            }
        }

        for (resource_name, (environment, _reference, path)) in &replacement_resource_outputs {
            let mount = ResourceSecretFileMount {
                resource_name: resource_name.clone(),
                host_source_path: strict_path_text(
                    path,
                    &format!("replacement resource output for {resource_name}"),
                )?,
            };
            let destination = mount
                .container_destination()
                .map_err(|error| LedgerError::InvalidState(error.to_string()))?;
            payload
                .new_spec
                .environment
                .push(format!("{environment}={destination}"));
            payload.new_spec.resource_secret_file_mounts.push(mount);
        }
        payload
            .new_spec
            .resource_secret_file_mounts
            .sort_by(|left, right| left.resource_name.cmp(&right.resource_name));

        let mut migration_results = Vec::with_capacity(payload.migrations.len());
        let mut applied_migration = false;
        for (index, migration) in payload.migrations.iter().enumerate() {
            if *cancellation.borrow() {
                return Ok(replacement_cancelled(&payload, action, vec![]));
            }
            let index = u32::try_from(index).unwrap_or(u32::MAX / 16);
            let base = 2_910_000_u32.saturating_add(index.saturating_mul(16));
            let mut migration = migration.clone();
            let mut migration_mounts = Vec::new();
            for resource_name in &migration.resource_claims {
                let (environment, _reference, path) = replacement_resource_outputs
                    .get(resource_name)
                    .expect("replacement payload validation resolved resource name");
                let mount = ResourceSecretFileMount {
                    resource_name: resource_name.clone(),
                    host_source_path: strict_path_text(
                        path,
                        &format!("replacement migration resource output for {resource_name}"),
                    )?,
                };
                let destination = mount
                    .container_destination()
                    .map_err(|error| LedgerError::InvalidState(error.to_string()))?;
                migration
                    .environment
                    .push(format!("{environment}={destination}"));
                migration_mounts.push(mount);
            }
            if migration.resource_claims.len() == 1 {
                let destination = migration_mounts[0]
                    .container_destination()
                    .map_err(|error| LedgerError::InvalidState(error.to_string()))?;
                migration
                    .environment
                    .push(format!("OJOS_RESOURCE_OUTPUT_FILE={destination}"));
            }
            migration_mounts.sort_by(|left, right| left.resource_name.cmp(&right.resource_name));
            match self
                .run_oci_migration(job, ledger, &migration, migration_mounts, base)
                .await
            {
                Ok(result) => {
                    applied_migration |= result
                        .get("status")
                        .and_then(Value::as_str)
                        .is_some_and(|status| status == "APPLIED");
                    migration_results.push(result);
                }
                Err(PipelineExecutionError::Ledger(error)) => return Err(error),
                Err(PipelineExecutionError::Outcome(outcome)) => {
                    return Ok(replacement_irreversible_context(
                        outcome,
                        &payload,
                        action,
                        applied_migration,
                    ));
                }
            }
        }

        if let Some(artifact) = payload.offline_oci_artifact.as_ref() {
            let Some(fetcher) = self.artifact_fetcher.as_ref() else {
                return Ok(replacement_irreversible_context(
                    ExecutionOutcome::failed(
                        "offline OCI artifact was assigned but no authenticated artifact fetcher is configured",
                    ),
                    &payload,
                    action,
                    applied_migration,
                ));
            };
            let downloaded = match fetcher.download(job, artifact).await {
                Ok(downloaded) => downloaded,
                Err(error) => {
                    return Ok(replacement_irreversible_context(
                        artifact_download_outcome(error),
                        &payload,
                        action,
                        applied_migration,
                    ));
                }
            };
            if let Err(error) = self
                .runtime_step(
                    ledger,
                    job,
                    1,
                    &format!("import_{action}_oci_archive"),
                    false,
                    self.runtime
                        .import_image_archive_path(downloaded.path(), &payload.new_spec.image),
                )
                .await
            {
                return replacement_step_result_with_migration(
                    error,
                    &payload,
                    action,
                    applied_migration,
                );
            }
        } else {
            let pull_step = format!("pull_{action}_image");
            if let Err(error) = self
                .runtime_step(
                    ledger,
                    job,
                    1,
                    &pull_step,
                    false,
                    self.runtime.pull_image(&payload.new_spec.image),
                )
                .await
            {
                return replacement_step_result_with_migration(
                    error,
                    &payload,
                    action,
                    applied_migration,
                );
            }
        }
        if let Err(error) = payload
            .new_spec
            .runtime_contract
            .validate_health_gate(&payload.health_gate)
        {
            return Ok(replacement_irreversible_context(
                runtime_error_outcome(&error, false),
                &payload,
                action,
                applied_migration,
            ));
        }
        let materialized = match self
            .prepare_runtime_context(job, ledger, &payload.new_spec)
            .await
        {
            Ok(context) => context,
            Err(ContextPreparationError::Ledger(error)) => return Err(error),
            Err(ContextPreparationError::Outcome(outcome)) => {
                return Ok(replacement_irreversible_context(
                    outcome,
                    &payload,
                    action,
                    applied_migration,
                ));
            }
        };
        if let Some(materialized) = materialized.as_ref() {
            payload.new_spec.runtime_context = Some(materialized.context.clone());
        }
        if *cancellation.borrow() {
            let mut outcome = replacement_irreversible_context(
                replacement_cancelled(&payload, action, vec![]),
                &payload,
                action,
                applied_migration,
            );
            if let Some(materialized) = materialized.as_ref()
                && let Err(error) = self
                    .cleanup_runtime_context(
                        job,
                        ledger,
                        800_002,
                        &payload.new_spec.deployment_id,
                        &materialized.context,
                    )
                    .await
            {
                match error {
                    ContextCleanupError::Ledger(error) => return Err(error),
                    ContextCleanupError::Policy(error) => {
                        outcome.status = CompletionStatus::NeedsAttention;
                        outcome.error_message = format!(
                            "{}; runtime context compensation failed: {error}",
                            outcome.error_message
                        );
                    }
                }
            }
            return Ok(outcome);
        }
        if materialized.is_some() {
            ledger.mark_runtime_context_creating(
                &payload.new_spec.deployment_id,
                &job.job_id,
                crate::now_ms(),
            )?;
        }

        let create_step = format!("create_{action}_container");
        let instance = match self
            .runtime_step(
                ledger,
                job,
                2,
                &create_step,
                true,
                self.runtime.create_container(&payload.new_spec),
            )
            .await
        {
            Ok(instance) => instance,
            Err(error) => {
                if let Some(materialized) = materialized.as_ref() {
                    match &error {
                        StepError::Runtime(outcome)
                            if outcome.status != CompletionStatus::NeedsAttention =>
                        {
                            if let Err(cleanup) = self
                                .cleanup_runtime_context(
                                    job,
                                    ledger,
                                    800_002,
                                    &payload.new_spec.deployment_id,
                                    &materialized.context,
                                )
                                .await
                            {
                                return match cleanup {
                                    ContextCleanupError::Ledger(error) => Err(error),
                                    ContextCleanupError::Policy(cleanup) => {
                                        let mut outcome = match error {
                                            StepError::Runtime(outcome) => outcome,
                                            StepError::Ledger(_) => unreachable!(),
                                        };
                                        outcome.status = CompletionStatus::NeedsAttention;
                                        outcome.error_message = format!(
                                            "{}; runtime context compensation failed: {cleanup}",
                                            outcome.error_message
                                        );
                                        Ok(replacement_irreversible_context(
                                            outcome,
                                            &payload,
                                            action,
                                            applied_migration,
                                        ))
                                    }
                                };
                            }
                        }
                        StepError::Runtime(outcome) => {
                            ledger.mark_runtime_context_needs_attention(
                                &payload.new_spec.deployment_id,
                                &outcome.error_message,
                                crate::now_ms(),
                            )?;
                        }
                        StepError::Ledger(_) => {}
                    }
                }
                return replacement_step_result_with_migration(
                    error,
                    &payload,
                    action,
                    applied_migration,
                );
            }
        };
        if materialized.is_some() {
            ledger.bind_runtime_context(
                &payload.new_spec.deployment_id,
                &job.job_id,
                &instance.container_id,
                crate::now_ms(),
            )?;
        }
        if instance.container_id == payload.old_container_id {
            return Ok(replacement_irreversible_context(
                ExecutionOutcome {
                    status: CompletionStatus::NeedsAttention,
                    result: json!({
                        "action": action,
                        "old_deployment_id": payload.old_deployment_id,
                        "old_container_id": payload.old_container_id,
                        "runtime_returned_replaced_container": true,
                    }),
                    error_message: format!(
                        "{action} runtime returned the existing container id for the new instance"
                    ),
                    events: vec![],
                },
                &payload,
                action,
                applied_migration,
            ));
        }

        let mut old_writer_state = ExclusiveOldWriterState::Unaffected;
        if payload.exclusive_retained_volume_cutover {
            if *cancellation.borrow() {
                let cancelled = replacement_cancelled(&payload, action, vec![]);
                return self
                    .compensate_replacement_container(
                        job,
                        ledger,
                        3_090_000,
                        &instance.container_id,
                        StepError::Runtime(cancelled),
                        payload.health_gate.compensation_timeout_ms,
                        action,
                        applied_migration,
                        materialized.as_ref(),
                        &payload,
                        old_writer_state,
                    )
                    .await;
            }
            ledger.step_started(
                &job.job_id,
                3_089_999,
                "record_exclusive_retained_volume_cutover_intent",
                crate::now_ms(),
            )?;
            ledger.step_succeeded(
                &job.job_id,
                3_089_999,
                &json!({
                    "old_deployment_id": payload.old_deployment_id,
                    "old_container_id": payload.old_container_id,
                    "candidate_deployment_id": payload.new_spec.deployment_id,
                    "candidate_container_id": instance.container_id,
                    "manual_recovery_evidence": "if this job is interrupted after the next step starts, inspect both named containers and prove exactly one healthy writer before taking action",
                    "secret_material_persisted": false,
                }),
                crate::now_ms(),
            )?;
            let stop_deadline =
                Instant::now() + Duration::from_millis(payload.health_gate.compensation_timeout_ms);
            match self
                .runtime_step(
                    ledger,
                    job,
                    3_090_001,
                    "stop_old_writer_for_exclusive_retained_volume_cutover",
                    true,
                    bounded_runtime_call(
                        stop_deadline,
                        "old writer stop",
                        self.runtime
                            .stop_container(&payload.old_container_id, default_timeout_seconds()),
                    ),
                )
                .await
            {
                Ok(()) => old_writer_state = ExclusiveOldWriterState::StopProven,
                Err(StepError::Ledger(error)) => return Err(error),
                Err(StepError::Runtime(stop_error)) => {
                    old_writer_state = ExclusiveOldWriterState::StopUncertain;
                    let mut outcome = replacement_context(stop_error, &payload, action);
                    outcome.status = CompletionStatus::NeedsAttention;
                    outcome.error_message = format!(
                        "{}; old writer stop result is unproven, so the candidate writer was not started",
                        outcome.error_message
                    );
                    return self
                        .compensate_replacement_container(
                            job,
                            ledger,
                            3_090_002,
                            &instance.container_id,
                            StepError::Runtime(outcome),
                            payload.health_gate.compensation_timeout_ms,
                            action,
                            applied_migration,
                            materialized.as_ref(),
                            &payload,
                            old_writer_state,
                        )
                        .await;
                }
            }
        }

        if *cancellation.borrow() {
            let cancelled = replacement_cancelled(&payload, action, vec![]);
            return self
                .compensate_replacement_container(
                    job,
                    ledger,
                    3,
                    &instance.container_id,
                    StepError::Runtime(cancelled),
                    payload.health_gate.compensation_timeout_ms,
                    action,
                    applied_migration,
                    materialized.as_ref(),
                    &payload,
                    old_writer_state,
                )
                .await;
        }

        let start_step = format!("start_{action}_container");
        if let Err(error) = self
            .runtime_step(
                ledger,
                job,
                3,
                &start_step,
                true,
                self.runtime.start_container(&instance.container_id),
            )
            .await
        {
            let error = contextualize_replacement_step(error, &payload, action)?;
            return self
                .compensate_replacement_container(
                    job,
                    ledger,
                    4,
                    &instance.container_id,
                    StepError::Runtime(error),
                    payload.health_gate.compensation_timeout_ms,
                    action,
                    applied_migration,
                    materialized.as_ref(),
                    &payload,
                    old_writer_state,
                )
                .await;
        }

        let (inspected, events) = match self
            .wait_for_container_health(
                job,
                ledger,
                4,
                &instance.container_id,
                &payload.health_gate,
                cancellation.clone(),
                action,
            )
            .await
        {
            Ok(ready) => ready,
            Err(HealthGateError::Ledger(error)) => return Err(error),
            Err(HealthGateError::Failed {
                outcome,
                compensation_step,
            }) => {
                let outcome = replacement_context(outcome, &payload, action);
                return self
                    .compensate_replacement_container(
                        job,
                        ledger,
                        compensation_step,
                        &instance.container_id,
                        StepError::Runtime(outcome),
                        payload.health_gate.compensation_timeout_ms,
                        action,
                        applied_migration,
                        materialized.as_ref(),
                        &payload,
                        old_writer_state,
                    )
                    .await;
            }
        };
        let remove_old_step = 4_u32.saturating_add(events.len() as u32);

        if let Err(message) = validate_replacement_instance(&payload, &instance, &inspected) {
            let outcome = replacement_context(
                ExecutionOutcome {
                    status: CompletionStatus::Failed,
                    result: json!({ "invalid_runtime_projection": true }),
                    error_message: message,
                    events,
                },
                &payload,
                action,
            );
            return self
                .compensate_replacement_container(
                    job,
                    ledger,
                    remove_old_step,
                    &instance.container_id,
                    StepError::Runtime(outcome),
                    payload.health_gate.compensation_timeout_ms,
                    action,
                    applied_migration,
                    materialized.as_ref(),
                    &payload,
                    old_writer_state,
                )
                .await;
        }

        let provider_components = if let Some(saga) = payload.provider_saga.as_ref() {
            match self
                .apply_replacement_provider_saga(job, ledger, saga)
                .await
            {
                Ok(applied) => applied,
                Err(PipelineExecutionError::Ledger(error)) => return Err(error),
                Err(PipelineExecutionError::Outcome(outcome)) => {
                    let outcome = replacement_irreversible_context(
                        outcome,
                        &payload,
                        action,
                        applied_migration,
                    );
                    return self
                        .compensate_replacement_container(
                            job,
                            ledger,
                            remove_old_step,
                            &instance.container_id,
                            StepError::Runtime(outcome),
                            payload.health_gate.compensation_timeout_ms,
                            action,
                            applied_migration,
                            materialized.as_ref(),
                            &payload,
                            old_writer_state,
                        )
                        .await;
                }
            }
        } else {
            Vec::new()
        };

        if *cancellation.borrow() {
            let mut cancelled = replacement_cancelled(&payload, action, events);
            if let Some(saga) = payload.provider_saga.as_ref() {
                let restore_errors = self
                    .rollback_replacement_provider_saga(job, ledger, saga, &provider_components)
                    .await
                    .map_err(|error| match error {
                        PipelineExecutionError::Ledger(error) => error,
                        PipelineExecutionError::Outcome(_) => LedgerError::InvalidState(
                            "provider rollback returned an unexpected outcome".to_string(),
                        ),
                    })?;
                if !restore_errors.is_empty() {
                    cancelled.status = CompletionStatus::NeedsAttention;
                    cancelled.error_message = format!(
                        "{}; previous provider revision restoration failed: {}",
                        cancelled.error_message,
                        restore_errors.join("; ")
                    );
                }
            }
            return self
                .compensate_replacement_container(
                    job,
                    ledger,
                    remove_old_step,
                    &instance.container_id,
                    StepError::Runtime(cancelled),
                    payload.health_gate.compensation_timeout_ms,
                    action,
                    applied_migration,
                    materialized.as_ref(),
                    &payload,
                    old_writer_state,
                )
                .await;
        }

        if payload.preserve_old_until_topology_cutover {
            if !payload.resource_claims.is_empty() {
                let claim_ids = payload
                    .resource_claims
                    .iter()
                    .map(|step| step.claim_id.clone())
                    .collect::<Vec<_>>();
                if let Some(manager) = self.resource_claims.as_ref()
                    && let Err(error) = manager
                        .bind_replacement(
                            &payload.old_deployment_id,
                            &payload.new_spec.deployment_id,
                            &claim_ids,
                        )
                        .await
                {
                    return Ok(needs_attention_outcome(format!(
                        "healthy replacement exists but ResourceClaim binding commit failed: {error}"
                    )));
                }
            }
            if let Some(materialized) = materialized.as_ref() {
                self.activate_runtime_context(job, ledger, &payload.new_spec, materialized)
                    .await?;
            }
            if payload.provider_saga.is_some() {
                ledger.set_provider_revision_state(
                    &job.job_id,
                    "COMMITTED",
                    None,
                    crate::now_ms(),
                )?;
            }
            return Ok(ExecutionOutcome::success_with_events(
                json!({
                    "action": action,
                    "instance": inspected,
                    "replaced_deployment_id": payload.old_deployment_id,
                    "replaced_container_id": payload.old_container_id,
                    "old_container_preserved": true,
                    "old_container_stopped": payload.exclusive_retained_volume_cutover,
                    "topology_cutover_pending": true,
                    "provider_revision_id": payload.provider_saga.as_ref().map(|saga| &saga.desired.revision_id),
                    "migrations": migration_results,
                }),
                events,
            ));
        }

        let old_runtime_context =
            ledger.runtime_context_for_container(&payload.old_container_id)?;
        let remove_old_name = format!("remove_replaced_container_for_{action}");
        if let Err(error) = self
            .runtime_step(
                ledger,
                job,
                remove_old_step,
                &remove_old_name,
                true,
                self.runtime
                    .remove_container(&payload.old_container_id, true),
            )
            .await
        {
            let mut outcome = match error {
                StepError::Ledger(error) => return Err(error),
                StepError::Runtime(outcome) => outcome,
            };
            outcome.status = CompletionStatus::NeedsAttention;
            let failure = outcome.result;
            outcome.result = json!({
                "action": action,
                "old_deployment_id": payload.old_deployment_id,
                "old_container_id": payload.old_container_id,
                "old_container_removal_unproven": true,
                "cutover_proven": false,
                "new_instance_preserved": true,
                "new_instance": inspected,
                "desired_provider_revision_id": payload.provider_saga.as_ref().map(|saga| &saga.desired.revision_id),
                "failure": failure,
            });
            outcome.events = events;
            if payload.provider_saga.is_some() {
                ledger.set_provider_revision_state(
                    &job.job_id,
                    "NEEDS_ATTENTION",
                    Some("old container removal outcome is unproven after provider cutover"),
                    crate::now_ms(),
                )?;
            }
            return Ok(outcome);
        }

        if let Some(materialized) = materialized.as_ref() {
            self.activate_runtime_context(job, ledger, &payload.new_spec, materialized)
                .await?;
        }
        if let Some(old_runtime_context) = old_runtime_context.as_ref()
            && let Err(error) = self
                .cleanup_runtime_context(
                    job,
                    ledger,
                    remove_old_step.saturating_add(1),
                    &old_runtime_context.deployment_id,
                    &old_runtime_context.context,
                )
                .await
        {
            return match error {
                ContextCleanupError::Ledger(error) => Err(error),
                ContextCleanupError::Policy(error) => Ok(ExecutionOutcome {
                    status: CompletionStatus::NeedsAttention,
                    result: json!({
                        "action": action,
                        "old_container_removed": true,
                        "old_runtime_context_removed": false,
                        "new_instance": inspected,
                    }),
                    error_message: format!(
                        "{action} cutover succeeded but old Agent runtime context cleanup failed: {error}"
                    ),
                    events,
                }),
            };
        }

        if payload.provider_saga.is_some() {
            ledger.set_provider_revision_state(&job.job_id, "COMMITTED", None, crate::now_ms())?;
        }

        if !payload.resource_claims.is_empty() {
            let claim_ids = payload
                .resource_claims
                .iter()
                .map(|step| step.claim_id.clone())
                .collect::<Vec<_>>();
            if let Some(manager) = self.resource_claims.as_ref() {
                manager
                    .bind_replacement(
                        &payload.old_deployment_id,
                        &payload.new_spec.deployment_id,
                        &claim_ids,
                    )
                    .await
                    .map_err(|error| {
                        LedgerError::InvalidState(format!(
                            "replacement ResourceClaim binding commit failed after runtime cutover: {error}"
                        ))
                    })?;
                // The old runtime has been removed. Removing only its binding
                // leaves the shared provider READY because the new binding now
                // exists; it must not run ordinary RETAIN compensation here.
                manager
                    .release_deployment(&payload.old_deployment_id)
                    .await
                    .map_err(|error| {
                        LedgerError::InvalidState(format!(
                            "remove old ResourceClaim binding after cutover: {error}"
                        ))
                    })?;
            }
        }

        let replacement = RuntimeReplacement {
            instance: inspected,
            replaced_deployment_id: payload.old_deployment_id,
            replaced_container_id: payload.old_container_id,
        };
        Ok(ExecutionOutcome::success_with_events(
            json!({
                "action": action,
                "instance": replacement.instance,
                "replaced_deployment_id": replacement.replaced_deployment_id,
                "replaced_container_id": replacement.replaced_container_id,
                "provider_revision_id": payload.provider_saga.as_ref().map(|saga| &saga.desired.revision_id),
                "migrations": migration_results,
            }),
            events,
        ))
    }

    async fn apply_replacement_provider_saga(
        &self,
        job: &LeasedJob,
        ledger: &mut AgentLedger,
        saga: &ReplacementProviderSaga,
    ) -> Result<Vec<String>, PipelineExecutionError> {
        const AUTH_STEP: u32 = 3_000_000;
        const PROVISIONER_BASE_STEP: u32 = 3_010_000;
        const GATEWAY_STEP: u32 = 3_020_000;
        let run = ledger.begin_provider_revision(
            &job.job_id,
            &saga.previous,
            &saga.desired,
            crate::now_ms(),
        )?;
        if run.state != "APPLYING" {
            return Err(PipelineExecutionError::Outcome(needs_attention_outcome(
                format!(
                    "provider revision saga {} is {}; explicit reconciliation is required",
                    saga.desired.revision_id, run.state
                ),
            )));
        }
        let mut applied = run.applied_components;

        if saga.previous.auth != saga.desired.auth && !component_applied(&applied, "auth") {
            let result = match (&saga.desired.auth, &saga.previous.auth) {
                (Some(desired), _) => {
                    self.provider_step(
                        ledger,
                        job,
                        AUTH_STEP,
                        "replacement_auth_apply",
                        self.pipeline_provider.apply_auth(desired),
                    )
                    .await
                }
                (None, Some(previous)) => {
                    self.provider_step(
                        ledger,
                        job,
                        AUTH_STEP,
                        "replacement_auth_remove",
                        self.pipeline_provider
                            .compensate_auth(&previous.service_name),
                    )
                    .await
                }
                (None, None) => Ok(()),
            };
            if let Err(error) = result {
                applied.push("auth".to_string());
                return self
                    .replacement_provider_failure(job, ledger, saga, applied, "auth", error)
                    .await;
            }
            ledger.mark_provider_component_applied(&job.job_id, "auth", crate::now_ms())?;
            applied.push("auth".to_string());
        }

        let provider_names = provider_names(&saga.previous, &saga.desired);
        for (index, provider_name) in provider_names.iter().enumerate() {
            let component = format!("provisioner:{provider_name}");
            let previous = provisioner(&saga.previous, provider_name);
            let desired = provisioner(&saga.desired, provider_name);
            if previous == desired || component_applied(&applied, &component) {
                continue;
            }
            let step_index = PROVISIONER_BASE_STEP.saturating_add(index as u32);
            let result = if let Some(desired) = desired {
                self.provider_step(
                    ledger,
                    job,
                    step_index,
                    &format!("replacement_{provider_name}_apply"),
                    self.pipeline_provider.apply_provisioner(desired),
                )
                .await
            } else if let Some(previous) = previous {
                self.provider_step(
                    ledger,
                    job,
                    step_index,
                    &format!("replacement_{provider_name}_remove"),
                    self.pipeline_provider.compensate_provisioner(previous),
                )
                .await
            } else {
                Ok(())
            };
            if let Err(error) = result {
                applied.push(component.clone());
                return self
                    .replacement_provider_failure(job, ledger, saga, applied, &component, error)
                    .await;
            }
            ledger.mark_provider_component_applied(&job.job_id, &component, crate::now_ms())?;
            applied.push(component);
        }

        if saga.previous.gateway != saga.desired.gateway && !component_applied(&applied, "gateway")
        {
            let result = if let Some(desired) = saga.desired.gateway.as_ref() {
                self.provider_step(
                    ledger,
                    job,
                    GATEWAY_STEP,
                    "replacement_gateway_publish",
                    self.pipeline_provider.publish_gateway(desired),
                )
                .await
            } else if let Some(previous) = saga.previous.gateway.as_ref() {
                let absent = absent_gateway_revision(previous, &saga.desired.revision_id, "apply");
                self.provider_step(
                    ledger,
                    job,
                    GATEWAY_STEP,
                    "replacement_gateway_remove",
                    self.pipeline_provider.publish_gateway(&absent),
                )
                .await
            } else {
                Ok(())
            };
            if let Err(error) = result {
                applied.push("gateway".to_string());
                return self
                    .replacement_provider_failure(job, ledger, saga, applied, "gateway", error)
                    .await;
            }
            ledger.mark_provider_component_applied(&job.job_id, "gateway", crate::now_ms())?;
            applied.push("gateway".to_string());
        }

        ledger.set_provider_revision_state(
            &job.job_id,
            "DESIRED_APPLIED",
            None,
            crate::now_ms(),
        )?;
        Ok(applied)
    }

    async fn replacement_provider_failure(
        &self,
        job: &LeasedJob,
        ledger: &mut AgentLedger,
        saga: &ReplacementProviderSaga,
        attempted: Vec<String>,
        action: &str,
        error: PipelineProviderError,
    ) -> Result<Vec<String>, PipelineExecutionError> {
        let mut outcome = provider_error_outcome(&format!("replacement {action}"), &error);
        let restore_errors = self
            .rollback_replacement_provider_saga(job, ledger, saga, &attempted)
            .await?;
        if restore_errors.is_empty() {
            if error.outcome_is_ambiguous() {
                outcome.status = CompletionStatus::Failed;
            }
        } else {
            outcome.status = CompletionStatus::NeedsAttention;
            outcome.error_message = format!(
                "{}; previous provider revision restoration failed: {}",
                outcome.error_message,
                restore_errors.join("; ")
            );
        }
        outcome.result = json!({
            "provider_revision_applied": false,
            "desired_revision_id": saga.desired.revision_id,
            "previous_revision_id": saga.previous.revision_id,
            "restored_previous_revision": restore_errors.is_empty(),
            "failure": outcome.result,
        });
        Err(PipelineExecutionError::Outcome(outcome))
    }

    async fn rollback_replacement_provider_saga(
        &self,
        job: &LeasedJob,
        ledger: &mut AgentLedger,
        saga: &ReplacementProviderSaga,
        attempted: &[String],
    ) -> Result<Vec<String>, PipelineExecutionError> {
        ledger.set_provider_revision_state(&job.job_id, "ROLLING_BACK", None, crate::now_ms())?;
        let mut errors = Vec::new();
        let mut unique = BTreeSet::new();
        let components = attempted
            .iter()
            .rev()
            .filter(|component| unique.insert((*component).clone()))
            .cloned()
            .collect::<Vec<_>>();
        for (index, component) in components.iter().enumerate() {
            let step_index = 3_100_000_u32.saturating_add((index as u32).saturating_mul(2));
            let result = if component == "gateway" {
                self.provider_step(
                    ledger,
                    job,
                    step_index,
                    "replacement_gateway_restore_previous",
                    self.pipeline_provider.restore_gateway(
                        saga.desired.gateway.as_ref(),
                        saga.previous.gateway.as_ref(),
                        &format!("{}:restore", saga.desired.revision_id),
                    ),
                )
                .await
            } else if component == "auth" {
                self.provider_step(
                    ledger,
                    job,
                    step_index,
                    "replacement_auth_restore_previous",
                    self.pipeline_provider
                        .restore_auth(saga.desired.auth.as_ref(), saga.previous.auth.as_ref()),
                )
                .await
            } else if let Some(provider_name) = component.strip_prefix("provisioner:") {
                self.provider_step(
                    ledger,
                    job,
                    step_index,
                    &format!("replacement_{provider_name}_restore_previous"),
                    self.pipeline_provider.restore_provisioner(
                        provisioner(&saga.desired, provider_name),
                        provisioner(&saga.previous, provider_name),
                    ),
                )
                .await
            } else {
                Err(PipelineProviderError::Rejected {
                    status: 422,
                    body: format!("unknown provider revision component {component}"),
                })
            };
            if let Err(error) = result {
                errors.push(format!("{component}: {error}"));
            }
        }
        ledger.set_provider_revision_state(
            &job.job_id,
            if errors.is_empty() {
                "ROLLED_BACK"
            } else {
                "NEEDS_ATTENTION"
            },
            (!errors.is_empty()).then(|| errors.join("; ")).as_deref(),
            crate::now_ms(),
        )?;
        Ok(errors)
    }

    async fn health(
        &self,
        job: &LeasedJob,
        ledger: &mut AgentLedger,
    ) -> Result<ExecutionOutcome, LedgerError> {
        let payload: ContainerTarget = match decode_payload(job) {
            Ok(payload) => payload,
            Err(outcome) => return Ok(outcome),
        };
        let instance = match self
            .runtime_step(
                ledger,
                job,
                1,
                "inspect_container",
                false,
                self.runtime.inspect_container(&payload.container_id),
            )
            .await
        {
            Ok(instance) => instance,
            Err(error) => return step_result(error),
        };
        Ok(ExecutionOutcome::success(json!({ "instance": instance })))
    }

    async fn inspect_after_mutation(
        &self,
        job: &LeasedJob,
        ledger: &mut AgentLedger,
        step_index: u32,
        container_id: &str,
    ) -> Result<ExecutionOutcome, LedgerError> {
        let instance = match self
            .runtime_step(
                ledger,
                job,
                step_index,
                "inspect_container",
                true,
                self.runtime.inspect_container(container_id),
            )
            .await
        {
            Ok(instance) => instance,
            Err(error) => return step_result(error),
        };
        Ok(ExecutionOutcome::success(json!({ "instance": instance })))
    }

    async fn runtime_step<T, F>(
        &self,
        ledger: &mut AgentLedger,
        job: &LeasedJob,
        step_index: u32,
        step_name: &str,
        ambiguous_after_request: bool,
        future: F,
    ) -> Result<T, StepError>
    where
        T: Serialize,
        F: Future<Output = Result<T, RuntimeError>>,
    {
        ledger
            .step_started(&job.job_id, step_index, step_name, crate::now_ms())
            .map_err(StepError::Ledger)?;
        match future.await {
            Ok(value) => {
                let output = serde_json::to_value(&value)
                    .map_err(|error| StepError::Ledger(LedgerError::Json(error)))?;
                ledger
                    .step_succeeded(&job.job_id, step_index, &output, crate::now_ms())
                    .map_err(StepError::Ledger)?;
                Ok(value)
            }
            Err(error) => {
                let outcome = runtime_error_outcome(&error, ambiguous_after_request);
                ledger
                    .step_failed(
                        &job.job_id,
                        step_index,
                        &outcome.error_message,
                        crate::now_ms(),
                    )
                    .map_err(StepError::Ledger)?;
                Err(StepError::Runtime(outcome))
            }
        }
    }
}

enum StepError {
    Ledger(LedgerError),
    Runtime(ExecutionOutcome),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ExclusiveOldWriterState {
    Unaffected,
    StopProven,
    StopUncertain,
}

enum ContextPreparationError {
    Ledger(LedgerError),
    Outcome(ExecutionOutcome),
}

impl From<LedgerError> for ContextPreparationError {
    fn from(value: LedgerError) -> Self {
        Self::Ledger(value)
    }
}

enum ContextCleanupError {
    Ledger(LedgerError),
    Policy(RuntimePolicyError),
}

enum PipelineExecutionError {
    Ledger(LedgerError),
    Outcome(ExecutionOutcome),
}

impl PipelineExecutionError {
    fn from_step(error: StepError) -> Self {
        match error {
            StepError::Ledger(error) => Self::Ledger(error),
            StepError::Runtime(outcome) => Self::Outcome(outcome),
        }
    }
}

impl From<LedgerError> for PipelineExecutionError {
    fn from(value: LedgerError) -> Self {
        Self::Ledger(value)
    }
}

enum HealthGateError {
    Ledger(LedgerError),
    Failed {
        outcome: ExecutionOutcome,
        compensation_step: u32,
    },
}

enum HealthProbeError {
    Ledger(LedgerError),
    Runtime(ExecutionOutcome),
    Cancelled,
    TimedOut,
}

fn validate_replacement_payload(payload: &ReleaseReplacementPayload) -> Result<(), String> {
    payload.validate().map_err(|error| error.to_string())?;
    if is_managed_service_contract_v2(&payload.new_spec)
        && payload
            .provider_saga
            .as_ref()
            .is_some_and(replacement_saga_has_control_plane_management)
    {
        return Err(
            "Service Contract v2 replacement management must execute on the control plane; the Node Agent refuses Auth, Gateway, and API Registry provider state"
                .to_string(),
        );
    }
    Ok(())
}

fn replacement_saga_has_control_plane_management(saga: &ReplacementProviderSaga) -> bool {
    [&saga.previous, &saga.desired]
        .into_iter()
        .any(provider_revision_has_control_plane_management)
}

fn provider_revision_has_control_plane_management(revision: &ReleaseProviderRevision) -> bool {
    revision.auth.is_some()
        || revision.gateway.is_some()
        || revision
            .provisioners
            .iter()
            .any(|step| matches!(step, TypedProvisionerStep::ApiRegistry { .. }))
}

fn component_applied(applied: &[String], component: &str) -> bool {
    applied.iter().any(|value| value == component)
}

fn provider_names(
    previous: &ReleaseProviderRevision,
    desired: &ReleaseProviderRevision,
) -> Vec<String> {
    previous
        .provisioners
        .iter()
        .chain(&desired.provisioners)
        .map(TypedProvisionerStep::provider_name)
        .map(str::to_string)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

fn provisioner<'a>(
    revision: &'a ReleaseProviderRevision,
    provider_name: &str,
) -> Option<&'a TypedProvisionerStep> {
    revision
        .provisioners
        .iter()
        .find(|step| step.provider_name() == provider_name)
}

fn absent_gateway_revision(
    previous: &orchestrator_runtime::GatewayPipelineStep,
    revision_id: &str,
    suffix: &str,
) -> orchestrator_runtime::GatewayPipelineStep {
    let mut absent = previous.clone();
    absent.operation_id = format!("{revision_id}:{suffix}:gateway-absent");
    absent.routes.clear();
    absent
}

fn validate_replacement_instance(
    payload: &ReleaseReplacementPayload,
    created: &RuntimeInstance,
    inspected: &RuntimeInstance,
) -> Result<(), String> {
    let expected_digest = payload.new_spec.image.to_string();
    if created.container_id.trim().is_empty()
        || inspected.container_id != created.container_id
        || inspected.deployment_id != payload.new_spec.deployment_id
        || inspected.service_id != payload.new_spec.service_id
        || inspected.artifact_digest != expected_digest
    {
        return Err(format!(
            "replacement runtime projection did not match new_spec: expected deployment={}, service={}, digest={}, container={}; observed deployment={}, service={}, digest={}, container={}",
            payload.new_spec.deployment_id,
            payload.new_spec.service_id,
            expected_digest,
            created.container_id,
            inspected.deployment_id,
            inspected.service_id,
            inspected.artifact_digest,
            inspected.container_id,
        ));
    }
    Ok(())
}

fn replacement_cancelled(
    payload: &ReleaseReplacementPayload,
    action: &str,
    events: Vec<NewJobEvent>,
) -> ExecutionOutcome {
    ExecutionOutcome {
        status: CompletionStatus::Cancelled,
        result: json!({
            "action": action,
            "old_deployment_id": payload.old_deployment_id,
            "old_container_id": payload.old_container_id,
            "old_instance_preserved": true,
            "cancelled_before_cutover": true,
        }),
        error_message: format!("{action} was cancelled before cutover"),
        events,
    }
}

fn replacement_context(
    mut outcome: ExecutionOutcome,
    payload: &ReleaseReplacementPayload,
    action: &str,
) -> ExecutionOutcome {
    let failure = outcome.result;
    outcome.result = json!({
        "action": action,
        "old_deployment_id": payload.old_deployment_id,
        "old_container_id": payload.old_container_id,
        "old_instance_preserved": true,
        "failure": failure,
    });
    outcome.error_message = format!(
        "{action} failed before cutover; old deployment {} was preserved: {}",
        payload.old_deployment_id, outcome.error_message
    );
    outcome
}

fn replacement_irreversible_context(
    outcome: ExecutionOutcome,
    payload: &ReleaseReplacementPayload,
    action: &str,
    applied_migration: bool,
) -> ExecutionOutcome {
    let mut outcome = replacement_context(outcome, payload, action);
    if applied_migration {
        outcome.status = CompletionStatus::NeedsAttention;
        outcome.error_message = format!(
            "{}; a signed OCI migration was already applied",
            outcome.error_message
        );
        if let Some(object) = outcome.result.as_object_mut() {
            object.insert("migration_applied".to_string(), Value::Bool(true));
        }
    }
    outcome
}

fn contextualize_replacement_step(
    error: StepError,
    payload: &ReleaseReplacementPayload,
    action: &str,
) -> Result<ExecutionOutcome, LedgerError> {
    match error {
        StepError::Ledger(error) => Err(error),
        StepError::Runtime(outcome) => Ok(replacement_context(outcome, payload, action)),
    }
}

fn replacement_step_result_with_migration(
    error: StepError,
    payload: &ReleaseReplacementPayload,
    action: &str,
    applied_migration: bool,
) -> Result<ExecutionOutcome, LedgerError> {
    match error {
        StepError::Ledger(error) => Err(error),
        StepError::Runtime(outcome) => Ok(replacement_irreversible_context(
            outcome,
            payload,
            action,
            applied_migration,
        )),
    }
}

async fn cancellation_signal(cancellation: &mut watch::Receiver<bool>) {
    loop {
        if *cancellation.borrow() {
            return;
        }
        if cancellation.changed().await.is_err() {
            std::future::pending::<()>().await;
        }
    }
}

const MAX_HEALTH_GATE_EVENTS: usize = 64;
const MAX_HEALTH_STATUS_CHARS: usize = 64;
const MAX_HEALTH_REASON_CHARS: usize = 256;

/// Credential-free evidence captured before an unhealthy container is
/// compensated. Deliberately exclude Docker log output, command lines,
/// environment variables, and mounts: health commands may echo secrets and
/// the durable Operation result must stay safe and bounded.
#[derive(Debug, Clone, Serialize)]
struct BoundedHealthObservation {
    probe: u32,
    observed_state: RuntimeObservedState,
    health: String,
    probe_reason: String,
}

fn retryable_health_failure(
    message: &str,
    probe_count: u32,
    last_observation: Option<BoundedHealthObservation>,
    events: Vec<NewJobEvent>,
) -> ExecutionOutcome {
    let last_probe_reason = last_observation
        .as_ref()
        .map(|observation| observation.probe_reason.clone())
        .unwrap_or_else(|| {
            "container inspection did not complete before the health deadline".to_string()
        });
    ExecutionOutcome {
        status: CompletionStatus::RetryableFailure,
        result: json!({
            "health_gate": "timeout",
            "probe_count": probe_count,
            "last_health_observation": last_observation,
            "last_probe_reason": last_probe_reason,
        }),
        error_message: message.to_string(),
        events,
    }
}

fn bounded_health_observation(
    probe: u32,
    instance: &RuntimeInstance,
    decision: &HealthGateDecision,
) -> BoundedHealthObservation {
    let health = safe_health_status(&instance.health);
    let probe_reason = match (decision, health) {
        (HealthGateDecision::Pending(_), "OTHER") => {
            "Docker returned an unrecognized health status"
        }
        (HealthGateDecision::Failed(_), "OTHER") => {
            "Docker returned an invalid terminal health status"
        }
        (HealthGateDecision::Ready, _) => "health gate satisfied",
        (HealthGateDecision::Pending(reason), _) | (HealthGateDecision::Failed(reason), _) => {
            reason
        }
    };
    BoundedHealthObservation {
        probe,
        observed_state: instance.observed_state.clone(),
        health: bounded_health_text(health, MAX_HEALTH_STATUS_CHARS),
        probe_reason: bounded_health_text(probe_reason, MAX_HEALTH_REASON_CHARS),
    }
}

fn safe_health_status(value: &str) -> &'static str {
    match value.trim().to_ascii_uppercase().as_str() {
        "HEALTHY" => "HEALTHY",
        "STARTING" => "STARTING",
        "UNHEALTHY" => "UNHEALTHY",
        "NONE" => "NONE",
        "UNKNOWN" | "" => "UNKNOWN",
        _ => "OTHER",
    }
}

fn bounded_health_text(value: &str, limit: usize) -> String {
    if value.chars().count() <= limit {
        return value.to_string();
    }
    let mut bounded = value
        .chars()
        .take(limit.saturating_sub(1))
        .collect::<String>();
    bounded.push('\u{2026}');
    bounded
}

fn with_last_health_observation(
    result: Value,
    last_observation: Option<BoundedHealthObservation>,
) -> Value {
    let mut result = match result {
        Value::Object(object) => object,
        Value::Null => serde_json::Map::new(),
        other => {
            let mut object = serde_json::Map::new();
            object.insert("runtime_failure".to_string(), other);
            object
        }
    };
    result.insert(
        "last_health_observation".to_string(),
        json!(last_observation),
    );
    Value::Object(result)
}

fn push_bounded_health_event(events: &mut Vec<NewJobEvent>, event: NewJobEvent) {
    if events.len() >= MAX_HEALTH_GATE_EVENTS {
        events.remove(0);
    }
    events.push(event);
}

fn health_probe_event(
    sequence: u64,
    probe: u32,
    observation: &BoundedHealthObservation,
    decision: &HealthGateDecision,
) -> NewJobEvent {
    let (decision_name, level) = match decision {
        HealthGateDecision::Ready => ("ready", "INFO"),
        HealthGateDecision::Pending(_) => ("pending", "INFO"),
        HealthGateDecision::Failed(_) => ("failed", "ERROR"),
    };
    NewJobEvent {
        sequence,
        event_type: "runtime.health_probe".to_string(),
        level: level.to_string(),
        message: format!(
            "health probe {probe}: {decision_name} ({})",
            observation.probe_reason
        ),
        data: json!({
            "probe": probe,
            "decision": decision_name,
            "reason": observation.probe_reason,
            "observed_state": observation.observed_state,
            "health": observation.health,
        }),
    }
}

fn health_control_event(
    sequence: u64,
    decision: &str,
    level: &str,
    message: &str,
    probe_count: u32,
    last_observation: Option<&BoundedHealthObservation>,
) -> NewJobEvent {
    NewJobEvent {
        sequence,
        event_type: "runtime.health_gate".to_string(),
        level: level.to_string(),
        message: message.to_string(),
        data: json!({
            "decision": decision,
            "probe_count": probe_count,
            "last_health_observation": last_observation,
        }),
    }
}

fn step_result(error: StepError) -> Result<ExecutionOutcome, LedgerError> {
    match error {
        StepError::Ledger(error) => Err(error),
        StepError::Runtime(outcome) => Ok(outcome),
    }
}

fn validate_pipeline_payload(payload: &ReleasePipelinePayload) -> Result<(), String> {
    let mut resource_names = BTreeSet::new();
    let mut output_environments = BTreeSet::new();
    for resource in &payload.resource_claims {
        resource.validate().map_err(|error| error.to_string())?;
        if resource.deployment_id != payload.install.spec.deployment_id
            || resource.service_id != payload.install.spec.service_id
        {
            return Err(
                "resource claim deployment_id/service_id must match install spec".to_string(),
            );
        }
        if !resource_names.insert(resource.resource_name.as_str())
            || !output_environments.insert(resource.output_path_environment.as_str())
        {
            return Err(
                "resource claim names and output environment keys must be unique".to_string(),
            );
        }
    }
    payload
        .install
        .health_gate
        .validate()
        .map_err(|error| error.to_string())?;
    if !payload.install.start && payload.gateway.is_some() {
        return Err("Gateway publication requires start=true and a healthy runtime".to_string());
    }
    let service_id = payload.install.spec.service_id.trim();
    if service_id.is_empty() {
        return Err("release pipeline install service_id is required".to_string());
    }
    if is_managed_service_contract_v2(&payload.install.spec)
        && (payload.auth.is_some()
            || payload.gateway.is_some()
            || payload
                .provisioners
                .iter()
                .any(|step| matches!(step, TypedProvisionerStep::ApiRegistry { .. })))
    {
        return Err(
            "Service Contract v2 management must execute on the control plane; the Node Agent refuses Auth, Gateway, and API Registry steps"
                .to_string(),
        );
    }
    if let Some(auth) = payload.auth.as_ref()
        && auth.service_name != service_id
    {
        return Err("auth provider service_name must match install service_id".to_string());
    }
    if let Some(gateway) = payload.gateway.as_ref()
        && (gateway.service_name != service_id
            || gateway.node_id.trim().is_empty()
            || gateway.operation_id.trim().is_empty())
    {
        return Err(
            "Gateway provider service_name must match install service_id and operation_id/node_id are required"
                .to_string(),
        );
    }
    let mut providers = std::collections::BTreeSet::new();
    for provisioner in &payload.provisioners {
        if provisioner.service_name() != service_id
            || !providers.insert(provisioner.provider_name())
        {
            return Err(
                "typed provider service_name must match install service_id and provider types must be unique"
                    .to_string(),
            );
        }
    }
    let mut versions = std::collections::BTreeSet::new();
    for migration in &payload.migrations {
        if migration.service_name != service_id
            || migration.version.trim().is_empty()
            || !versions.insert(migration.version.as_str())
        {
            return Err(
                "migration service_name must match install service_id and versions must be unique"
                    .to_string(),
            );
        }
        let mut migration_resources = BTreeSet::new();
        for resource in &migration.resource_claims {
            if !migration_resources.insert(resource.as_str())
                || !resource_names.contains(resource.as_str())
            {
                return Err(format!(
                    "migration {} has a duplicate or unresolved resource claim {resource}",
                    migration.version
                ));
            }
        }
    }
    Ok(())
}

fn is_managed_service_contract_v2(spec: &ContainerSpec) -> bool {
    spec.managed_service_context.is_some()
        || spec
            .labels
            .get("ojos.service_contract_version")
            .and_then(|value| value.parse::<u32>().ok())
            .is_some_and(|version| version >= 2)
}

fn is_sha256_checksum(value: &str) -> bool {
    value.len() == 71
        && value.starts_with("sha256:")
        && value[7..]
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn artifact_download_outcome(error: crate::TransportError) -> ExecutionOutcome {
    ExecutionOutcome {
        status: CompletionStatus::RetryableFailure,
        result: json!({"artifact_downloaded": false}),
        error_message: format!("download offline OCI artifact: {error}"),
        events: vec![],
    }
}

fn needs_attention_outcome(message: impl Into<String>) -> ExecutionOutcome {
    let message = message.into();
    ExecutionOutcome {
        status: CompletionStatus::NeedsAttention,
        result: json!({"pipeline_requires_reconciliation": true}),
        error_message: message,
        events: vec![],
    }
}

fn mark_resource_claim_compensation_evidence(outcome: &mut ExecutionOutcome) {
    let original = std::mem::replace(&mut outcome.result, Value::Null);
    outcome.result = json!({
        "failure": original,
        "resource_claim_compensation": {
            "deployment_binding_released": false,
            "provider_lifecycle": "RETAIN",
            "secret_material_persisted": false,
        },
    });
}

fn mark_resource_claim_compensation_unknown(outcome: &mut ExecutionOutcome, reason: &str) {
    outcome.status = CompletionStatus::NeedsAttention;
    outcome.error_message = format!(
        "{}; ResourceClaim RETAIN compensation could not be proven: {reason}",
        outcome.error_message
    );
    mark_resource_claim_compensation_evidence(outcome);
}

fn provider_error_outcome(action: &str, error: &PipelineProviderError) -> ExecutionOutcome {
    ExecutionOutcome {
        status: if error.outcome_is_ambiguous() {
            CompletionStatus::NeedsAttention
        } else {
            CompletionStatus::Failed
        },
        result: json!({
            "provider_action": action,
            "provider_error": error.to_string(),
        }),
        error_message: format!("{action} failed: {error}"),
        events: vec![],
    }
}

fn strict_path_text(path: &Path, purpose: &str) -> Result<String, LedgerError> {
    path.to_str()
        .map(str::to_owned)
        .ok_or_else(|| LedgerError::InvalidState(format!("{purpose} path is not valid UTF-8")))
}

fn pipeline_provider_failure(
    action: &str,
    error: PipelineProviderError,
    compensation_error: Option<PipelineProviderError>,
    irreversible_side_effect: bool,
) -> Result<ExecutionOutcome, LedgerError> {
    let mut outcome = provider_error_outcome(action, &error);
    if let Some(compensation_error) = compensation_error {
        outcome.status = CompletionStatus::NeedsAttention;
        outcome.error_message = format!(
            "{}; compensation failed: {compensation_error}",
            outcome.error_message
        );
    } else if error.outcome_is_ambiguous() {
        // A successful idempotent compensation establishes the desired absent
        // state even if the original request response was lost.
        outcome.status = CompletionStatus::Failed;
    }
    if irreversible_side_effect {
        outcome.status = CompletionStatus::NeedsAttention;
    }
    Ok(outcome)
}

fn append_compensation_errors(
    outcome: &mut ExecutionOutcome,
    resource_errors: Vec<String>,
    auth_error: Option<PipelineProviderError>,
) {
    if resource_errors.is_empty() && auth_error.is_none() {
        return;
    }
    outcome.status = CompletionStatus::NeedsAttention;
    if !resource_errors.is_empty() {
        outcome.error_message = format!(
            "{}; provider compensation failed: {}",
            outcome.error_message,
            resource_errors.join("; ")
        );
    }
    if let Some(error) = auth_error {
        outcome.error_message = format!(
            "{}; auth compensation failed: {error}",
            outcome.error_message
        );
    }
}

fn step_error_message(error: &StepError) -> String {
    match error {
        StepError::Ledger(error) => error.to_string(),
        StepError::Runtime(outcome) => outcome.error_message.clone(),
    }
}

fn runtime_error_outcome(error: &RuntimeError, ambiguous_after_request: bool) -> ExecutionOutcome {
    let status = match error {
        RuntimeError::InvalidImageReference(_)
        | RuntimeError::InvalidHealthPolicy(_)
        | RuntimeError::InvalidReleaseReplacement(_)
        | RuntimeError::InvalidPublishedEndpoint(_)
        | RuntimeError::InvalidRegistryCredentials(_)
        | RuntimeError::InvalidRuntimeContract(_)
        | RuntimeError::InvalidRuntimeContext(_)
        | RuntimeError::DigestMismatch { .. }
        | RuntimeError::MissingContainerId => CompletionStatus::Failed,
        RuntimeError::EngineUnavailable(_) | RuntimeError::Engine(_) if ambiguous_after_request => {
            CompletionStatus::NeedsAttention
        }
        RuntimeError::EngineUnavailable(_) | RuntimeError::Engine(_) => {
            CompletionStatus::RetryableFailure
        }
    };
    ExecutionOutcome {
        status,
        result: json!({ "runtime_error": error.to_string() }),
        error_message: error.to_string(),
        events: vec![],
    }
}

fn runtime_policy_outcome(error: &RuntimePolicyError) -> ExecutionOutcome {
    let status = match error {
        RuntimePolicyError::Credential(_) | RuntimePolicyError::Publication(_) => {
            CompletionStatus::RetryableFailure
        }
        RuntimePolicyError::Compensation(_) => CompletionStatus::NeedsAttention,
        RuntimePolicyError::InvalidPolicy(_)
        | RuntimePolicyError::ProfileNotAllowed(_)
        | RuntimePolicyError::UnsupportedRuntime { .. }
        | RuntimePolicyError::Materialization(_) => CompletionStatus::Failed,
    };
    ExecutionOutcome {
        status,
        result: json!({
            "runtime_context_error": error.to_string(),
            "credential_persisted_in_ledger": false,
        }),
        error_message: error.to_string(),
        events: vec![],
    }
}

fn decode_payload<T>(job: &LeasedJob) -> Result<T, ExecutionOutcome>
where
    T: for<'de> Deserialize<'de>,
{
    serde_json::from_value(job.payload.clone()).map_err(|error| {
        ExecutionOutcome::failed(format!("invalid {:?} payload: {error}", job.kind))
    })
}

type InstallPayload = RuntimeInstallPayload;

#[derive(Debug, Deserialize)]
struct ContainerTarget {
    container_id: String,
}

#[derive(Debug, Deserialize)]
struct TimedContainerTarget {
    container_id: String,
    #[serde(default = "default_timeout_seconds")]
    timeout_seconds: i32,
}

#[derive(Debug, Deserialize)]
struct RemoveContainerTarget {
    #[serde(default)]
    deployment_id: String,
    container_id: String,
    #[serde(default)]
    force: bool,
}

fn default_timeout_seconds() -> i32 {
    30
}

async fn bounded_runtime_call<T>(
    deadline: Instant,
    action: &str,
    future: impl Future<Output = Result<T, RuntimeError>>,
) -> Result<T, RuntimeError> {
    match tokio::time::timeout_at(deadline, future).await {
        Ok(result) => result,
        Err(_) => Err(RuntimeError::EngineUnavailable(format!(
            "{action} exceeded the retained-volume compensation deadline"
        ))),
    }
}

fn annotate_exclusive_restore(
    outcome: &mut ExecutionOutcome,
    payload: &ReleaseReplacementPayload,
    candidate_container_id: &str,
    evidence: &str,
) {
    let previous = outcome.result.clone();
    outcome.result = json!({
        "exclusive_retained_volume_cutover": true,
        "old_deployment_id": payload.old_deployment_id,
        "old_container_id": payload.old_container_id,
        "candidate_deployment_id": payload.new_spec.deployment_id,
        "candidate_container_id": candidate_container_id,
        "candidate_absence_proven": true,
        "old_writer_restored": true,
        "old_writer_restore_evidence": evidence,
        "failure": previous,
    });
}

fn exclusive_restore_needs_attention(
    mut outcome: ExecutionOutcome,
    payload: &ReleaseReplacementPayload,
    candidate_container_id: &str,
    reason: impl Into<String>,
) -> ExecutionOutcome {
    let reason = reason.into();
    let previous = outcome.result.clone();
    let original_error = outcome.error_message.clone();
    outcome.status = CompletionStatus::NeedsAttention;
    outcome.result = json!({
        "exclusive_retained_volume_cutover": true,
        "old_deployment_id": payload.old_deployment_id,
        "old_container_id": payload.old_container_id,
        "candidate_deployment_id": payload.new_spec.deployment_id,
        "candidate_container_id": candidate_container_id,
        "candidate_absence_proven": previous
            .get("compensated")
            .and_then(Value::as_bool)
            .unwrap_or(false)
            || previous
                .get("container_compensated")
                .and_then(Value::as_bool)
                .unwrap_or(false),
        "old_writer_restored": false,
        "manual_recovery_required": true,
        "manual_recovery_evidence": reason,
        "failure": previous,
    });
    outcome.error_message = format!(
        "{original_error}; retained-volume single-writer recovery for old container {} and candidate {} needs attention: {}",
        payload.old_container_id, candidate_container_id, reason
    );
    outcome
}
