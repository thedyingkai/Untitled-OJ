//! Read-only, deployment-scoped projection compiled from the active
//! [`ContributionHeadV1`] records.
//!
//! The head is the only publication pointer. Gateway, Auth and both Web Shells
//! consume this same deterministic document, so activating or restoring a
//! revision never requires five independent writers to agree on ordering.

use crate::durable::DurableStore;
use orchestrator_legacy::{
    ContributionActivationStateV1, ContributionRevisionStatusV1, ContributionRevisionV1,
    parse_endpoint_id,
};
use orchestrator_runtime::{RuntimeDesiredState, RuntimeObservedState};
use orchestrator_storage::{ContributionRepository, RuntimeManagementMode};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};

pub(crate) const CONTRIBUTION_SNAPSHOT_SCHEMA_VERSION: &str = "ojos.dev/contribution-snapshot/v1";

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub(crate) enum ContributionProjectionExpectedStateV1 {
    Active,
    Restored,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct ContributionProjectionAcknowledgementV1 {
    pub activation_id: String,
    pub service_id: String,
    pub candidate_revision_id: String,
    pub candidate_generation: u64,
    pub expected_state: ContributionProjectionExpectedStateV1,
    pub observed_revision_id: String,
    pub observed_generation: u64,
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum ContributionSnapshotError {
    #[error("contribution snapshot storage failure: {0}")]
    Storage(String),
    #[error("active contribution invariant failed: {0}")]
    Invariant(String),
}

pub(crate) fn active_contribution_snapshot(
    storage: &DurableStore,
    scope_id: &str,
) -> Result<Value, ContributionSnapshotError> {
    let mut revisions = storage
        .contribution_revisions(scope_id, None)
        .map_err(|error| ContributionSnapshotError::Storage(error.to_string()))?;
    revisions.retain(|revision| revision.status() == ContributionRevisionStatusV1::Active);
    revisions.sort_by(|left, right| {
        left.service_id()
            .cmp(right.service_id())
            .then(left.generation().cmp(&right.generation()))
            .then(left.revision_id().cmp(right.revision_id()))
    });

    let mut active = Vec::new();
    for revision in revisions {
        let head = storage
            .contribution_head(scope_id, revision.service_id())
            .map_err(|error| ContributionSnapshotError::Storage(error.to_string()))?;
        if head.as_ref().map(|head| head.active_revision_id()) == Some(revision.revision_id()) {
            active.push((revision, head.expect("matching active head exists")));
        }
    }

    let mut revision_documents = Vec::with_capacity(active.len());
    let mut api_surfaces = Vec::new();
    let mut gateway_routes = Vec::new();
    let mut permission_definitions = Vec::new();
    let mut user_frontend_modules = Vec::new();
    let mut admin_frontend_modules = Vec::new();

    for (revision, head) in &active {
        let runtime = storage
            .runtime_instance(revision.deployment_id())
            .map_err(|error| ContributionSnapshotError::Storage(error.to_string()))?;
        let runtime = runtime
            .map(|runtime| storage.runtime_with_current_evidence(runtime, current_time_ms()))
            .transpose()
            .map_err(|error| ContributionSnapshotError::Storage(error.to_string()))?;
        let runtime_ready = runtime.as_ref().is_some_and(|runtime| {
            runtime.instance.service_id == revision.service_id()
                && runtime.instance.desired_state == RuntimeDesiredState::Running
                && runtime.instance.observed_state == RuntimeObservedState::Running
                && runtime.instance.health.eq_ignore_ascii_case("HEALTHY")
                && runtime.drift_reason.trim().is_empty()
                && (runtime.management_mode == RuntimeManagementMode::External
                    || runtime.instance.runtime_attested)
        });
        let upstream_base = runtime
            .as_ref()
            .and_then(|runtime| runtime_upstream_base(&runtime.endpoint));

        revision_documents.push(json!({
            "scope_id": revision.scope_id(),
            "service_id": revision.service_id(),
            "deployment_id": revision.deployment_id(),
            "revision_id": revision.revision_id(),
            "release_digest": revision.release_digest(),
            "contract_digest": revision.contract_digest(),
            "generation": revision.generation(),
            "head_generation": head.generation(),
            "head_etag": head.etag(),
            "runtime_ready": runtime_ready,
        }));

        for surface in revision.api_surfaces() {
            api_surfaces.push(json!({
                "service_id": revision.service_id(),
                "deployment_id": revision.deployment_id(),
                "revision_id": revision.revision_id(),
                "generation": revision.generation(),
                "api": surface,
                "runtime_ready": runtime_ready,
            }));
        }
        for route in revision.operation_routes() {
            gateway_routes.push(json!({
                "service_id": revision.service_id(),
                "deployment_id": revision.deployment_id(),
                "revision_id": revision.revision_id(),
                "generation": revision.generation(),
                "audience": route.audience,
                "method": route.method.as_str(),
                "path": route.path,
                "api_id": route.api_id,
                "operation_id": route.operation_id,
                "provider_path": route.provider_path,
                "auth": route.auth,
                "permission": route.permission,
                "permission_scope": route.permission_scope,
                "upstream_base": upstream_base,
                "enabled": runtime_ready,
            }));
        }
        for permission in revision.permission_definitions() {
            permission_definitions.push(json!({
                "service_id": revision.service_id(),
                "revision_id": revision.revision_id(),
                "generation": revision.generation(),
                "key": permission.key,
                "title": permission.title,
                "description": permission.description,
            }));
        }
        append_frontend_modules(
            &mut user_frontend_modules,
            revision,
            "user-shell",
            runtime_ready,
        );
        append_frontend_modules(
            &mut admin_frontend_modules,
            revision,
            "admin-shell",
            runtime_ready,
        );
    }

    sort_json_objects(&mut api_surfaces, &["service_id", "api", "api_id"]);
    sort_json_objects(
        &mut gateway_routes,
        &["audience", "method", "path", "service_id", "operation_id"],
    );
    sort_json_objects(&mut permission_definitions, &["key", "service_id"]);
    sort_json_objects(&mut user_frontend_modules, &["route", "module_id"]);
    sort_json_objects(&mut admin_frontend_modules, &["route", "module_id"]);

    // An acknowledgement is a challenge emitted from durable controller
    // state, never a claim invented by a service. It only appears after the
    // publication CAS (or restore CAS) is visible in the same snapshot the
    // consumer must atomically apply.
    let mut acknowledgements = Vec::new();
    for activation in storage
        .contribution_activations(scope_id)
        .map_err(|error| ContributionSnapshotError::Storage(error.to_string()))?
    {
        let expected_state = match activation.state() {
            ContributionActivationStateV1::Committing => {
                ContributionProjectionExpectedStateV1::Active
            }
            ContributionActivationStateV1::Compensating => {
                ContributionProjectionExpectedStateV1::Restored
            }
            _ => continue,
        };
        let candidate = storage
            .contribution_revision(activation.candidate_revision_id())
            .map_err(|error| ContributionSnapshotError::Storage(error.to_string()))?
            .ok_or_else(|| {
                ContributionSnapshotError::Invariant(format!(
                    "activation {} candidate revision is missing",
                    activation.activation_id()
                ))
            })?;
        let Some(head) = storage
            .contribution_head(scope_id, activation.service_id())
            .map_err(|error| ContributionSnapshotError::Storage(error.to_string()))?
        else {
            continue;
        };
        let publication_visible = match expected_state {
            ContributionProjectionExpectedStateV1::Active => {
                head.active_revision_id() == candidate.revision_id()
            }
            ContributionProjectionExpectedStateV1::Restored => {
                head.active_revision_id() != candidate.revision_id()
            }
        };
        if !publication_visible {
            continue;
        }
        acknowledgements.push(ContributionProjectionAcknowledgementV1 {
            activation_id: activation.activation_id().to_string(),
            service_id: activation.service_id().to_string(),
            candidate_revision_id: candidate.revision_id().to_string(),
            candidate_generation: candidate.generation(),
            expected_state,
            observed_revision_id: head.active_revision_id().to_string(),
            observed_generation: head.generation(),
        });
    }
    acknowledgements.sort_by(|left, right| {
        left.activation_id
            .cmp(&right.activation_id)
            .then(left.service_id.cmp(&right.service_id))
    });

    let projection = json!({
        "scope_id": scope_id,
        "acknowledgements": acknowledgements,
        "revisions": revision_documents,
        "api_surfaces": api_surfaces,
        "gateway_routes": gateway_routes,
        "permission_definitions": permission_definitions,
        "user_frontend_modules": user_frontend_modules,
        "admin_frontend_modules": admin_frontend_modules,
    });
    let digest = canonical_json_digest(&projection)?;
    let mut snapshot = Map::new();
    snapshot.insert(
        "schema_version".to_string(),
        Value::String(CONTRIBUTION_SNAPSHOT_SCHEMA_VERSION.to_string()),
    );
    snapshot.insert("digest".to_string(), Value::String(digest));
    let Value::Object(projection) = projection else {
        unreachable!("static contribution projection is an object")
    };
    snapshot.extend(projection);
    Ok(Value::Object(snapshot))
}

fn append_frontend_modules(
    output: &mut Vec<Value>,
    revision: &ContributionRevisionV1,
    target: &str,
    runtime_ready: bool,
) {
    let modules = if target == "user-shell" {
        revision.user_frontend_modules()
    } else {
        revision.admin_frontend_modules()
    };
    output.extend(modules.iter().map(|module| {
        json!({
            "service_id": revision.service_id(),
            "deployment_id": revision.deployment_id(),
            "revision_id": revision.revision_id(),
            "generation": revision.generation(),
            "target": target,
            "module_id": module.module_id,
            "surface_id": module.surface_id,
            "route": module.route,
            "menu_label": module.menu_label,
            "menu": module.menu,
            "order": module.order,
            "permission": module.permission,
            "artifact": module.artifact,
            "host_api_range": module.host_api_range,
            "manifest_digest": module.manifest_digest,
            "manifest_reference": module.manifest_reference,
            "bundle_digest": module.bundle_digest,
            "bundle_reference": module.bundle_reference,
            "enabled": runtime_ready,
        })
    }));
}

fn runtime_upstream_base(endpoint: &str) -> Option<String> {
    let identity = parse_endpoint_id(endpoint).ok()?;
    let host = identity
        .host
        .parse::<std::net::IpAddr>()
        .ok()
        .map(|host| match host {
            std::net::IpAddr::V4(_) => identity.host.to_string(),
            std::net::IpAddr::V6(_) => format!("[{}]", identity.host),
        })?;
    let port = identity.port.parse::<u16>().ok()?;
    Some(format!("http://{host}:{port}"))
}

fn sort_json_objects(values: &mut [Value], fields: &[&str]) {
    values.sort_by(|left, right| {
        fields
            .iter()
            .map(|field| json_sort_value(left.pointer(&format!("/{field}"))))
            .cmp(
                fields
                    .iter()
                    .map(|field| json_sort_value(right.pointer(&format!("/{field}")))),
            )
    });
}

fn json_sort_value(value: Option<&Value>) -> String {
    match value {
        Some(Value::String(value)) => value.clone(),
        Some(value) => value.to_string(),
        None => String::new(),
    }
}

fn canonical_json_digest(value: &Value) -> Result<String, ContributionSnapshotError> {
    let bytes = serde_json_canonicalizer::to_vec(value)
        .map_err(|error| ContributionSnapshotError::Invariant(error.to_string()))?;
    Ok(format!("sha256:{:x}", Sha256::digest(bytes)))
}

fn current_time_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(i64::MAX as u128) as i64
}

#[cfg(test)]
mod tests {
    use super::*;
    use orchestrator_legacy::{
        ContributionAudienceV1, ContributionPathPermissionScopeV1, ContributionPermissionScopeV1,
    };

    #[test]
    fn renders_ipv4_and_ipv6_runtime_endpoints_without_service_suffix() {
        assert_eq!(
            runtime_upstream_base("127.0.0.2:18080:contest-service").as_deref(),
            Some("http://127.0.0.2:18080")
        );
        assert_eq!(
            runtime_upstream_base("2001:db8::1:18080:contest-service").as_deref(),
            Some("http://[2001:db8::1]:18080")
        );
        assert!(runtime_upstream_base("https://untrusted.example").is_none());
    }

    #[test]
    fn snapshot_digest_is_deterministic_and_schema_scoped() {
        let left = json!({"scope_id":"default","revisions":[],"gateway_routes":[]});
        let right = json!({"scope_id":"default","revisions":[],"gateway_routes":[]});
        assert_eq!(
            canonical_json_digest(&left).unwrap(),
            canonical_json_digest(&right).unwrap()
        );
        assert_eq!(
            CONTRIBUTION_SNAPSHOT_SCHEMA_VERSION,
            "ojos.dev/contribution-snapshot/v1"
        );
    }

    #[test]
    fn audience_type_remains_serializable_for_snapshot_consumers() {
        assert_eq!(
            serde_json::to_value(ContributionAudienceV1::Admin).unwrap(),
            json!("ADMIN")
        );
    }

    #[test]
    fn permission_scope_wire_shape_matches_gateway_contract() {
        assert_eq!(
            serde_json::to_value(ContributionPermissionScopeV1::system()).unwrap(),
            json!("system")
        );
        assert_eq!(
            serde_json::to_value(ContributionPermissionScopeV1::PathParameter(
                ContributionPathPermissionScopeV1 {
                    scope_type: "contest".to_string(),
                    path_parameter: "contestId".to_string(),
                }
            ))
            .unwrap(),
            json!({"type":"contest","pathParameter":"contestId"})
        );
    }
}
