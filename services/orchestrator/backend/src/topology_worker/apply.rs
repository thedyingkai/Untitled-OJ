//! Background apply responsibilities.
use crate::contribution_controller;
use crate::durable::DurableStore;
use crate::topology_provider::TopologyProviderApplyState;
use crate::topology_provider::TopologyProviderSaga;
use crate::topology_provider::provider_projection_sha256;
use crate::topology_worker::binding_health::{
    topology_binding_consumers_healthy, topology_binding_providers_healthy,
};
use crate::topology_worker::context::{CONTROL_PLANE_NODE_ID, now_marker, now_ms};
use crate::topology_worker::external_health::process_external_health;
use crate::topology_worker::lease::{
    ControlPlaneLeaseHeartbeat, complete_and_project, lease_token,
};
use crate::topology_worker::node_lifecycle::process_node_lifecycle;
use crate::topology_worker::payload::{
    TopologyApplyGroupPayloadMember, TopologyApplyPayload, TopologyApplyPhase,
};
use crate::topology_worker::recovery::finish_unknown_topology_apply;
use orchestrator_control_plane::ClaimRequest;
use orchestrator_control_plane::CompletionStatus;
use orchestrator_control_plane::DEFAULT_LEASE_MS;
use orchestrator_control_plane::JobKind;
use orchestrator_control_plane::JobStore;
use orchestrator_core::binding_projection::activate_staged_bindings;
use orchestrator_core::binding_projection::normalize_group_binding_moves;
use orchestrator_core::binding_projection::validate_prepared_bindings;
use orchestrator_storage::TopologyApplyOutcome;
use serde_json::Value;

pub(crate) fn process_one(
    storage: &DurableStore,
    provider: Option<&TopologyProviderSaga>,
) -> Result<bool, String> {
    let now = now_ms();
    let mut jobs = storage.job_store();
    let Some(job) = jobs
        .claim(ClaimRequest {
            node_id: CONTROL_PLANE_NODE_ID.to_string(),
            instance_id: "single-active-control-plane".to_string(),
            lease_token: lease_token()?,
            now_ms: now,
            lease_ms: DEFAULT_LEASE_MS,
        })
        .map_err(|error| error.to_string())?
    else {
        return Ok(false);
    };
    let lease_token = job
        .lease_token
        .clone()
        .ok_or_else(|| "claimed topology job has no lease token".to_string())?;
    let lease_heartbeat = ControlPlaneLeaseHeartbeat::start(
        storage.clone(),
        job.job_id.clone(),
        lease_token.clone(),
        job.lease_expires_at_ms
            .ok_or_else(|| "claimed topology job has no lease expiry".to_string())?,
    )?;
    lease_heartbeat.checkpoint(&mut jobs)?;
    if contribution_controller::is_contribution_job(&job) {
        let outcome = contribution_controller::execute_contribution_job(
            storage,
            &job.payload,
            &job.operation_id,
            || lease_heartbeat.checkpoint(&mut jobs),
        );
        match outcome {
            Ok(outcome) => complete_and_project(
                storage,
                &mut jobs,
                &job.job_id,
                &job.operation_id,
                lease_token,
                CompletionStatus::Succeeded,
                outcome.result,
                String::new(),
            )?,
            Err(error) => {
                let status = if error.retryable() {
                    if error.retry_exhaustion_needs_attention() && job.attempt >= job.max_attempts {
                        CompletionStatus::NeedsAttention
                    } else {
                        CompletionStatus::RetryableFailure
                    }
                } else if error.needs_attention() {
                    CompletionStatus::NeedsAttention
                } else {
                    CompletionStatus::Failed
                };
                complete_and_project(
                    storage,
                    &mut jobs,
                    &job.job_id,
                    &job.operation_id,
                    lease_token,
                    status,
                    serde_json::json!({"code": error.code()}),
                    error.to_string(),
                )?;
            }
        }
        return Ok(true);
    }
    if matches!(job.kind, JobKind::NodeDrain | JobKind::NodeRemove) {
        let outcome = process_node_lifecycle(storage, &job.kind, &job.payload);
        match outcome {
            Ok(result) => complete_and_project(
                storage,
                &mut jobs,
                &job.job_id,
                &job.operation_id,
                lease_token,
                CompletionStatus::Succeeded,
                result,
                String::new(),
            )?,
            Err(failure) => complete_and_project(
                storage,
                &mut jobs,
                &job.job_id,
                &job.operation_id,
                lease_token,
                CompletionStatus::Failed,
                serde_json::json!({"code": failure.code}),
                failure.detail,
            )?,
        }
        return Ok(true);
    }
    if job.kind == JobKind::ExternalHealth {
        match process_external_health(storage, &job.payload) {
            Ok(result) => complete_and_project(
                storage,
                &mut jobs,
                &job.job_id,
                &job.operation_id,
                lease_token,
                CompletionStatus::Succeeded,
                result,
                String::new(),
            )?,
            Err(failure) => complete_and_project(
                storage,
                &mut jobs,
                &job.job_id,
                &job.operation_id,
                lease_token,
                failure.status,
                serde_json::json!({"code": failure.code}),
                failure.detail,
            )?,
        }
        return Ok(true);
    }
    if job.kind != JobKind::TopologyApply {
        complete_and_project(
            storage,
            &mut jobs,
            &job.job_id,
            &job.operation_id,
            lease_token,
            CompletionStatus::NeedsAttention,
            Value::Null,
            format!(
                "control-plane queue received unsupported job kind {:?}",
                job.kind
            ),
        )?;
        return Ok(true);
    }
    let Some(provider) = provider else {
        complete_and_project(
            storage,
            &mut jobs,
            &job.job_id,
            &job.operation_id,
            lease_token,
            CompletionStatus::NeedsAttention,
            Value::Null,
            "Topology providers are unavailable after the apply job was durably accepted"
                .to_string(),
        )?;
        if let Ok(payload) = serde_json::from_value::<TopologyApplyPayload>(job.payload.clone())
            && payload.phase != TopologyApplyPhase::FinalizeGroup
        {
            finish_unknown_topology_apply(
                storage,
                &payload.topology_id,
                &payload.revision_id,
                &job.operation_id,
                "Topology providers are unavailable after the apply job was durably accepted",
            )?;
        }
        return Ok(true);
    };

    let payload: TopologyApplyPayload = serde_json::from_value(job.payload.clone())
        .map_err(|error| format!("invalid topology apply payload: {error}"))?;
    if payload.phase == TopologyApplyPhase::FinalizeGroup {
        return finalize_topology_group(
            storage,
            provider,
            &lease_heartbeat,
            &mut jobs,
            &job.job_id,
            &job.operation_id,
            lease_token,
            &payload.group,
        );
    }
    let mut heads = storage
        .topology_heads(&payload.topology_id)
        .map_err(|error| error.to_string())?
        .ok_or_else(|| format!("topology {} disappeared", payload.topology_id))?;
    if matches!(
        payload.phase,
        TopologyApplyPhase::Full | TopologyApplyPhase::Prepare
    ) && heads.applying_revision_id.is_none()
        && heads.draft_revision_id == payload.revision_id
        && heads.applied_revision_id.as_deref() != Some(payload.revision_id.as_str())
    {
        // A compensated FAILED apply clears ownership. A generic Operation
        // retry creates a fresh durable job for the same revision, so it must
        // reacquire the topology CAS before any provider I/O.
        storage
            .begin_topology_apply(
                &payload.topology_id,
                &payload.revision_id,
                &job.operation_id,
                &now_marker(),
            )
            .map_err(|error| format!("retry could not reacquire topology apply: {error}"))?;
        heads = storage
            .topology_heads(&payload.topology_id)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| format!("topology {} disappeared", payload.topology_id))?;
    }
    let aborting_completed_group_member = payload.phase == TopologyApplyPhase::Abort
        && heads.applying_revision_id.is_none()
        && heads.applied_revision_id.as_deref() == Some(payload.revision_id.as_str())
        && heads.last_operation_id.as_deref() == Some(job.operation_id.as_str());
    if (heads.applying_revision_id.as_deref() != Some(payload.revision_id.as_str())
        || heads.applying_operation_id.as_deref() != Some(job.operation_id.as_str()))
        && !aborting_completed_group_member
    {
        let compensated_failure_kept_retryable_draft = heads.draft_revision_id
            == payload.revision_id
            && heads.applied_revision_id.as_deref() != Some(payload.revision_id.as_str());
        let compensated_abort_restored_previous_draft = heads
            .applied_revision_id
            .as_ref()
            .is_some_and(|applied| heads.draft_revision_id == *applied)
            && heads.applied_revision_id.as_deref() != Some(payload.revision_id.as_str());
        if payload.phase == TopologyApplyPhase::Abort
            && heads.applying_revision_id.is_none()
            && heads.last_operation_id.as_deref() == Some(job.operation_id.as_str())
            && (compensated_failure_kept_retryable_draft
                || compensated_abort_restored_previous_draft)
        {
            // A FAILED forward phase releases topology ownership only after
            // it has proved provider and binding compensation. It deliberately
            // leaves the candidate as the draft so an explicit retry can
            // reacquire the same immutable revision. A completed ABORT instead
            // restores draft and applied to the previous revision. Both are
            // durable, writer-fenced terminal facts, so replaying the planned
            // ABORT must be a no-op rather than manufacturing NEEDS_ATTENTION.
            complete_and_project(
                storage,
                &mut jobs,
                &job.job_id,
                &job.operation_id,
                lease_token,
                CompletionStatus::Succeeded,
                serde_json::json!({
                    "phase": "ABORT",
                    "restored": true,
                    "replayed": true,
                    "retryable_draft": compensated_failure_kept_retryable_draft,
                }),
                String::new(),
            )?;
            return Ok(true);
        }
        complete_and_project(
            storage,
            &mut jobs,
            &job.job_id,
            &job.operation_id,
            lease_token,
            CompletionStatus::NeedsAttention,
            Value::Null,
            "topology apply ownership no longer matches the durable head".to_string(),
        )?;
        return Ok(true);
    }
    let revision = storage
        .topology_revision(&payload.topology_id, &payload.revision_id)
        .map_err(|error| error.to_string())?
        .ok_or_else(|| format!("topology revision {} disappeared", payload.revision_id))?;
    let previous_revision_id = if aborting_completed_group_member {
        revision.parent_revision_id()
    } else {
        heads.applied_revision_id.as_deref()
    };
    let previous = previous_revision_id
        .map(|revision_id| {
            storage
                .topology_revision(&payload.topology_id, revision_id)
                .map_err(|error| error.to_string())?
                .ok_or_else(|| format!("applied topology revision {revision_id} disappeared"))
        })
        .transpose()?;
    if payload.phase == TopologyApplyPhase::Abort {
        lease_heartbeat.checkpoint(&mut jobs)?;
        let provider_compensation = provider.compensate_applied_revision(
            &payload.topology_id,
            &payload.revision_id,
            previous.as_ref().map(|revision| revision.revision_id()),
            previous.as_ref().map(|revision| revision.spec()),
            &payload.previous_bindings,
            &job.operation_id,
        );
        lease_heartbeat.checkpoint(&mut jobs)?;
        let binding_compensation =
            storage.replace_topology_api_bindings(&payload.topology_id, &payload.previous_bindings);
        let degraded = provider_compensation.is_err() || binding_compensation.is_err();
        let finish = if degraded {
            Err(
                "provider or binding restoration failed; durable candidate ownership was retained"
                    .to_string(),
            )
        } else if aborting_completed_group_member {
            previous_revision_id
                .ok_or_else(|| {
                    "group compensation cannot rewind an initial topology revision".to_string()
                })
                .and_then(|previous_revision_id| {
                    storage
                        .compensate_completed_topology_apply(
                            &payload.topology_id,
                            &payload.revision_id,
                            previous_revision_id,
                            &job.operation_id,
                            &now_marker(),
                        )
                        .map_err(|error| error.to_string())
                })
        } else {
            previous_revision_id
                .ok_or_else(|| {
                    "an initial topology revision has no safe previous draft to restore".to_string()
                })
                .and_then(|previous_revision_id| {
                    storage
                        .complete_compensated_topology_abort(
                            &payload.topology_id,
                            &payload.revision_id,
                            previous_revision_id,
                            &job.operation_id,
                            &now_marker(),
                        )
                        .map_err(|error| error.to_string())
                })
        };
        let finish_error = finish.err();
        let needs_attention = degraded || finish_error.is_some();
        let detail = format!(
            "topology abort provider restore: {}; binding restore: {}; head release: {}",
            provider_compensation
                .err()
                .unwrap_or_else(|| "succeeded".to_string()),
            binding_compensation
                .err()
                .map(|error| error.to_string())
                .unwrap_or_else(|| "succeeded".to_string()),
            finish_error
                .map(|error| error.to_string())
                .unwrap_or_else(|| "succeeded".to_string()),
        );
        complete_and_project(
            storage,
            &mut jobs,
            &job.job_id,
            &job.operation_id,
            lease_token,
            if needs_attention {
                CompletionStatus::NeedsAttention
            } else {
                CompletionStatus::Succeeded
            },
            serde_json::json!({
                "phase": "ABORT",
                "restored": !needs_attention,
            }),
            if needs_attention {
                detail
            } else {
                String::new()
            },
        )?;
        return Ok(true);
    }
    if payload.phase == TopologyApplyPhase::Finalize {
        let staged_bindings = storage
            .api_bindings_for_topology(&payload.topology_id)
            .map_err(|error| error.to_string())?;
        let health = topology_binding_providers_healthy(storage, &staged_bindings)
            .and_then(|()| topology_binding_consumers_healthy(storage, &staged_bindings));
        if health.is_ok() {
            let active_bindings = activate_staged_bindings(staged_bindings, &now_marker());
            if let Err(activation_error) =
                storage.replace_topology_api_bindings(&payload.topology_id, &active_bindings)
            {
                let provider_compensation = provider.compensate_applied_revision(
                    &payload.topology_id,
                    &payload.revision_id,
                    previous.as_ref().map(|revision| revision.revision_id()),
                    previous.as_ref().map(|revision| revision.spec()),
                    &payload.previous_bindings,
                    &job.operation_id,
                );
                let binding_compensation = storage.replace_topology_api_bindings(
                    &payload.topology_id,
                    &payload.previous_bindings,
                );
                let degraded = provider_compensation.is_err() || binding_compensation.is_err();
                storage
                    .finish_topology_apply(
                        &payload.topology_id,
                        &payload.revision_id,
                        &job.operation_id,
                        if degraded {
                            TopologyApplyOutcome::Degraded
                        } else {
                            TopologyApplyOutcome::Failed
                        },
                        &now_marker(),
                    )
                    .map_err(|error| {
                        format!("finalize activation cleanup could not release ownership: {error}")
                    })?;
                complete_and_project(
                    storage,
                    &mut jobs,
                    &job.job_id,
                    &job.operation_id,
                    lease_token,
                    if degraded {
                        CompletionStatus::NeedsAttention
                    } else {
                        CompletionStatus::Failed
                    },
                    serde_json::json!({"code": "TOPOLOGY_BINDING_ACTIVATION_FAILED", "phase": "FINALIZE"}),
                    format!(
                        "binding activation failed ({activation_error}); provider compensation: {}; binding compensation: {}",
                        provider_compensation
                            .err()
                            .unwrap_or_else(|| "succeeded".to_string()),
                        binding_compensation
                            .err()
                            .map(|error| error.to_string())
                            .unwrap_or_else(|| "succeeded".to_string()),
                    ),
                )?;
                return Ok(true);
            }
            lease_heartbeat.checkpoint(&mut jobs)?;
            if let Err(head_error) = storage.finish_topology_apply_fenced(
                &payload.topology_id,
                &payload.revision_id,
                &job.operation_id,
                TopologyApplyOutcome::Succeeded,
                &now_marker(),
                &job.job_id,
                &lease_token,
                now_ms(),
            ) {
                let provider_compensation = provider.compensate_applied_revision(
                    &payload.topology_id,
                    &payload.revision_id,
                    previous.as_ref().map(|revision| revision.revision_id()),
                    previous.as_ref().map(|revision| revision.spec()),
                    &payload.previous_bindings,
                    &job.operation_id,
                );
                let binding_compensation = storage.replace_topology_api_bindings(
                    &payload.topology_id,
                    &payload.previous_bindings,
                );
                let _ = storage.finish_topology_apply(
                    &payload.topology_id,
                    &payload.revision_id,
                    &job.operation_id,
                    TopologyApplyOutcome::Degraded,
                    &now_marker(),
                );
                complete_and_project(
                    storage,
                    &mut jobs,
                    &job.job_id,
                    &job.operation_id,
                    lease_token,
                    CompletionStatus::NeedsAttention,
                    serde_json::json!({"code": "TOPOLOGY_HEAD_ADVANCE_FAILED", "phase": "FINALIZE"}),
                    format!(
                        "binding projection activated but applied head did not advance ({head_error}); provider compensation: {}; binding compensation: {}",
                        provider_compensation
                            .err()
                            .unwrap_or_else(|| "succeeded".to_string()),
                        binding_compensation
                            .err()
                            .map(|error| error.to_string())
                            .unwrap_or_else(|| "succeeded".to_string()),
                    ),
                )?;
                return Ok(true);
            }
            complete_and_project(
                storage,
                &mut jobs,
                &job.job_id,
                &job.operation_id,
                lease_token,
                CompletionStatus::Succeeded,
                serde_json::json!({
                    "topology_id": payload.topology_id,
                    "revision_id": payload.revision_id,
                    "phase": "FINALIZE",
                    "bindings": active_bindings.len(),
                }),
                String::new(),
            )?;
            return Ok(true);
        }
        let health = health.unwrap_err();
        let provider_compensation = provider.compensate_applied_revision(
            &payload.topology_id,
            &payload.revision_id,
            previous.as_ref().map(|revision| revision.revision_id()),
            previous.as_ref().map(|revision| revision.spec()),
            &payload.previous_bindings,
            &job.operation_id,
        );
        let binding_compensation =
            storage.replace_topology_api_bindings(&payload.topology_id, &payload.previous_bindings);
        let degraded = provider_compensation.is_err() || binding_compensation.is_err();
        storage
            .finish_topology_apply(
                &payload.topology_id,
                &payload.revision_id,
                &job.operation_id,
                if degraded {
                    TopologyApplyOutcome::Degraded
                } else {
                    TopologyApplyOutcome::Failed
                },
                &now_marker(),
            )
            .map_err(|error| {
                format!("finalize failure could not release apply ownership: {error}")
            })?;
        let detail = format!(
            "consumer health gate failed ({health}); provider compensation: {}; binding compensation: {}",
            provider_compensation
                .err()
                .unwrap_or_else(|| "succeeded".to_string()),
            binding_compensation
                .err()
                .map(|error| error.to_string())
                .unwrap_or_else(|| "succeeded".to_string()),
        );
        complete_and_project(
            storage,
            &mut jobs,
            &job.job_id,
            &job.operation_id,
            lease_token,
            if degraded {
                CompletionStatus::NeedsAttention
            } else {
                CompletionStatus::Failed
            },
            serde_json::json!({"code": "TOPOLOGY_CONSUMER_UNHEALTHY", "phase": "FINALIZE"}),
            detail,
        )?;
        return Ok(true);
    }
    let previous_bindings = storage
        .api_bindings_for_topology(&payload.topology_id)
        .map_err(|error| error.to_string())?;
    if payload.phase == TopologyApplyPhase::Stage {
        let validation = validate_prepared_bindings(
            &payload.bindings,
            &payload.topology_id,
            &payload.revision_id,
            &job.operation_id,
        );
        if let Err(detail) = validation {
            storage
                .finish_topology_apply(
                    &payload.topology_id,
                    &payload.revision_id,
                    &job.operation_id,
                    TopologyApplyOutcome::Failed,
                    &now_marker(),
                )
                .map_err(|error| format!("{detail}; stage validation cleanup failed: {error}"))?;
            complete_and_project(
                storage,
                &mut jobs,
                &job.job_id,
                &job.operation_id,
                lease_token,
                CompletionStatus::Failed,
                serde_json::json!({"code": "TOPOLOGY_BINDING_STAGE_REJECTED", "phase": "STAGE"}),
                detail,
            )?;
            return Ok(true);
        }
        match storage.replace_topology_api_bindings(&payload.topology_id, &payload.bindings) {
            Ok(()) => complete_and_project(
                storage,
                &mut jobs,
                &job.job_id,
                &job.operation_id,
                lease_token,
                CompletionStatus::Succeeded,
                serde_json::json!({"phase": "STAGE", "bindings": payload.bindings.len()}),
                String::new(),
            )?,
            Err(error) => {
                storage
                    .finish_topology_apply(
                        &payload.topology_id,
                        &payload.revision_id,
                        &job.operation_id,
                        TopologyApplyOutcome::Failed,
                        &now_marker(),
                    )
                    .map_err(|finish| {
                        format!("binding stage failed ({error}); cleanup failed: {finish}")
                    })?;
                complete_and_project(
                    storage,
                    &mut jobs,
                    &job.job_id,
                    &job.operation_id,
                    lease_token,
                    CompletionStatus::Failed,
                    serde_json::json!({"code": "TOPOLOGY_BINDING_STAGE_FAILED", "phase": "STAGE"}),
                    error.to_string(),
                )?;
            }
        }
        return Ok(true);
    }
    if payload.phase == TopologyApplyPhase::Prepare {
        let validation = validate_prepared_bindings(
            &payload.bindings,
            &payload.topology_id,
            &payload.revision_id,
            &job.operation_id,
        )
        .and_then(|()| topology_binding_providers_healthy(storage, &payload.bindings));
        if let Err(detail) = validation {
            storage
                .finish_topology_apply(
                    &payload.topology_id,
                    &payload.revision_id,
                    &job.operation_id,
                    TopologyApplyOutcome::Failed,
                    &now_marker(),
                )
                .map_err(|error| format!("{detail}; prepare validation cleanup failed: {error}"))?;
            complete_and_project(
                storage,
                &mut jobs,
                &job.job_id,
                &job.operation_id,
                lease_token,
                CompletionStatus::Failed,
                serde_json::json!({
                    "code": "TOPOLOGY_BINDING_PREPARE_REJECTED",
                    "phase": "PREPARE",
                }),
                detail,
            )?;
            return Ok(true);
        }
        storage
            .replace_topology_api_bindings(&payload.topology_id, &payload.bindings)
            .map_err(|error| format!("prepare could not stage bindings: {error}"))?;
        lease_heartbeat.checkpoint(&mut jobs)?;
        let provider_result = provider.apply_with_bindings(
            &payload.topology_id,
            &payload.revision_id,
            revision.spec(),
            &payload.bindings,
            previous.as_ref().map(|revision| revision.revision_id()),
            previous.as_ref().map(|revision| revision.spec()),
            &payload.previous_bindings,
            &job.operation_id,
        );
        lease_heartbeat.checkpoint(&mut jobs)?;
        match provider_result {
            Ok(receipt) => {
                complete_and_project(
                    storage,
                    &mut jobs,
                    &job.job_id,
                    &job.operation_id,
                    lease_token,
                    CompletionStatus::Succeeded,
                    serde_json::json!({"phase": "PREPARE", "receipt": receipt}),
                    String::new(),
                )?;
            }
            Err(failure) => {
                let binding_compensation = storage.replace_topology_api_bindings(
                    &payload.topology_id,
                    &payload.previous_bindings,
                );
                let degraded = failure.state == TopologyProviderApplyState::Degraded
                    || binding_compensation.is_err();
                storage
                    .finish_topology_apply(
                        &payload.topology_id,
                        &payload.revision_id,
                        &job.operation_id,
                        if degraded {
                            TopologyApplyOutcome::Degraded
                        } else {
                            TopologyApplyOutcome::Failed
                        },
                        &now_marker(),
                    )
                    .map_err(|error| {
                        format!("prepare failure could not release apply ownership: {error}")
                    })?;
                complete_and_project(
                    storage,
                    &mut jobs,
                    &job.job_id,
                    &job.operation_id,
                    lease_token,
                    if degraded {
                        CompletionStatus::NeedsAttention
                    } else {
                        CompletionStatus::Failed
                    },
                    serde_json::to_value(&failure).map_err(|error| error.to_string())?,
                    format!(
                        "{failure}; binding compensation: {}",
                        binding_compensation
                            .err()
                            .map(|error| error.to_string())
                            .unwrap_or_else(|| "succeeded".to_string())
                    ),
                )?;
            }
        }
        return Ok(true);
    }
    let staged_bindings = match storage.resolve_topology_api_bindings(
        revision.spec(),
        &payload.revision_id,
        &job.operation_id,
    ) {
        Ok(bindings) => bindings,
        Err(error) => {
            let detail = format!("topology binding resolution failed: {error}");
            storage
                .finish_topology_apply(
                    &payload.topology_id,
                    &payload.revision_id,
                    &job.operation_id,
                    TopologyApplyOutcome::Failed,
                    &now_marker(),
                )
                .map_err(|finish| format!("{detail}; apply ownership cleanup failed: {finish}"))?;
            complete_and_project(
                storage,
                &mut jobs,
                &job.job_id,
                &job.operation_id,
                lease_token,
                CompletionStatus::Failed,
                serde_json::json!({"code": "TOPOLOGY_API_BINDING_INVALID"}),
                detail,
            )?;
            return Ok(true);
        }
    };
    if let Err(error) =
        storage.replace_topology_api_bindings(&payload.topology_id, &staged_bindings)
    {
        let detail = format!("topology bindings could not be staged atomically: {error}");
        storage
            .finish_topology_apply(
                &payload.topology_id,
                &payload.revision_id,
                &job.operation_id,
                TopologyApplyOutcome::Failed,
                &now_marker(),
            )
            .map_err(|finish| format!("{detail}; apply ownership cleanup failed: {finish}"))?;
        complete_and_project(
            storage,
            &mut jobs,
            &job.job_id,
            &job.operation_id,
            lease_token,
            CompletionStatus::Failed,
            serde_json::json!({"code": "TOPOLOGY_BINDING_STAGE_FAILED"}),
            detail,
        )?;
        return Ok(true);
    }

    // All provider I/O happens after the topology transaction that established
    // apply ownership has committed and before the completion transaction.
    lease_heartbeat.checkpoint(&mut jobs)?;
    let provider_result = provider.apply_with_bindings(
        &payload.topology_id,
        &payload.revision_id,
        revision.spec(),
        &staged_bindings,
        previous.as_ref().map(|revision| revision.revision_id()),
        previous.as_ref().map(|revision| revision.spec()),
        &previous_bindings,
        &job.operation_id,
    );
    lease_heartbeat.checkpoint(&mut jobs)?;
    match provider_result {
        Ok(receipt) => {
            let health_failure = topology_binding_providers_healthy(storage, &staged_bindings)
                .and_then(|()| topology_binding_consumers_healthy(storage, &staged_bindings))
                .err();
            if let Some(health_failure) = health_failure {
                let provider_compensation = provider.compensate_applied_revision(
                    &payload.topology_id,
                    &payload.revision_id,
                    previous.as_ref().map(|revision| revision.revision_id()),
                    previous.as_ref().map(|revision| revision.spec()),
                    &previous_bindings,
                    &job.operation_id,
                );
                let storage_compensation =
                    storage.replace_topology_api_bindings(&payload.topology_id, &previous_bindings);
                let degraded = provider_compensation.is_err() || storage_compensation.is_err();
                storage
                    .finish_topology_apply(
                        &payload.topology_id,
                        &payload.revision_id,
                        &job.operation_id,
                        if degraded {
                            TopologyApplyOutcome::Degraded
                        } else {
                            TopologyApplyOutcome::Failed
                        },
                        &now_marker(),
                    )
                    .map_err(|error| {
                        format!("health-gate failure could not be persisted: {error}")
                    })?;
                let detail = format!(
                    "consumer health gate failed ({health_failure}); provider compensation: {}; binding compensation: {}",
                    provider_compensation
                        .err()
                        .unwrap_or_else(|| "succeeded".to_string()),
                    storage_compensation
                        .err()
                        .map(|error| error.to_string())
                        .unwrap_or_else(|| "succeeded".to_string())
                );
                complete_and_project(
                    storage,
                    &mut jobs,
                    &job.job_id,
                    &job.operation_id,
                    lease_token,
                    if degraded {
                        CompletionStatus::NeedsAttention
                    } else {
                        CompletionStatus::Failed
                    },
                    serde_json::json!({"code": "TOPOLOGY_CONSUMER_UNHEALTHY"}),
                    detail,
                )?;
                return Ok(true);
            }
            let active_bindings = activate_staged_bindings(staged_bindings, &now_marker());
            if let Err(activation_error) =
                storage.replace_topology_api_bindings(&payload.topology_id, &active_bindings)
            {
                let provider_compensation = provider.compensate_applied_revision(
                    &payload.topology_id,
                    &payload.revision_id,
                    previous.as_ref().map(|revision| revision.revision_id()),
                    previous.as_ref().map(|revision| revision.spec()),
                    &previous_bindings,
                    &job.operation_id,
                );
                let storage_compensation =
                    storage.replace_topology_api_bindings(&payload.topology_id, &previous_bindings);
                let degraded = provider_compensation.is_err() || storage_compensation.is_err();
                storage
                    .finish_topology_apply(
                        &payload.topology_id,
                        &payload.revision_id,
                        &job.operation_id,
                        if degraded {
                            TopologyApplyOutcome::Degraded
                        } else {
                            TopologyApplyOutcome::Failed
                        },
                        &now_marker(),
                    )
                    .map_err(|error| {
                        format!("binding activation failure could not be persisted: {error}")
                    })?;
                complete_and_project(
                    storage,
                    &mut jobs,
                    &job.job_id,
                    &job.operation_id,
                    lease_token,
                    if degraded {
                        CompletionStatus::NeedsAttention
                    } else {
                        CompletionStatus::Failed
                    },
                    serde_json::json!({"code": "TOPOLOGY_BINDING_ACTIVATION_FAILED"}),
                    format!(
                        "binding activation failed ({activation_error}); provider compensation: {}; storage compensation: {}",
                        provider_compensation
                            .err()
                            .unwrap_or_else(|| "succeeded".to_string()),
                        storage_compensation
                            .err()
                            .map(|error| error.to_string())
                            .unwrap_or_else(|| "succeeded".to_string())
                    ),
                )?;
                return Ok(true);
            }
            lease_heartbeat.checkpoint(&mut jobs)?;
            storage
                .finish_topology_apply_fenced(
                    &payload.topology_id,
                    &payload.revision_id,
                    &job.operation_id,
                    TopologyApplyOutcome::Succeeded,
                    &now_marker(),
                    &job.job_id,
                    &lease_token,
                    now_ms(),
                )
                .map_err(|error| {
                    format!("providers accepted topology but durable head did not advance: {error}")
                })?;
            complete_and_project(
                storage,
                &mut jobs,
                &job.job_id,
                &job.operation_id,
                lease_token,
                CompletionStatus::Succeeded,
                serde_json::to_value(receipt).map_err(|error| error.to_string())?,
                String::new(),
            )?;
        }
        Err(failure) => {
            let binding_compensation =
                storage.replace_topology_api_bindings(&payload.topology_id, &previous_bindings);
            let degraded = failure.state == TopologyProviderApplyState::Degraded
                || binding_compensation.is_err();
            storage
                .finish_topology_apply(
                    &payload.topology_id,
                    &payload.revision_id,
                    &job.operation_id,
                    if degraded {
                        TopologyApplyOutcome::Degraded
                    } else {
                        TopologyApplyOutcome::Failed
                    },
                    &now_marker(),
                )
                .map_err(|error| {
                    format!("provider failure could not be persisted in topology status: {error}")
                })?;
            let detail = format!(
                "{}; binding compensation: {}",
                failure,
                binding_compensation
                    .err()
                    .map(|error| error.to_string())
                    .unwrap_or_else(|| "succeeded".to_string())
            );
            complete_and_project(
                storage,
                &mut jobs,
                &job.job_id,
                &job.operation_id,
                lease_token,
                if degraded {
                    CompletionStatus::NeedsAttention
                } else {
                    CompletionStatus::Failed
                },
                serde_json::to_value(failure).map_err(|error| error.to_string())?,
                detail,
            )?;
        }
    }
    Ok(true)
}

#[allow(clippy::too_many_arguments)]
pub(super) fn finalize_topology_group(
    storage: &DurableStore,
    provider: &TopologyProviderSaga,
    lease_heartbeat: &ControlPlaneLeaseHeartbeat,
    jobs: &mut crate::durable::DurableJobStore,
    job_id: &str,
    operation_id: &str,
    lease_token: String,
    group: &[TopologyApplyGroupPayloadMember],
) -> Result<bool, String> {
    let prepared = (|| -> Result<Vec<orchestrator_storage::TopologyApplyGroupMember>, String> {
        if group.is_empty() {
            return Err("FINALIZE_GROUP requires at least one topology member".to_string());
        }
        let mut identities = group
            .iter()
            .map(|member| (member.topology_id.clone(), member.revision_id.clone()))
            .collect::<Vec<_>>();
        identities.sort();
        if identities.iter().any(|(topology_id, revision_id)| {
            topology_id.trim().is_empty() || revision_id.trim().is_empty()
        }) || identities.windows(2).any(|pair| pair[0].0 == pair[1].0)
        {
            return Err(
                "FINALIZE_GROUP members must have unique non-empty topology identities".to_string(),
            );
        }
        let mut result = Vec::with_capacity(identities.len());
        for (topology_id, revision_id) in identities {
            lease_heartbeat.checkpoint(jobs)?;
            let heads = storage
                .topology_heads(&topology_id)
                .map_err(|error| error.to_string())?
                .ok_or_else(|| format!("topology {topology_id} disappeared"))?;
            if heads.applying_revision_id.as_deref() != Some(revision_id.as_str())
                || heads.applying_operation_id.as_deref() != Some(operation_id)
            {
                return Err(format!(
                    "topology {topology_id} no longer owns revision {revision_id} for operation {operation_id}"
                ));
            }
            let revision = storage
                .topology_revision(&topology_id, &revision_id)
                .map_err(|error| error.to_string())?
                .ok_or_else(|| format!("topology revision {revision_id} disappeared"))?;
            let staged_bindings = storage
                .api_bindings_for_topology(&topology_id)
                .map_err(|error| error.to_string())?;
            validate_prepared_bindings(&staged_bindings, &topology_id, &revision_id, operation_id)?;
            topology_binding_providers_healthy(storage, &staged_bindings)
                .and_then(|()| topology_binding_consumers_healthy(storage, &staged_bindings))?;
            let content_sha256 = revision
                .spec()
                .content_sha256()
                .map_err(|error| error.to_string())?;
            let projection_sha256 = provider_projection_sha256(&staged_bindings)?;
            let observed = provider.observe(&topology_id);
            lease_heartbeat.checkpoint(jobs)?;
            if !observed
                .gateway
                .matches(&revision_id, &content_sha256, &projection_sha256)
                || !observed
                    .auth
                    .matches(&revision_id, &content_sha256, &projection_sha256)
            {
                return Err(format!(
                    "topology {topology_id} provider evidence does not acknowledge revision {revision_id}"
                ));
            }
            result.push(orchestrator_storage::TopologyApplyGroupMember {
                topology_id,
                revision_id,
                active_bindings: activate_staged_bindings(staged_bindings, &now_marker()),
            });
        }
        normalize_group_binding_moves(&mut result);
        Ok(result)
    })();
    match prepared.and_then(|members| {
        lease_heartbeat.checkpoint(jobs)?;
        storage
            .finish_topology_apply_group_fenced(
                &members,
                operation_id,
                &now_marker(),
                job_id,
                &lease_token,
                now_ms(),
            )
            .map(|_| members)
            .map_err(|error| error.to_string())
    }) {
        Ok(members) => complete_and_project(
            storage,
            jobs,
            job_id,
            operation_id,
            lease_token,
            CompletionStatus::Succeeded,
            serde_json::json!({
                "phase": "FINALIZE_GROUP",
                "topologies": members.iter().map(|member| serde_json::json!({
                    "topology_id": member.topology_id,
                    "revision_id": member.revision_id,
                    "bindings": member.active_bindings.len(),
                })).collect::<Vec<_>>(),
            }),
            String::new(),
        )?,
        Err(detail) => complete_and_project(
            storage,
            jobs,
            job_id,
            operation_id,
            lease_token,
            CompletionStatus::Failed,
            serde_json::json!({
                "code": "TOPOLOGY_GROUP_FINALIZE_REJECTED",
                "phase": "FINALIZE_GROUP",
            }),
            detail,
        )?,
    }
    Ok(true)
}
