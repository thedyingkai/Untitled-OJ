//! Background worker entry points. Lease/recovery, observation, projection and
//! job application have separate owners; deterministic Binding rules live in core.
mod apply;
mod binding_health;
mod context;
mod external_health;
mod lease;
mod network;
mod node_lifecycle;
mod observation;
mod payload;
mod projection;
mod reconciliation;
mod recovery;

pub(crate) use apply::process_one;
pub(crate) use projection::{
    reconcile_runtime_binding_projections, runtime_preserves_active_binding_route,
};

use crate::durable::DurableStore;
use crate::topology_provider::TopologyProviderSaga;
use crate::topology_worker::context::now_ms;
use crate::topology_worker::reconciliation::run_reconciler_loop;
use crate::topology_worker::recovery::{
    recover_expired, recover_terminal_topology_applies, repair_recoverable_operation_projections,
};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;
use std::thread;
use std::time::Duration;

pub(crate) fn run_loop(
    storage: DurableStore,
    provider: Option<TopologyProviderSaga>,
    shutdown: Arc<AtomicBool>,
) {
    let reconcile_provider = provider.clone();
    let reconcile_storage = storage.clone();
    let reconcile_shutdown = Arc::clone(&shutdown);
    let reconciler = thread::Builder::new()
        .name("orchestrator-topology-reconciler".to_string())
        .spawn(move || {
            run_reconciler_loop(
                &reconcile_storage,
                reconcile_provider.as_ref(),
                &reconcile_shutdown,
            )
        })
        .ok();
    let mut last_terminal_recovery_ms = 0_i64;
    while !shutdown.load(Ordering::Acquire) {
        let now = now_ms();
        if now.saturating_sub(last_terminal_recovery_ms) >= 1_000 {
            if let Err(error) = recover_terminal_topology_applies(&storage) {
                eprintln!("topology terminal-operation recovery error: {error}");
            }
            last_terminal_recovery_ms = now;
        }
        match process_one(&storage, provider.as_ref()) {
            Ok(true) => {}
            Ok(false) => thread::sleep(Duration::from_millis(100)),
            Err(error) => {
                eprintln!("topology control-plane worker error: {error}");
                thread::sleep(Duration::from_secs(1));
            }
        }
    }
    if let Some(reconciler) = reconciler {
        let _ = reconciler.join();
    }
}

/// The sole periodic owner of expired-lease recovery. Claims never perform
/// recovery, so 100 long-polling Agents cannot multiply full recovery scans or
/// serialize the queue mutex hundreds of times per second.
pub(crate) fn run_lease_recovery_loop(storage: DurableStore, shutdown: Arc<AtomicBool>) {
    while !shutdown.load(Ordering::Acquire) {
        if let Err(error) = recover_expired(&storage, now_ms()) {
            eprintln!("control-plane lease recovery error: {error}");
        }
        if let Err(error) = repair_recoverable_operation_projections(&storage, now_ms()) {
            eprintln!("control-plane Operation projection repair error: {error}");
        }
        let deadline = std::time::Instant::now() + Duration::from_secs(1);
        while !shutdown.load(Ordering::Acquire) && std::time::Instant::now() < deadline {
            thread::sleep(Duration::from_millis(100));
        }
    }
}
