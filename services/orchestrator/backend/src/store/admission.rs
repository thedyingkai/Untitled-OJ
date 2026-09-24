//! Shared mutation admission lock and durable Operation/Job publication. Keep the lock across final conflict checks and publication.
use crate::durable::DurableStore;
use crate::store::context::{now_marker, now_ms};
use crate::store::error::{StoreError, operation_error, storage_error};
use orchestrator_control_plane::DurableOperation;
use orchestrator_control_plane::OperationCoordinator;
use orchestrator_control_plane::PlanOperation;
use std::sync::{Mutex, MutexGuard};

static STORE_PLAN_LOCK: Mutex<()> = Mutex::new(());

/// Admission remains alive until the caller has constructed its response. This
/// keeps the established lock scope across conflict checks and durable publication.
pub(crate) struct StoreAdmission<'a> {
    storage: &'a DurableStore,
    _guard: MutexGuard<'static, ()>,
}

/// An apply reservation contains identity only; admission does not plan topology.
pub(crate) struct TopologyReservation<'a> {
    pub(crate) topology_id: &'a str,
    pub(crate) revision_id: &'a str,
}

impl<'a> StoreAdmission<'a> {
    pub(crate) fn acquire(storage: &'a DurableStore) -> Result<Self, StoreError> {
        let guard = STORE_PLAN_LOCK.lock().map_err(|_| {
            StoreError::new(
                503,
                "STORE_PLANNER_UNAVAILABLE",
                "Store planner coordination lock is poisoned",
            )
        })?;
        Ok(Self {
            storage,
            _guard: guard,
        })
    }

    /// Reserve in plan order and compensate in reverse order if reservation or
    /// Operation publication fails. These remain recoverable writes, not a new
    /// cross-repository transaction; OperationCoordinator owns enqueue recovery.
    pub(crate) fn enqueue<'t>(
        &self,
        plan: PlanOperation,
        topologies: impl IntoIterator<Item = TopologyReservation<'t>>,
    ) -> Result<DurableOperation, StoreError> {
        let operation_id = plan.operation_id.clone();
        let mut begun = Vec::new();
        for topology in topologies {
            if let Err(error) = self.storage.begin_topology_apply(
                topology.topology_id,
                topology.revision_id,
                &operation_id,
                &now_marker(),
            ) {
                self.release_failed_reservations(&operation_id, &begun);
                return Err(storage_error(error));
            }
            begun.push(topology);
        }
        match enqueue_plan(self.storage, plan) {
            Ok(operation) => Ok(operation),
            Err(error) => {
                self.release_failed_reservations(&operation_id, &begun);
                Err(error)
            }
        }
    }

    fn release_failed_reservations(&self, operation_id: &str, begun: &[TopologyReservation<'_>]) {
        for topology in begun.iter().rev() {
            let _ = self.storage.finish_topology_apply(
                topology.topology_id,
                topology.revision_id,
                operation_id,
                orchestrator_storage::TopologyApplyOutcome::Failed,
                &now_marker(),
            );
        }
    }
}

fn enqueue_plan(
    storage: &DurableStore,
    plan: PlanOperation,
) -> Result<orchestrator_control_plane::DurableOperation, StoreError> {
    use orchestrator_control_plane::DurableOperationStatus;

    let operation_id = plan.operation_id.clone();
    let now = now_ms();
    let mut operations = storage.operation_store();
    let mut jobs = storage.job_store();
    let mut coordinator = OperationCoordinator::new(&mut operations, &mut jobs);
    let existing = coordinator.plan(plan, now).map_err(operation_error)?;
    match existing.status {
        DurableOperationStatus::Planned => {
            coordinator
                .confirm(&operation_id, now)
                .map_err(operation_error)?;
            coordinator
                .enqueue(&operation_id, now)
                .map_err(operation_error)
        }
        DurableOperationStatus::Confirmed
        | DurableOperationStatus::Enqueuing
        | DurableOperationStatus::Running => coordinator
            .enqueue(&operation_id, now)
            .map_err(operation_error),
        DurableOperationStatus::Cancelling
        | DurableOperationStatus::Succeeded
        | DurableOperationStatus::Failed
        | DurableOperationStatus::Cancelled
        | DurableOperationStatus::NeedsAttention
        | DurableOperationStatus::RolledBack => Ok(existing),
    }
}
