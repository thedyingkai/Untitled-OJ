//! Background lease responsibilities.
use crate::durable::DurableStore;
use crate::topology_worker::context::now_ms;
use getrandom::fill as random_fill;
use orchestrator_control_plane::CompleteRequest;
use orchestrator_control_plane::CompletionStatus;
use orchestrator_control_plane::DEFAULT_LEASE_MS;
use orchestrator_control_plane::HeartbeatRequest;
use orchestrator_control_plane::JobError;
use orchestrator_control_plane::JobStore;
use orchestrator_control_plane::OperationCoordinator;
use serde_json::Value;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::AtomicI64;
use std::sync::atomic::Ordering;
use std::sync::mpsc;
use std::thread;
use std::thread::JoinHandle;
use std::time::Duration;

pub(super) const CONTROL_PLANE_HEARTBEAT_INTERVAL: Duration = Duration::from_secs(10);

pub(super) const CONTROL_PLANE_MAX_STALL_MS: i64 = 25_000;

pub(super) struct ControlPlaneLeaseHeartbeat {
    pub(super) stop: Option<mpsc::Sender<()>>,
    pub(super) handle: Option<JoinHandle<()>>,
    pub(super) state: Arc<ControlPlaneLeaseState>,
    pub(super) job_id: String,
    pub(super) lease_token: String,
}

pub(super) struct ControlPlaneLeaseState {
    pub(super) lost: AtomicBool,
    pub(super) last_progress_ms: AtomicI64,
    pub(super) lease_expires_at_ms: AtomicI64,
}

impl ControlPlaneLeaseHeartbeat {
    pub(super) fn start(
        storage: DurableStore,
        job_id: String,
        lease_token: String,
        lease_expires_at_ms: i64,
    ) -> Result<Self, String> {
        Self::start_with_timing(
            storage,
            job_id,
            lease_token,
            lease_expires_at_ms,
            CONTROL_PLANE_HEARTBEAT_INTERVAL,
            CONTROL_PLANE_MAX_STALL_MS,
        )
    }

    pub(super) fn start_with_timing(
        storage: DurableStore,
        job_id: String,
        lease_token: String,
        lease_expires_at_ms: i64,
        heartbeat_interval: Duration,
        max_stall_ms: i64,
    ) -> Result<Self, String> {
        if heartbeat_interval.is_zero() || max_stall_ms <= 0 {
            return Err("control-plane heartbeat timing must be positive".to_string());
        }
        let (stop, stopped) = mpsc::channel();
        let state = Arc::new(ControlPlaneLeaseState {
            lost: AtomicBool::new(false),
            last_progress_ms: AtomicI64::new(now_ms()),
            lease_expires_at_ms: AtomicI64::new(lease_expires_at_ms),
        });
        let heartbeat_state = Arc::clone(&state);
        let heartbeat_job_id = job_id.clone();
        let heartbeat_lease_token = lease_token.clone();
        let handle = thread::Builder::new()
            .name("orchestrator-control-plane-heartbeat".to_string())
            .spawn(move || {
                let mut delay = heartbeat_interval;
                loop {
                    match stopped.recv_timeout(delay) {
                        Ok(()) | Err(mpsc::RecvTimeoutError::Disconnected) => break,
                        Err(mpsc::RecvTimeoutError::Timeout) => {
                            let heartbeat_at = now_ms();
                            if heartbeat_at.saturating_sub(
                                heartbeat_state.last_progress_ms.load(Ordering::Acquire),
                            ) >= max_stall_ms
                            {
                                heartbeat_state.lost.store(true, Ordering::Release);
                                eprintln!(
                                    "control-plane Job {heartbeat_job_id} stopped heartbeating after {max_stall_ms}ms without progress"
                                );
                                break;
                            }
                            let mut jobs = storage.job_store();
                            match jobs.heartbeat(HeartbeatRequest {
                                job_id: heartbeat_job_id.clone(),
                                lease_token: heartbeat_lease_token.clone(),
                                now_ms: heartbeat_at,
                                lease_ms: DEFAULT_LEASE_MS,
                                events: Vec::new(),
                            }) {
                                Ok(job) => {
                                    heartbeat_state.lease_expires_at_ms.store(
                                        job.lease_expires_at_ms.unwrap_or(heartbeat_at),
                                        Ordering::Release,
                                    );
                                    delay = heartbeat_interval;
                                }
                                Err(JobError::StaleLease) => {
                                    heartbeat_state.lost.store(true, Ordering::Release);
                                    eprintln!(
                                        "control-plane Job {heartbeat_job_id} heartbeat lost its lease"
                                    );
                                    break;
                                }
                                Err(error) => {
                                    eprintln!(
                                        "control-plane Job {heartbeat_job_id} heartbeat error: {error}"
                                    );
                                    if heartbeat_at
                                        >= heartbeat_state
                                            .lease_expires_at_ms
                                            .load(Ordering::Acquire)
                                    {
                                        heartbeat_state.lost.store(true, Ordering::Release);
                                        break;
                                    }
                                    delay = Duration::from_secs(1);
                                }
                            }
                        }
                    }
                }
            })
            .map_err(|error| format!("spawn control-plane Job heartbeat: {error}"))?;
        Ok(Self {
            stop: Some(stop),
            handle: Some(handle),
            state,
            job_id,
            lease_token,
        })
    }

    pub(super) fn checkpoint(
        &self,
        jobs: &mut crate::durable::DurableJobStore,
    ) -> Result<(), String> {
        if self.state.lost.load(Ordering::Acquire) {
            return Err(format!(
                "control-plane Job {} lost its execution lease",
                self.job_id
            ));
        }
        let heartbeat_at = now_ms();
        match jobs.heartbeat(HeartbeatRequest {
            job_id: self.job_id.clone(),
            lease_token: self.lease_token.clone(),
            now_ms: heartbeat_at,
            lease_ms: DEFAULT_LEASE_MS,
            events: Vec::new(),
        }) {
            Ok(job) => {
                self.state.lease_expires_at_ms.store(
                    job.lease_expires_at_ms.unwrap_or(heartbeat_at),
                    Ordering::Release,
                );
                self.state
                    .last_progress_ms
                    .store(heartbeat_at, Ordering::Release);
                Ok(())
            }
            Err(error) => {
                if matches!(error, JobError::StaleLease)
                    || heartbeat_at >= self.state.lease_expires_at_ms.load(Ordering::Acquire)
                {
                    self.state.lost.store(true, Ordering::Release);
                }
                Err(format!(
                    "control-plane Job {} lease checkpoint failed: {error}",
                    self.job_id
                ))
            }
        }
    }
}

impl Drop for ControlPlaneLeaseHeartbeat {
    fn drop(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) fn complete_and_project(
    storage: &DurableStore,
    jobs: &mut crate::durable::DurableJobStore,
    job_id: &str,
    operation_id: &str,
    lease_token: String,
    status: CompletionStatus,
    result: Value,
    error_message: String,
) -> Result<(), String> {
    jobs.complete(CompleteRequest {
        job_id: job_id.to_string(),
        lease_token,
        status,
        result,
        error_message,
        now_ms: now_ms(),
        events: Vec::new(),
    })
    .map_err(|error| error.to_string())?;
    let mut operations = storage.operation_store();
    OperationCoordinator::new(&mut operations, jobs)
        .project(operation_id, now_ms())
        .map_err(|error| error.to_string())?;
    Ok(())
}

pub(super) fn lease_token() -> Result<String, String> {
    let mut bytes = [0_u8; 32];
    random_fill(&mut bytes).map_err(|_| "generate topology worker lease token".to_string())?;
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}
