//! Background recovery responsibilities.
use crate::contribution_controller;
use crate::durable::DurableStore;
use crate::topology_worker::context::now_marker;
use crate::topology_worker::payload::{TopologyApplyPayload, TopologyApplyPhase};
use orchestrator_control_plane::DurableOperationStatus;
use orchestrator_control_plane::Job;
use orchestrator_control_plane::JobKind;
use orchestrator_control_plane::JobStore;
use orchestrator_control_plane::OperationCoordinator;
use orchestrator_control_plane::OperationRepository;
use orchestrator_control_plane::ResolveExpiredSuccessRequest;
use orchestrator_legacy::TopologyDrift;
use orchestrator_legacy::TopologyDriftKind;
use orchestrator_legacy::TopologyReconciliationState;
use orchestrator_legacy::TopologyResourceKind;
use orchestrator_storage::TopologyApplyOutcome;
use serde_json::Value;

pub(super) fn topology_expired_success_plan(
    storage: &DurableStore,
    job: &Job,
    payload: &TopologyApplyPayload,
) -> Result<Option<(Vec<orchestrator_storage::TopologyApplyGroupMember>, Value)>, String> {
    let mut identities = match payload.phase {
        TopologyApplyPhase::FinalizeGroup => payload
            .group
            .iter()
            .map(|member| (member.topology_id.clone(), member.revision_id.clone()))
            .collect::<Vec<_>>(),
        TopologyApplyPhase::Finalize | TopologyApplyPhase::Full => {
            vec![(payload.topology_id.clone(), payload.revision_id.clone())]
        }
        TopologyApplyPhase::Stage | TopologyApplyPhase::Prepare | TopologyApplyPhase::Abort => {
            return Ok(None);
        }
    };
    identities.sort();
    if identities.is_empty()
        || identities.iter().any(|(topology_id, revision_id)| {
            topology_id.trim().is_empty() || revision_id.trim().is_empty()
        })
        || identities.windows(2).any(|pair| pair[0].0 == pair[1].0)
    {
        return Err(format!(
            "expired topology Job {} has invalid success-evidence identities",
            job.job_id
        ));
    }

    let mut binding_counts = Vec::with_capacity(identities.len());
    let members = identities
        .iter()
        .map(|(topology_id, revision_id)| {
            let binding_count = storage
                .api_bindings_for_topology(topology_id)
                .map_err(|error| error.to_string())?
                .len();
            binding_counts.push((topology_id.clone(), revision_id.clone(), binding_count));
            Ok(orchestrator_storage::TopologyApplyGroupMember {
                topology_id: topology_id.clone(),
                revision_id: revision_id.clone(),
                active_bindings: Vec::new(),
            })
        })
        .collect::<Result<Vec<_>, String>>()?;
    let result = match payload.phase {
        TopologyApplyPhase::FinalizeGroup => serde_json::json!({
            "phase": "FINALIZE_GROUP",
            "topologies": binding_counts
                .into_iter()
                .map(|(topology_id, revision_id, bindings)| serde_json::json!({
                    "topology_id": topology_id,
                    "revision_id": revision_id,
                    "bindings": bindings,
                }))
                .collect::<Vec<_>>(),
        }),
        TopologyApplyPhase::Finalize => serde_json::json!({
            "phase": "FINALIZE",
            "topology_id": payload.topology_id,
            "revision_id": payload.revision_id,
            "bindings": binding_counts.first().map(|member| member.2).unwrap_or(0),
        }),
        TopologyApplyPhase::Full => serde_json::json!({
            "phase": "FULL",
            "topology_id": payload.topology_id,
            "revision_id": payload.revision_id,
            "recovered_from_durable_head": true,
        }),
        TopologyApplyPhase::Stage | TopologyApplyPhase::Prepare | TopologyApplyPhase::Abort => {
            unreachable!("non-final phases returned before building recovery evidence")
        }
    };
    Ok(Some((members, result)))
}

pub(super) fn recover_unknown_topology_payload(
    storage: &DurableStore,
    payload: TopologyApplyPayload,
    operation_id: &str,
) -> Result<(), String> {
    if payload.phase == TopologyApplyPhase::FinalizeGroup {
        if payload.group.is_empty() {
            return Err("expired FINALIZE_GROUP payload has no members".to_string());
        }
        let detail = "control-plane worker lease expired with an unproven grouped provider outcome";
        let mut failures = Vec::new();
        for member in payload.group {
            if let Err(error) = recover_unknown_topology_apply(
                storage,
                &member.topology_id,
                &member.revision_id,
                operation_id,
                detail,
            ) {
                failures.push(format!("{}: {error}", member.topology_id));
                continue;
            }
            // A mixed group is never a successful atomic generation. Members
            // that were already visible must therefore remain visible but be
            // marked Degraded alongside members whose applying head was
            // released above.
            if let Err(error) = mark_degraded(storage, &member.topology_id, operation_id, detail) {
                failures.push(format!("{}: {error}", member.topology_id));
            }
        }
        if !failures.is_empty() {
            return Err(format!(
                "{} grouped topology recovery member(s) failed: {}",
                failures.len(),
                failures.join("; ")
            ));
        }
        return Ok(());
    }
    recover_unknown_topology_apply(
        storage,
        &payload.topology_id,
        &payload.revision_id,
        operation_id,
        "control-plane worker lease expired with an unproven provider outcome",
    )
}

pub(super) fn recover_expired(storage: &DurableStore, now_ms: i64) -> Result<(), String> {
    let jobs = storage.job_store();
    let expired = jobs
        .expired_leases(now_ms)
        .map_err(|error| error.to_string())?;
    drop(jobs);

    for job in &expired {
        if !matches!(
            job.kind,
            JobKind::TopologyApply | JobKind::ContributionProjection
        ) {
            continue;
        }
        if contribution_controller::is_contribution_job(job) {
            match contribution_controller::recover_expired_contribution_job(storage, job) {
                Ok(Some(result)) => {
                    let mut jobs = storage.job_store();
                    jobs.resolve_expired_success(ResolveExpiredSuccessRequest {
                        job_id: job.job_id.clone(),
                        now_ms,
                        result,
                    })
                    .map_err(|error| {
                        format!(
                            "resolve expired contribution Job {} from durable evidence: {error}",
                            job.job_id
                        )
                    })?;
                }
                Ok(None) => {
                    // No side-effect outcome can be proved. JobStore recovery
                    // will move this non-retry-safe lease to NEEDS_ATTENTION;
                    // the durable activation/receipts remain the repair source.
                }
                Err(error) => {
                    eprintln!(
                        "expired contribution Job {} could not be reconciled: {error}",
                        job.job_id
                    );
                }
            }
            continue;
        }
        let payload = serde_json::from_value::<TopologyApplyPayload>(job.payload.clone()).map_err(
            |error| {
                format!(
                    "expired topology Job {} has an invalid recovery payload: {error}",
                    job.job_id
                )
            },
        )?;
        let resolved = match topology_expired_success_plan(storage, job, &payload)? {
            Some((members, result)) => storage
                .resolve_expired_topology_apply_group_success(
                    &members,
                    &job.operation_id,
                    &job.job_id,
                    now_ms,
                    result,
                )
                .map_err(|error| error.to_string())?
                .is_some(),
            None => false,
        };
        if !resolved {
            recover_unknown_topology_payload(storage, payload, &job.operation_id)?;
        }
    }

    let mut jobs = storage.job_store();
    let mut operations = storage.operation_store();
    OperationCoordinator::new(&mut operations, &mut jobs)
        .recover(now_ms)
        .map_err(|error| error.to_string())?;
    Ok(())
}

/// Repairs the intentional transaction boundary between durable Job completion

/// and its Operation projection. A process crash or a transient persistence

/// failure after `JobStore::complete` must not leave a terminal Job represented

/// forever by a stale RUNNING/LEASED Operation snapshot.

pub(super) fn repair_recoverable_operation_projections(
    storage: &DurableStore,
    now_ms: i64,
) -> Result<(), String> {
    let recoverable = storage
        .operation_store()
        .recoverable()
        .map_err(|error| error.to_string())?;
    let mut failures = Vec::new();
    for operation in recoverable {
        let should_auto_enqueue = operation.status == DurableOperationStatus::Confirmed
            && operation
                .request
                .get("auto_enqueue")
                .and_then(Value::as_bool)
                == Some(true);
        if operation.status == DurableOperationStatus::Confirmed && !should_auto_enqueue {
            continue;
        }
        let mut operations = storage.operation_store();
        let mut jobs = storage.job_store();
        let mut coordinator = OperationCoordinator::new(&mut operations, &mut jobs);
        let repaired = match operation.status {
            DurableOperationStatus::Confirmed | DurableOperationStatus::Enqueuing => {
                coordinator.enqueue(&operation.operation_id, now_ms)
            }
            DurableOperationStatus::Running => coordinator.project(&operation.operation_id, now_ms),
            DurableOperationStatus::Cancelling => {
                coordinator.cancel(&operation.operation_id, now_ms)
            }
            _ => continue,
        };
        if let Err(error) = repaired {
            failures.push(format!("{}: {error}", operation.operation_id));
        }
    }
    if failures.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "{} recoverable Operation projection(s) failed: {}",
            failures.len(),
            failures.join("; ")
        ))
    }
}

pub(super) fn recover_terminal_topology_applies(storage: &DurableStore) -> Result<(), String> {
    for heads in storage
        .list_topology_heads()
        .map_err(|error| error.to_string())?
    {
        let (Some(revision_id), Some(operation_id)) = (
            heads.applying_revision_id.as_deref(),
            heads.applying_operation_id.as_deref(),
        ) else {
            continue;
        };
        let operation = storage
            .operation_store()
            .get(operation_id)
            .map_err(|error| error.to_string())?;
        let (outcome, degraded_detail) = match operation.map(|operation| operation.status) {
            Some(DurableOperationStatus::Cancelled | DurableOperationStatus::Failed) => {
                (TopologyApplyOutcome::Failed, None)
            }
            Some(DurableOperationStatus::NeedsAttention) => (
                TopologyApplyOutcome::Degraded,
                Some("topology apply operation requires explicit reconciliation"),
            ),
            Some(DurableOperationStatus::Succeeded) => (TopologyApplyOutcome::Succeeded, None),
            Some(
                DurableOperationStatus::Planned
                | DurableOperationStatus::Confirmed
                | DurableOperationStatus::Enqueuing
                | DurableOperationStatus::Running
                | DurableOperationStatus::Cancelling
                | DurableOperationStatus::RolledBack,
            ) => continue,
            None => (
                TopologyApplyOutcome::Degraded,
                Some("topology apply ownership references a missing Operation"),
            ),
        };
        storage
            .finish_topology_apply(
                &heads.topology_id,
                revision_id,
                operation_id,
                outcome,
                &now_marker(),
            )
            .map_err(|error| error.to_string())?;
        if let Some(detail) = degraded_detail {
            mark_degraded(storage, &heads.topology_id, operation_id, detail)?;
        }
    }
    Ok(())
}

/// Releases durable apply ownership after an outcome becomes unknowable.

///

/// A crashed control-plane must never blindly replay provider mutations, but

/// leaving `applying_revision_id` set would also permanently prevent drafts

/// and make the reconciler skip the topology.  Completing the apply as

/// `Degraded` keeps the last proven applied head, records the attempted

/// revision as desired state, and lets fresh provider observations drive the

/// explicit operator reconciliation that follows `NEEDS_ATTENTION`.

pub(super) fn finish_unknown_topology_apply(
    storage: &DurableStore,
    topology_id: &str,
    revision_id: &str,
    operation_id: &str,
    detail: &str,
) -> Result<(), String> {
    storage
        .finish_topology_apply(
            topology_id,
            revision_id,
            operation_id,
            TopologyApplyOutcome::Degraded,
            &now_marker(),
        )
        .map_err(|error| {
            format!("unknown topology apply outcome could not release durable ownership: {error}")
        })?;
    mark_degraded(storage, topology_id, operation_id, detail)
}

pub(super) fn recover_unknown_topology_apply(
    storage: &DurableStore,
    topology_id: &str,
    revision_id: &str,
    operation_id: &str,
    detail: &str,
) -> Result<(), String> {
    let heads = storage
        .topology_heads(topology_id)
        .map_err(|error| error.to_string())?
        .ok_or_else(|| format!("topology {topology_id} disappeared during recovery"))?;
    if heads.applying_revision_id.as_deref() == Some(revision_id)
        && heads.applying_operation_id.as_deref() == Some(operation_id)
    {
        return finish_unknown_topology_apply(
            storage,
            topology_id,
            revision_id,
            operation_id,
            detail,
        );
    }
    if heads.applied_revision_id.as_deref() == Some(revision_id)
        && heads.last_operation_id.as_deref() == Some(operation_id)
    {
        // The provider acknowledgement and applied-head commit completed
        // before the worker crashed.  That durable commit is proof of the
        // topology result, so do not downgrade or replay the provider state.
        return Ok(());
    }
    mark_degraded(storage, topology_id, operation_id, detail)
}

pub(super) fn mark_degraded(
    storage: &DurableStore,
    topology_id: &str,
    operation_id: &str,
    detail: &str,
) -> Result<(), String> {
    let mut status = storage
        .topology_status(topology_id)
        .map_err(|error| error.to_string())?
        .ok_or_else(|| format!("topology {topology_id} has no status"))?;
    status.state = TopologyReconciliationState::Degraded;
    status.last_operation_id = Some(operation_id.to_string());
    status.updated_at = now_marker();
    status.drift = vec![TopologyDrift {
        resource_kind: TopologyResourceKind::Authority,
        resource_id: topology_id.to_string(),
        kind: TopologyDriftKind::Unreachable,
        detail: detail.to_string(),
    }];
    storage
        .put_topology_status(&status)
        .map_err(|error| error.to_string())
}
