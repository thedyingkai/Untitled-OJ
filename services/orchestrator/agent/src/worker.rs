use crate::{
    AgentClaimRequest, AgentLedger, AgentTransport, BeginDecision, ExecutionOutcome, JobExecutor,
    LedgerError, StoredCompletion, TransportError,
};
use orchestrator_control_plane::{
    CompleteRequest, CompletionStatus, HeartbeatRequest, canonical_payload_sha256,
};
use orchestrator_runtime::ContainerRuntime;
use std::time::Duration;
use thiserror::Error;
use tokio::sync::watch;
use tokio::time::{Instant, MissedTickBehavior};

const MAX_SERVER_RETRY_MS: u64 = 25_000;
const MIN_IDLE_RETRY_MS: u64 = 250;
const DEFAULT_DRAIN_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug, Clone)]
pub struct WorkerConfig {
    pub node_id: String,
    pub instance_id: String,
    pub heartbeat_ms: u64,
    pub lease_ms: i64,
    pub transport_retry_ms: u64,
}

impl WorkerConfig {
    pub fn validate(&self) -> Result<(), WorkerError> {
        if self.node_id.trim().is_empty() || self.instance_id.trim().is_empty() {
            return Err(WorkerError::Configuration(
                "node_id and instance_id are required".to_string(),
            ));
        }
        if self.heartbeat_ms == 0 || self.lease_ms <= 0 || self.transport_retry_ms == 0 {
            return Err(WorkerError::Configuration(
                "heartbeat, lease, and transport retry intervals must be positive".to_string(),
            ));
        }
        if self.heartbeat_ms as i64 >= self.lease_ms {
            return Err(WorkerError::Configuration(
                "heartbeat interval must be shorter than the lease duration".to_string(),
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PollOutcome {
    Idle {
        retry_after_ms: u64,
    },
    Completed {
        job_id: String,
        status: CompletionStatus,
        replayed: bool,
    },
}

#[derive(Debug, Error)]
pub enum WorkerError {
    #[error("invalid worker configuration: {0}")]
    Configuration(String),
    #[error(transparent)]
    Ledger(#[from] LedgerError),
    #[error(transparent)]
    Transport(#[from] TransportError),
    #[error("shutdown requested before a job was leased")]
    ShutdownRequested,
}

pub struct AgentWorker<T, R> {
    config: WorkerConfig,
    transport: T,
    executor: JobExecutor<R>,
    ledger: AgentLedger,
}

impl<T, R> AgentWorker<T, R>
where
    T: AgentTransport,
    R: ContainerRuntime,
{
    pub fn new(
        config: WorkerConfig,
        transport: T,
        executor: JobExecutor<R>,
        ledger: AgentLedger,
    ) -> Result<Self, WorkerError> {
        config.validate()?;
        Ok(Self {
            config,
            transport,
            executor,
            ledger,
        })
    }

    pub fn ledger(&self) -> &AgentLedger {
        &self.ledger
    }

    pub async fn poll_once(&mut self) -> Result<PollOutcome, WorkerError> {
        self.poll_once_interruptible(None, DEFAULT_DRAIN_TIMEOUT)
            .await
    }

    async fn poll_once_interruptible(
        &mut self,
        mut shutdown: Option<watch::Receiver<bool>>,
        drain_timeout: Duration,
    ) -> Result<PollOutcome, WorkerError> {
        if shutdown.as_ref().is_some_and(|signal| *signal.borrow()) {
            return Err(WorkerError::ShutdownRequested);
        }
        let claim_request = AgentClaimRequest {
            node_id: self.config.node_id.clone(),
            instance_id: self.config.instance_id.clone(),
        };
        let claim = self.transport.claim(claim_request);
        tokio::pin!(claim);
        let mut claim = if let Some(signal) = shutdown.as_mut() {
            loop {
                tokio::select! {
                    response = &mut claim => break response?,
                    changed = signal.changed() => {
                        if changed.is_err() || *signal.borrow() {
                            return Err(WorkerError::ShutdownRequested);
                        }
                    }
                }
            }
        } else {
            claim.await?
        };
        if claim.jobs.len() > 1 {
            return Err(TransportError::Protocol(format!(
                "claim returned {} jobs; expected at most one",
                claim.jobs.len()
            ))
            .into());
        }
        let Some(job) = claim.jobs.pop() else {
            return Ok(PollOutcome::Idle {
                retry_after_ms: claim
                    .retry_after_ms
                    .clamp(MIN_IDLE_RETRY_MS, MAX_SERVER_RETRY_MS),
            });
        };

        // If shutdown raced with the claim response, release the lease through
        // a retryable completion before any ledger or runtime side effect.
        if shutdown.as_ref().is_some_and(|signal| *signal.borrow()) {
            let completion = StoredCompletion {
                status: CompletionStatus::RetryableFailure,
                result: serde_json::json!({"worker_shutdown_before_execution": true}),
                error_message: "worker began draining before execution started".to_string(),
                events: vec![],
            };
            self.report_completion(&job.job_id, &job.lease_token, &completion)
                .await?;
            return Ok(PollOutcome::Completed {
                job_id: job.job_id,
                status: completion.status,
                replayed: false,
            });
        }

        let computed_hash = canonical_payload_sha256(&job.payload);
        if computed_hash != job.payload_sha256 {
            let completion = StoredCompletion {
                status: CompletionStatus::NeedsAttention,
                result: serde_json::json!({
                    "claimed_payload_sha256": job.payload_sha256,
                    "computed_payload_sha256": computed_hash,
                }),
                error_message: "claimed payload did not match payload_sha256".to_string(),
                events: vec![],
            };
            self.report_completion(&job.job_id, &job.lease_token, &completion)
                .await?;
            return Ok(PollOutcome::Completed {
                job_id: job.job_id,
                status: completion.status,
                replayed: false,
            });
        }

        let decision = match self.ledger.begin(
            &job.job_id,
            &job.kind,
            &job.payload_sha256,
            &job.lease_token,
            crate::now_ms(),
        ) {
            Ok(decision) => decision,
            Err(LedgerError::PayloadConflict { .. }) => {
                let completion = StoredCompletion {
                    status: CompletionStatus::NeedsAttention,
                    result: serde_json::json!({ "ledger_payload_conflict": true }),
                    error_message: format!(
                        "job {} conflicts with an existing local ledger entry",
                        job.job_id
                    ),
                    events: vec![],
                };
                self.report_completion(&job.job_id, &job.lease_token, &completion)
                    .await?;
                return Ok(PollOutcome::Completed {
                    job_id: job.job_id,
                    status: completion.status,
                    replayed: false,
                });
            }
            Err(error) => return Err(error.into()),
        };

        if let BeginDecision::Replay(completion) = decision {
            self.report_completion(&job.job_id, &job.lease_token, &completion)
                .await?;
            return Ok(PollOutcome::Completed {
                job_id: job.job_id,
                status: completion.status,
                replayed: true,
            });
        }

        let mut outcome = {
            let (cancel_sender, cancel_receiver) = watch::channel(false);
            let execution =
                self.executor
                    .execute_with_cancellation(&job, &mut self.ledger, cancel_receiver);
            tokio::pin!(execution);
            let heartbeat_period = Duration::from_millis(self.config.heartbeat_ms);
            let mut heartbeat =
                tokio::time::interval_at(Instant::now() + heartbeat_period, heartbeat_period);
            heartbeat.set_missed_tick_behavior(MissedTickBehavior::Delay);

            if let Some(mut shutdown) = shutdown {
                loop {
                    tokio::select! {
                        outcome = &mut execution => break outcome?,
                        _ = heartbeat.tick() => {
                            let request = HeartbeatRequest {
                                job_id: job.job_id.clone(),
                                lease_token: job.lease_token.clone(),
                                now_ms: crate::now_ms(),
                                lease_ms: self.config.lease_ms,
                                events: vec![],
                            };
                            match self.transport.heartbeat(&self.config.node_id, request).await {
                                Ok(ack) if ack.cancel_requested => {
                                    let _ = cancel_sender.send(true);
                                }
                                Ok(_) => {}
                                Err(error) => {
                                    break heartbeat_unknown_outcome(error);
                                }
                            }
                        }
                        changed = shutdown.changed() => {
                            if changed.is_ok() && !*shutdown.borrow() {
                                continue;
                            }
                            let _ = cancel_sender.send(true);
                            let drain = async {
                                loop {
                                    tokio::select! {
                                        outcome = &mut execution => break outcome,
                                        _ = heartbeat.tick() => {
                                            let request = HeartbeatRequest {
                                                job_id: job.job_id.clone(),
                                                lease_token: job.lease_token.clone(),
                                                now_ms: crate::now_ms(),
                                                lease_ms: self.config.lease_ms,
                                                events: vec![],
                                            };
                                            match self.transport.heartbeat(&self.config.node_id, request).await {
                                                Ok(_) => {}
                                                Err(error) => break Ok(heartbeat_unknown_outcome(error)),
                                            }
                                        }
                                    }
                                }
                            };
                            break match tokio::time::timeout(drain_timeout, drain).await {
                                Ok(Ok(mut outcome)) => {
                                    if outcome.status == CompletionStatus::Cancelled {
                                        outcome.status = CompletionStatus::RetryableFailure;
                                        outcome.error_message =
                                            "worker drained during shutdown; execution was cancelled and may be retried"
                                                .to_string();
                                    }
                                    outcome
                                }
                                Ok(Err(error)) => return Err(error.into()),
                                Err(_) => ExecutionOutcome {
                                    status: CompletionStatus::NeedsAttention,
                                    result: serde_json::json!({
                                        "worker_shutdown_timeout": true,
                                        "drain_timeout_ms": drain_timeout.as_millis(),
                                    }),
                                    error_message: format!(
                                        "worker could not prove the runtime outcome within the {} ms shutdown drain deadline",
                                        drain_timeout.as_millis()
                                    ),
                                    events: vec![],
                                },
                            };
                        }
                    }
                }
            } else {
                loop {
                    tokio::select! {
                        outcome = &mut execution => break outcome?,
                        _ = heartbeat.tick() => {
                            let request = HeartbeatRequest {
                                job_id: job.job_id.clone(),
                                lease_token: job.lease_token.clone(),
                                now_ms: crate::now_ms(),
                                lease_ms: self.config.lease_ms,
                                events: vec![],
                            };
                            match self.transport.heartbeat(&self.config.node_id, request).await {
                                Ok(ack) if ack.cancel_requested => {
                                    let _ = cancel_sender.send(true);
                                }
                                Ok(_) => {}
                                Err(error) => break heartbeat_unknown_outcome(error),
                            }
                        }
                    }
                }
            }
        };
        attach_runtime_observation_watermark(&job.kind, &mut outcome);
        let completion = StoredCompletion {
            status: outcome.status,
            result: outcome.result,
            error_message: outcome.error_message,
            events: outcome.events,
        };
        self.ledger
            .finish(&job.job_id, &completion, crate::now_ms())?;
        self.report_completion(&job.job_id, &job.lease_token, &completion)
            .await?;
        Ok(PollOutcome::Completed {
            job_id: job.job_id,
            status: completion.status,
            replayed: false,
        })
    }

    async fn report_completion(
        &self,
        job_id: &str,
        lease_token: &str,
        completion: &StoredCompletion,
    ) -> Result<(), TransportError> {
        self.transport
            .complete(
                &self.config.node_id,
                CompleteRequest {
                    job_id: job_id.to_string(),
                    lease_token: lease_token.to_string(),
                    status: completion.status.clone(),
                    result: completion.result.clone(),
                    error_message: completion.error_message.clone(),
                    now_ms: crate::now_ms(),
                    events: completion.events.clone(),
                },
            )
            .await
    }

    /// Runs until a shutdown value of `true` is observed. Transport outages are
    /// retried; ledger failures terminate the process because replay safety can
    /// no longer be guaranteed.
    pub async fn run_until_shutdown(
        &mut self,
        shutdown: watch::Receiver<bool>,
    ) -> Result<(), WorkerError> {
        self.run_until_shutdown_with_timeout(shutdown, DEFAULT_DRAIN_TIMEOUT)
            .await
    }

    async fn run_until_shutdown_with_timeout(
        &mut self,
        mut shutdown: watch::Receiver<bool>,
        drain_timeout: Duration,
    ) -> Result<(), WorkerError> {
        while !*shutdown.borrow() {
            let delay = match self
                .poll_once_interruptible(Some(shutdown.clone()), drain_timeout)
                .await
            {
                Ok(PollOutcome::Idle { retry_after_ms }) => Some(retry_after_ms),
                Ok(PollOutcome::Completed { .. }) => None,
                Err(WorkerError::Transport(_)) => Some(self.config.transport_retry_ms),
                Err(WorkerError::ShutdownRequested) => break,
                Err(error) => return Err(error),
            };
            let Some(delay) = delay else {
                continue;
            };
            tokio::select! {
                _ = tokio::time::sleep(Duration::from_millis(delay)) => {}
                changed = shutdown.changed() => {
                    if changed.is_err() || *shutdown.borrow() {
                        break;
                    }
                }
            }
        }
        Ok(())
    }
}

/// Stamps successful runtime lifecycle evidence with the Agent clock that is
/// also used by `NodeRuntimeFactsV1::observed_at_ms`.  The control plane uses
/// this causal watermark to distinguish a Docker inventory captured before a
/// lifecycle result from a genuinely newer inventory that proves drift.
///
/// The value is persisted in the local completion ledger before publication,
/// so a replay after an ambiguous HTTP outcome retains the exact same
/// watermark and completion fingerprint.
fn attach_runtime_observation_watermark(
    kind: &orchestrator_control_plane::JobKind,
    outcome: &mut ExecutionOutcome,
) {
    if outcome.status != CompletionStatus::Succeeded
        || !matches!(
            kind,
            orchestrator_control_plane::JobKind::Install
                | orchestrator_control_plane::JobKind::ReleasePipeline
                | orchestrator_control_plane::JobKind::Upgrade
                | orchestrator_control_plane::JobKind::Start
                | orchestrator_control_plane::JobKind::Stop
                | orchestrator_control_plane::JobKind::Restart
                | orchestrator_control_plane::JobKind::Rollback
                | orchestrator_control_plane::JobKind::Uninstall
                | orchestrator_control_plane::JobKind::Health
        )
    {
        return;
    }
    if let Some(result) = outcome.result.as_object_mut() {
        result.insert(
            "runtime_observed_at_ms".to_string(),
            serde_json::json!(crate::now_ms()),
        );
    }
}

fn heartbeat_unknown_outcome(error: TransportError) -> ExecutionOutcome {
    ExecutionOutcome {
        status: CompletionStatus::NeedsAttention,
        result: serde_json::json!({"heartbeat_error": error.to_string()}),
        error_message: format!("lease heartbeat failed while runtime outcome was unknown: {error}"),
        events: vec![],
    }
}
