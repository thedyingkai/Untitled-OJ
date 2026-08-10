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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ClaimResponse, HeartbeatAck, LeasedJob};
    use async_trait::async_trait;
    use orchestrator_control_plane::{CompleteRequest, HeartbeatRequest, JobKind, NewJobEvent};
    use orchestrator_runtime::{
        ContainerSpec, OciImageReference, RuntimeDesiredState, RuntimeError, RuntimeInstance,
        RuntimeObservedState,
    };
    use serde_json::json;
    use std::collections::VecDeque;
    use std::sync::{Arc, Mutex};

    #[derive(Default)]
    struct MockTransport {
        claims: Mutex<VecDeque<ClaimResponse>>,
        completions: Mutex<Vec<CompleteRequest>>,
    }

    #[async_trait]
    impl AgentTransport for Arc<MockTransport> {
        async fn claim(
            &self,
            _request: AgentClaimRequest,
        ) -> Result<ClaimResponse, TransportError> {
            Ok(self
                .claims
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or(ClaimResponse {
                    jobs: vec![],
                    retry_after_ms: 1_000,
                }))
        }

        async fn heartbeat(
            &self,
            _node_id: &str,
            _request: HeartbeatRequest,
        ) -> Result<HeartbeatAck, TransportError> {
            Ok(HeartbeatAck::default())
        }

        async fn complete(
            &self,
            _node_id: &str,
            request: CompleteRequest,
        ) -> Result<(), TransportError> {
            self.completions.lock().unwrap().push(request);
            Ok(())
        }
    }

    #[derive(Default)]
    struct CountingRuntime {
        starts: Mutex<u32>,
    }

    struct BlockingRuntime {
        entered: watch::Sender<bool>,
    }

    #[async_trait]
    impl ContainerRuntime for CountingRuntime {
        async fn pull_image(&self, _image: &OciImageReference) -> Result<(), RuntimeError> {
            Ok(())
        }

        async fn create_container(
            &self,
            _spec: &ContainerSpec,
        ) -> Result<RuntimeInstance, RuntimeError> {
            unreachable!()
        }

        async fn start_container(&self, _container_id: &str) -> Result<(), RuntimeError> {
            *self.starts.lock().unwrap() += 1;
            Ok(())
        }

        async fn stop_container(
            &self,
            _container_id: &str,
            _timeout_seconds: i32,
        ) -> Result<(), RuntimeError> {
            unreachable!()
        }

        async fn restart_container(
            &self,
            _container_id: &str,
            _timeout_seconds: i32,
        ) -> Result<(), RuntimeError> {
            unreachable!()
        }

        async fn remove_container(
            &self,
            _container_id: &str,
            _force: bool,
        ) -> Result<(), RuntimeError> {
            unreachable!()
        }

        async fn inspect_container(
            &self,
            container_id: &str,
        ) -> Result<RuntimeInstance, RuntimeError> {
            Ok(RuntimeInstance {
                deployment_id: "deployment-1".to_string(),
                service_id: "service-1".to_string(),
                release_version: "1.0.0".to_string(),
                container_id: container_id.to_string(),
                artifact_digest: "digest".to_string(),
                runtime_contract: orchestrator_runtime::RuntimeContract::standard_v1(),
                runtime_policy_sha256: String::new(),
                effective_runtime_sha256: String::new(),
                runtime_attested: true,
                desired_state: RuntimeDesiredState::Running,
                observed_state: RuntimeObservedState::Running,
                health: "HEALTHY".to_string(),
            })
        }
    }

    #[async_trait]
    impl ContainerRuntime for BlockingRuntime {
        async fn pull_image(&self, _image: &OciImageReference) -> Result<(), RuntimeError> {
            unreachable!()
        }

        async fn create_container(
            &self,
            _spec: &ContainerSpec,
        ) -> Result<RuntimeInstance, RuntimeError> {
            unreachable!()
        }

        async fn start_container(&self, _container_id: &str) -> Result<(), RuntimeError> {
            let _ = self.entered.send(true);
            std::future::pending().await
        }

        async fn stop_container(
            &self,
            _container_id: &str,
            _timeout_seconds: i32,
        ) -> Result<(), RuntimeError> {
            unreachable!()
        }

        async fn restart_container(
            &self,
            _container_id: &str,
            _timeout_seconds: i32,
        ) -> Result<(), RuntimeError> {
            unreachable!()
        }

        async fn remove_container(
            &self,
            _container_id: &str,
            _force: bool,
        ) -> Result<(), RuntimeError> {
            unreachable!()
        }

        async fn inspect_container(
            &self,
            _container_id: &str,
        ) -> Result<RuntimeInstance, RuntimeError> {
            unreachable!()
        }
    }

    fn config() -> WorkerConfig {
        WorkerConfig {
            node_id: "node-1".to_string(),
            instance_id: "instance-1".to_string(),
            heartbeat_ms: 10_000,
            lease_ms: 30_000,
            transport_retry_ms: 1_000,
        }
    }

    #[tokio::test]
    async fn completed_job_is_reported_again_without_runtime_reexecution() {
        let payload = json!({"container_id": "container-1"});
        let first = LeasedJob::new_for_test("job-1", JobKind::Start, payload.clone(), "lease-1");
        let second = LeasedJob::new_for_test("job-1", JobKind::Start, payload, "lease-2");
        let transport = Arc::new(MockTransport::default());
        transport.claims.lock().unwrap().extend([
            ClaimResponse {
                jobs: vec![first],
                retry_after_ms: 0,
            },
            ClaimResponse {
                jobs: vec![second],
                retry_after_ms: 0,
            },
        ]);
        let runtime = Arc::new(CountingRuntime::default());
        let executor = JobExecutor::from_shared(Arc::clone(&runtime));
        let mut worker = AgentWorker::new(
            config(),
            Arc::clone(&transport),
            executor,
            AgentLedger::open_in_memory().unwrap(),
        )
        .unwrap();

        assert!(matches!(
            worker.poll_once().await.unwrap(),
            PollOutcome::Completed {
                replayed: false,
                ..
            }
        ));
        assert!(matches!(
            worker.poll_once().await.unwrap(),
            PollOutcome::Completed { replayed: true, .. }
        ));
        assert_eq!(*runtime.starts.lock().unwrap(), 1);
        let completions = transport.completions.lock().unwrap();
        assert_eq!(completions.len(), 2);
        let observed_at_ms = completions[0].result["runtime_observed_at_ms"]
            .as_i64()
            .expect("successful runtime completion carries its Agent-clock watermark");
        assert!(observed_at_ms > 0);
        assert_eq!(
            completions[1].result["runtime_observed_at_ms"],
            json!(observed_at_ms),
            "a ledger replay must retain the original causal watermark"
        );
    }

    #[test]
    fn every_runtime_lifecycle_completion_gets_an_agent_clock_watermark() {
        for kind in [
            JobKind::Install,
            JobKind::ReleasePipeline,
            JobKind::Upgrade,
            JobKind::Start,
            JobKind::Stop,
            JobKind::Restart,
            JobKind::Rollback,
            JobKind::Uninstall,
            JobKind::Health,
        ] {
            let mut outcome = ExecutionOutcome {
                status: CompletionStatus::Succeeded,
                result: json!({}),
                error_message: String::new(),
                events: vec![],
            };
            attach_runtime_observation_watermark(&kind, &mut outcome);
            assert!(
                outcome.result["runtime_observed_at_ms"]
                    .as_i64()
                    .is_some_and(|value| value > 0),
                "{kind:?} omitted its Agent-clock watermark"
            );
        }
        let mut context = ExecutionOutcome {
            status: CompletionStatus::Succeeded,
            result: json!({}),
            error_message: String::new(),
            events: vec![],
        };
        attach_runtime_observation_watermark(&JobKind::BindingContextApply, &mut context);
        assert!(context.result.get("runtime_observed_at_ms").is_none());
    }

    #[tokio::test]
    async fn claimed_hash_mismatch_never_reaches_runtime_or_ledger() {
        let mut job = LeasedJob::new_for_test(
            "job-1",
            JobKind::Start,
            json!({"container_id": "container-1"}),
            "lease-1",
        );
        job.payload_sha256 = "wrong".to_string();
        let transport = Arc::new(MockTransport::default());
        transport.claims.lock().unwrap().push_back(ClaimResponse {
            jobs: vec![job],
            retry_after_ms: 0,
        });
        let runtime = Arc::new(CountingRuntime::default());
        let executor = JobExecutor::from_shared(Arc::clone(&runtime));
        let mut worker = AgentWorker::new(
            config(),
            Arc::clone(&transport),
            executor,
            AgentLedger::open_in_memory().unwrap(),
        )
        .unwrap();
        let outcome = worker.poll_once().await.unwrap();
        assert!(matches!(
            outcome,
            PollOutcome::Completed {
                status: CompletionStatus::NeedsAttention,
                ..
            }
        ));
        assert_eq!(*runtime.starts.lock().unwrap(), 0);
        assert!(worker.ledger().get("job-1").unwrap().is_none());
    }

    #[tokio::test]
    async fn persisted_health_events_are_included_in_completion() {
        let transport = Arc::new(MockTransport::default());
        let worker = AgentWorker::new(
            config(),
            Arc::clone(&transport),
            JobExecutor::new(CountingRuntime::default()),
            AgentLedger::open_in_memory().unwrap(),
        )
        .unwrap();
        let completion = StoredCompletion {
            status: CompletionStatus::Succeeded,
            result: json!({"instance": {"health": "HEALTHY"}}),
            error_message: String::new(),
            events: vec![NewJobEvent {
                sequence: 1_000_001,
                event_type: "runtime.health_probe".to_string(),
                level: "INFO".to_string(),
                message: "health probe ready".to_string(),
                data: json!({"decision": "ready"}),
            }],
        };

        worker
            .report_completion("job-health", "lease-health", &completion)
            .await
            .unwrap();

        let requests = transport.completions.lock().unwrap();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].events, completion.events);
    }

    #[tokio::test]
    async fn shutdown_stops_claiming_new_jobs() {
        let transport = Arc::new(MockTransport::default());
        transport.claims.lock().unwrap().push_back(ClaimResponse {
            jobs: vec![LeasedJob::new_for_test(
                "job-never-claimed",
                JobKind::Start,
                json!({"container_id": "container-1"}),
                "lease-1",
            )],
            retry_after_ms: 0,
        });
        let mut worker = AgentWorker::new(
            config(),
            Arc::clone(&transport),
            JobExecutor::new(CountingRuntime::default()),
            AgentLedger::open_in_memory().unwrap(),
        )
        .unwrap();
        let (_shutdown_tx, shutdown_rx) = watch::channel(true);

        worker.run_until_shutdown(shutdown_rx).await.unwrap();

        assert_eq!(transport.claims.lock().unwrap().len(), 1);
        assert!(transport.completions.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn shutdown_deadline_marks_unknown_runtime_outcome_for_attention() {
        let transport = Arc::new(MockTransport::default());
        transport.claims.lock().unwrap().push_back(ClaimResponse {
            jobs: vec![LeasedJob::new_for_test(
                "job-drain-timeout",
                JobKind::Start,
                json!({"container_id": "container-1"}),
                "lease-1",
            )],
            retry_after_ms: 0,
        });
        let (entered_tx, mut entered_rx) = watch::channel(false);
        let mut worker = AgentWorker::new(
            config(),
            Arc::clone(&transport),
            JobExecutor::new(BlockingRuntime {
                entered: entered_tx,
            }),
            AgentLedger::open_in_memory().unwrap(),
        )
        .unwrap();
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let run = worker.run_until_shutdown_with_timeout(shutdown_rx, Duration::from_millis(20));
        tokio::pin!(run);
        tokio::select! {
            result = &mut run => panic!("worker exited before runtime began: {result:?}"),
            changed = entered_rx.changed() => changed.unwrap(),
        }
        shutdown_tx.send(true).unwrap();
        tokio::time::timeout(Duration::from_secs(1), &mut run)
            .await
            .expect("worker must honor the drain deadline")
            .unwrap();

        let completions = transport.completions.lock().unwrap();
        assert_eq!(completions.len(), 1);
        assert_eq!(completions[0].status, CompletionStatus::NeedsAttention);
        assert_eq!(
            completions[0].result["worker_shutdown_timeout"],
            json!(true)
        );
    }
}
