//! Background node lifecycle responsibilities.
use crate::durable::DurableStore;
use crate::topology_worker::context::{CONTROL_PLANE_NODE_ID, now_marker};
use crate::topology_worker::payload::NodeLifecyclePayload;
use orchestrator_control_plane::JobKind;
use serde_json::Value;

#[derive(Debug)]
pub(super) struct NodeLifecycleFailure {
    pub(super) code: &'static str,
    pub(super) detail: String,
}

pub(super) fn process_node_lifecycle(
    storage: &DurableStore,
    kind: &JobKind,
    payload: &Value,
) -> Result<Value, NodeLifecycleFailure> {
    let payload: NodeLifecyclePayload =
        serde_json::from_value(payload.clone()).map_err(|error| NodeLifecycleFailure {
            code: "INVALID_NODE_LIFECYCLE_PAYLOAD",
            detail: format!("invalid Node lifecycle payload: {error}"),
        })?;
    if payload.node_id.trim().is_empty() || payload.node_id == CONTROL_PLANE_NODE_ID {
        return Err(NodeLifecycleFailure {
            code: "INVALID_NODE_ID",
            detail: "Node lifecycle payload requires a non-control-plane node_id".to_string(),
        });
    }
    match kind {
        JobKind::NodeDrain => drain_node(storage, &payload.node_id),
        JobKind::NodeRemove => remove_node(storage, &payload.node_id),
        _ => Err(NodeLifecycleFailure {
            code: "INVALID_NODE_LIFECYCLE_KIND",
            detail: format!("job kind {kind:?} is not a Node lifecycle action"),
        }),
    }
}

pub(super) fn drain_node(
    storage: &DurableStore,
    node_id: &str,
) -> Result<Value, NodeLifecycleFailure> {
    let mut node = storage
        .get_node(node_id)
        .map_err(node_storage_failure)?
        .ok_or_else(|| NodeLifecycleFailure {
            code: "NODE_NOT_FOUND",
            detail: format!("node {node_id} was not found"),
        })?;
    let original_status = node.status.to_ascii_uppercase();
    if original_status == "DRAINED" {
        return Ok(serde_json::json!({"node": node, "already_drained": true}));
    }
    if !matches!(original_status.as_str(), "READY" | "DRAINING") {
        return Err(NodeLifecycleFailure {
            code: "NODE_STATE_CONFLICT",
            detail: format!("node {node_id} cannot drain from state {}", node.status),
        });
    }
    if original_status == "READY" {
        node.status = "DRAINING".to_string();
        node.updated_at = now_marker();
        storage
            .upsert_node(node.clone())
            .map_err(node_storage_failure)?;
    }
    let active_jobs = storage
        .job_store()
        .active_job_count(node_id)
        .map_err(|error| NodeLifecycleFailure {
            code: "NODE_JOB_STATE_ERROR",
            detail: error.to_string(),
        })?;
    let runtime_instances = storage
        .runtime_instances(Some(node_id))
        .map_err(node_storage_failure)?;
    if active_jobs != 0 || !runtime_instances.is_empty() {
        // A job/deployment raced the preflight. Restore admission only when
        // this operation was the writer that changed READY -> DRAINING.
        if original_status == "READY" {
            node.status = "READY".to_string();
            node.updated_at = now_marker();
            storage.upsert_node(node).map_err(node_storage_failure)?;
        }
        return Err(NodeLifecycleFailure {
            code: "NODE_NOT_EMPTY",
            detail: format!(
                "node {node_id} owns {active_jobs} active jobs and {} runtime instances",
                runtime_instances.len()
            ),
        });
    }
    node.status = "DRAINED".to_string();
    node.updated_at = now_marker();
    storage
        .upsert_node(node.clone())
        .map_err(node_storage_failure)?;
    Ok(serde_json::json!({
        "node": node,
        "active_jobs": 0,
        "runtime_instances": 0,
    }))
}

pub(super) fn remove_node(
    storage: &DurableStore,
    node_id: &str,
) -> Result<Value, NodeLifecycleFailure> {
    let Some(node) = storage.get_node(node_id).map_err(node_storage_failure)? else {
        return Ok(serde_json::json!({"node_id": node_id, "already_absent": true}));
    };
    if !node.status.eq_ignore_ascii_case("DRAINED") {
        return Err(NodeLifecycleFailure {
            code: "NODE_NOT_DRAINED",
            detail: format!("node {node_id} must be DRAINED before removal"),
        });
    }
    let active_jobs = storage
        .job_store()
        .active_job_count(node_id)
        .map_err(|error| NodeLifecycleFailure {
            code: "NODE_JOB_STATE_ERROR",
            detail: error.to_string(),
        })?;
    let runtime_instances = storage
        .runtime_instances(Some(node_id))
        .map_err(node_storage_failure)?;
    if active_jobs != 0 || !runtime_instances.is_empty() {
        return Err(NodeLifecycleFailure {
            code: "NODE_NOT_EMPTY",
            detail: format!(
                "node {node_id} owns {active_jobs} active jobs and {} runtime instances",
                runtime_instances.len()
            ),
        });
    }
    storage.delete_node(node_id).map_err(node_storage_failure)?;
    Ok(serde_json::json!({"node_id": node_id, "removed": true}))
}

pub(super) fn node_storage_failure(error: crate::durable::DurableError) -> NodeLifecycleFailure {
    NodeLifecycleFailure {
        code: "NODE_STORAGE_ERROR",
        detail: error.to_string(),
    }
}
