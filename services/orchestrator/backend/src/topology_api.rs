use crate::durable::{DurableError, DurableStore, LinkProbeBindingError, TopologyApiBindingError};
use crate::http::{ApiRequest, ApiResponse, path_segments, query_value};
use crate::store_v1_api::{
    InstallTopologySelection, StoreTopologyApplyPlan, align_group_binding_generations,
    binding_context_transition_plans, propose_generation_sibling_topology, selected_topology_spec,
};
use crate::topology_provider::TopologyProviderSaga;
use orchestrator_control_plane::{
    JobKind, OperationCoordinator, PlanOperation, PlannedJob, PlannedJobCondition,
};
use orchestrator_legacy::{
    TopologyEndpointSpec, TopologyLinkSpec, TopologySpec, diff_topology_revisions,
};
use orchestrator_storage::TopologyApplyOutcome;
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct DiffRequest {
    #[serde(default)]
    from_revision_id: Option<String>,
    #[serde(default)]
    to_revision_id: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RollbackRequest {
    revision_id: String,
    #[serde(default)]
    topologies: Vec<ApplyTopologyCas>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct ApplyRequest {
    #[serde(default)]
    topologies: Vec<ApplyTopologyCas>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ApplyTopologyCas {
    topology_id: String,
    topology_etag: String,
}

pub(crate) fn route(
    store: Option<&DurableStore>,
    provider: Option<&TopologyProviderSaga>,
    request: &ApiRequest,
    request_id: &str,
) -> Option<ApiResponse> {
    let path = request.path.split('?').next().unwrap_or("/");
    if !path.starts_with("/api/v1/topologies") {
        return None;
    }
    let segments = match path_segments(path) {
        Ok(segments) => segments,
        Err(error) => {
            return Some(problem(400, "INVALID_PATH", error.to_string(), request_id));
        }
    };
    let segments = segments.iter().map(String::as_str).collect::<Vec<_>>();
    if segments.get(0..3) != Some(&["api", "v1", "topologies"]) {
        return None;
    }
    let Some(store) = store else {
        return Some(problem(
            503,
            "TOPOLOGY_STORAGE_UNAVAILABLE",
            "Topology v1 requires the transactional SQLite or PostgreSQL repository",
            request_id,
        ));
    };
    Some(
        match route_with_store(store, provider, request, &segments, request_id) {
            Ok(response) => response,
            Err(error) => problem(error.status, error.code, error.detail, request_id),
        },
    )
}

fn route_with_store(
    store: &DurableStore,
    provider: Option<&TopologyProviderSaga>,
    request: &ApiRequest,
    segments: &[&str],
    request_id: &str,
) -> Result<ApiResponse, TopologyApiError> {
    match (request.method.as_str(), segments) {
        ("GET", ["api", "v1", "topologies"]) => {
            let (cursor, limit) = page_request(request)?;
            let mut heads = store.list_topology_heads().map_err(storage_error)?;
            heads.sort_by(|left, right| left.topology_id.cmp(&right.topology_id));
            let mut heads = heads
                .into_iter()
                .filter(|heads| heads.topology_id.as_str() > cursor.as_str())
                .take(limit + 1)
                .collect::<Vec<_>>();
            let next_cursor = if heads.len() > limit {
                heads.truncate(limit);
                heads.last().map(|heads| heads.topology_id.clone())
            } else {
                None
            };
            Ok(success(
                200,
                json!({"items": heads, "next_cursor": next_cursor}),
                request_id,
            ))
        }
        ("POST", ["api", "v1", "topologies"]) => {
            let spec = parse_spec(&request.body)?;
            validate_registered_services(store, &spec)?;
            let revision = store
                .create_initial_topology_revision(
                    spec,
                    now_marker(),
                    actor(request),
                    change_message(request, "initial draft"),
                )
                .map_err(storage_error)?;
            Ok(success(201, json!({"revision": revision}), request_id)
                .with_header("ETag", etag(revision.revision_id())))
        }
        ("GET", ["api", "v1", "topologies", topology_id]) => {
            let heads = store
                .topology_heads(topology_id)
                .map_err(storage_error)?
                .ok_or_else(|| not_found("topology", topology_id))?;
            let revision = store
                .topology_revision(topology_id, &heads.draft_revision_id)
                .map_err(storage_error)?
                .ok_or_else(|| not_found("revision", &heads.draft_revision_id))?;
            let status = store.topology_status(topology_id).map_err(storage_error)?;
            Ok(success(
                200,
                json!({"heads": heads, "draft": revision, "status": status}),
                request_id,
            )
            .with_header("ETag", etag(revision.revision_id())))
        }
        ("GET", ["api", "v1", "topologies", topology_id, "revisions"]) => {
            let (cursor, limit) = page_request(request)?;
            let mut revisions = store
                .topology_revisions(topology_id)
                .map_err(storage_error)?;
            revisions.sort_by(|left, right| left.revision_id().cmp(right.revision_id()));
            let mut revisions = revisions
                .into_iter()
                .filter(|revision| revision.revision_id() > cursor.as_str())
                .take(limit + 1)
                .collect::<Vec<_>>();
            let next_cursor = if revisions.len() > limit {
                revisions.truncate(limit);
                revisions
                    .last()
                    .map(|revision| revision.revision_id().to_string())
            } else {
                None
            };
            Ok(success(
                200,
                json!({"items": revisions, "next_cursor": next_cursor}),
                request_id,
            ))
        }
        ("POST", ["api", "v1", "topologies", topology_id, "revisions"]) => {
            let expected = required_if_match(request)?;
            let spec = parse_spec(&request.body)?;
            if spec.topology_id != *topology_id {
                return Err(invalid("path topology_id must match body topology_id"));
            }
            validate_registered_services(store, &spec)?;
            let revision = store
                .create_next_topology_revision(
                    topology_id,
                    &expected,
                    spec,
                    now_marker(),
                    actor(request),
                    change_message(request, "draft update"),
                )
                .map_err(storage_error)?;
            Ok(success(201, json!({"revision": revision}), request_id)
                .with_header("ETag", etag(revision.revision_id())))
        }
        (
            "PUT",
            [
                "api",
                "v1",
                "topologies",
                topology_id,
                "draft",
                "endpoints",
                endpoint_id,
            ],
        ) => {
            let expected = required_if_match(request)?;
            let endpoint: TopologyEndpointSpec =
                serde_json::from_str(&request.body).map_err(json_error)?;
            if endpoint.endpoint != *endpoint_id {
                return Err(invalid("path endpointId must match body endpoint identity"));
            }
            let mut spec = current_draft_spec(store, topology_id, &expected)?;
            if let Some(existing) = spec
                .endpoints
                .iter_mut()
                .find(|candidate| candidate.endpoint == *endpoint_id)
            {
                *existing = endpoint.clone();
            } else {
                spec.endpoints.push(endpoint.clone());
            }
            let revision = persist_draft_edit(
                store,
                topology_id,
                &expected,
                spec,
                request,
                "endpoint draft edit",
            )?;
            Ok(success(
                201,
                json!({"revision": revision, "endpoint": endpoint}),
                request_id,
            )
            .with_header("ETag", etag(revision.revision_id())))
        }
        (
            "DELETE",
            [
                "api",
                "v1",
                "topologies",
                topology_id,
                "draft",
                "endpoints",
                endpoint_id,
            ],
        ) => {
            let expected = required_if_match(request)?;
            let mut spec = current_draft_spec(store, topology_id, &expected)?;
            if spec.root_endpoint == *endpoint_id {
                return Err(TopologyApiError {
                    status: 409,
                    code: "TOPOLOGY_ENDPOINT_IN_USE",
                    detail: "the root endpoint cannot be deleted; select a different root in a full draft revision first"
                        .to_string(),
                });
            }
            if spec.links.iter().any(|link| {
                link.source_endpoint == *endpoint_id || link.target_endpoint == *endpoint_id
            }) {
                return Err(TopologyApiError {
                    status: 409,
                    code: "TOPOLOGY_ENDPOINT_IN_USE",
                    detail: "delete links that reference the endpoint before deleting the endpoint"
                        .to_string(),
                });
            }
            let previous_len = spec.endpoints.len();
            spec.endpoints
                .retain(|candidate| candidate.endpoint != *endpoint_id);
            if spec.endpoints.len() == previous_len {
                return Err(not_found("endpoint", endpoint_id));
            }
            let revision = persist_draft_edit(
                store,
                topology_id,
                &expected,
                spec,
                request,
                "endpoint draft delete",
            )?;
            Ok(success(
                201,
                json!({"revision": revision, "deleted_endpoint": endpoint_id}),
                request_id,
            )
            .with_header("ETag", etag(revision.revision_id())))
        }
        (
            "PUT",
            [
                "api",
                "v1",
                "topologies",
                topology_id,
                "draft",
                "links",
                source_endpoint,
                target_endpoint,
            ],
        ) => {
            let expected = required_if_match(request)?;
            let link: TopologyLinkSpec = serde_json::from_str(&request.body).map_err(json_error)?;
            if link.source_endpoint != *source_endpoint || link.target_endpoint != *target_endpoint
            {
                return Err(invalid(
                    "path sourceEndpoint/targetEndpoint must match body link identity",
                ));
            }
            let mut spec = current_draft_spec(store, topology_id, &expected)?;
            if let Some(existing) = spec.links.iter_mut().find(|candidate| {
                candidate.source_endpoint == *source_endpoint
                    && candidate.target_endpoint == *target_endpoint
            }) {
                *existing = link.clone();
            } else {
                spec.links.push(link.clone());
            }
            let revision = persist_draft_edit(
                store,
                topology_id,
                &expected,
                spec,
                request,
                "link draft edit",
            )?;
            Ok(
                success(201, json!({"revision": revision, "link": link}), request_id)
                    .with_header("ETag", etag(revision.revision_id())),
            )
        }
        (
            "DELETE",
            [
                "api",
                "v1",
                "topologies",
                topology_id,
                "draft",
                "links",
                source_endpoint,
                target_endpoint,
            ],
        ) => {
            let expected = required_if_match(request)?;
            let mut spec = current_draft_spec(store, topology_id, &expected)?;
            let previous_len = spec.links.len();
            spec.links.retain(|candidate| {
                candidate.source_endpoint != *source_endpoint
                    || candidate.target_endpoint != *target_endpoint
            });
            if spec.links.len() == previous_len {
                return Err(not_found(
                    "link",
                    &format!("{source_endpoint}->{target_endpoint}"),
                ));
            }
            let revision = persist_draft_edit(
                store,
                topology_id,
                &expected,
                spec,
                request,
                "link draft delete",
            )?;
            Ok(success(
                201,
                json!({
                    "revision": revision,
                    "deleted_link": {
                        "source_endpoint": source_endpoint,
                        "target_endpoint": target_endpoint,
                    }
                }),
                request_id,
            )
            .with_header("ETag", etag(revision.revision_id())))
        }
        (
            "GET",
            [
                "api",
                "v1",
                "topologies",
                topology_id,
                "revisions",
                revision_id,
            ],
        ) => {
            let revision = store
                .topology_revision(topology_id, revision_id)
                .map_err(storage_error)?
                .ok_or_else(|| not_found("revision", revision_id))?;
            Ok(success(200, json!({"revision": revision}), request_id)
                .with_header("ETag", etag(revision.revision_id())))
        }
        ("GET", ["api", "v1", "topologies", topology_id, "status"]) => {
            let status = store
                .topology_status(topology_id)
                .map_err(storage_error)?
                .ok_or_else(|| not_found("topology status", topology_id))?;
            Ok(success(200, json!({"status": status}), request_id))
        }
        ("POST", ["api", "v1", "topologies", topology_action])
            if topology_action.ends_with(":validate") =>
        {
            let topology_id = topology_action.trim_end_matches(":validate");
            let spec = parse_spec(&request.body)?;
            if spec.topology_id != topology_id {
                return Err(invalid("path topology_id must match body topology_id"));
            }
            validate_registered_services(store, &spec)?;
            Ok(success(
                200,
                json!({
                    "valid": true,
                    "content_sha256": spec.content_sha256().map_err(domain_error)?,
                }),
                request_id,
            ))
        }
        ("POST", ["api", "v1", "topologies", topology_action])
            if topology_action.ends_with(":diff") =>
        {
            let topology_id = topology_action.trim_end_matches(":diff");
            let body: DiffRequest = if request.body.trim().is_empty() {
                DiffRequest {
                    from_revision_id: None,
                    to_revision_id: None,
                }
            } else {
                serde_json::from_str(&request.body).map_err(json_error)?
            };
            let heads = store
                .topology_heads(topology_id)
                .map_err(storage_error)?
                .ok_or_else(|| not_found("topology", topology_id))?;
            let to_revision_id = body
                .to_revision_id
                .as_deref()
                .unwrap_or(&heads.draft_revision_id);
            let from_revision_id = body
                .from_revision_id
                .as_deref()
                .or(heads.applied_revision_id.as_deref());
            let from = from_revision_id
                .map(|revision_id| {
                    store
                        .topology_revision(topology_id, revision_id)
                        .map_err(storage_error)?
                        .ok_or_else(|| not_found("revision", revision_id))
                })
                .transpose()?;
            let to = store
                .topology_revision(topology_id, to_revision_id)
                .map_err(storage_error)?
                .ok_or_else(|| not_found("revision", to_revision_id))?;
            let diff = diff_topology_revisions(from.as_ref(), &to).map_err(domain_error)?;
            Ok(success(200, json!({"diff": diff}), request_id))
        }
        ("POST", ["api", "v1", "topologies", topology_action])
            if topology_action.ends_with(":apply") =>
        {
            require_provider(provider)?;
            let topology_id = topology_action.trim_end_matches(":apply");
            let revision_id = required_if_match(request)?;
            let apply_request = if request.body.trim().is_empty() {
                ApplyRequest::default()
            } else {
                serde_json::from_str::<ApplyRequest>(&request.body).map_err(json_error)?
            };
            enqueue_apply(
                store,
                topology_id,
                &revision_id,
                "topology.apply",
                request,
                request_id,
                apply_request,
            )
        }
        ("POST", ["api", "v1", "topologies", topology_action])
            if topology_action.ends_with(":rollback") =>
        {
            require_provider(provider)?;
            let topology_id = topology_action.trim_end_matches(":rollback");
            let expected = required_if_match(request)?;
            let body: RollbackRequest = serde_json::from_str(&request.body).map_err(json_error)?;
            if body.revision_id.trim().is_empty() {
                return Err(invalid("rollback revision_id is required"));
            }
            let rollback_target = store
                .topology_revision(topology_id, &body.revision_id)
                .map_err(storage_error)?
                .ok_or_else(|| not_found("revision", &body.revision_id))?;
            validate_registered_services(store, rollback_target.spec())?;
            let revision = store
                .create_topology_rollback_revision(
                    topology_id,
                    &expected,
                    &body.revision_id,
                    now_marker(),
                    actor(request),
                    change_message(request, "rollback draft"),
                )
                .map_err(storage_error)?;
            enqueue_apply(
                store,
                topology_id,
                revision.revision_id(),
                "topology.rollback",
                request,
                request_id,
                ApplyRequest {
                    topologies: body.topologies,
                },
            )
        }
        _ => Err(TopologyApiError {
            status: 404,
            code: "ROUTE_NOT_FOUND",
            detail: "the requested topology v1 route does not exist".to_string(),
        }),
    }
}

fn page_request(request: &ApiRequest) -> Result<(String, usize), TopologyApiError> {
    let query = request
        .path
        .split_once('?')
        .map(|(_, query)| query)
        .unwrap_or("");
    let cursor = query_value(query, "cursor")
        .map_err(|error| invalid(error.to_string()))?
        .unwrap_or_default();
    let limit = query_value(query, "limit")
        .map_err(|error| invalid(error.to_string()))?
        .map(|value| value.parse::<usize>())
        .transpose()
        .map_err(|_| invalid("limit must be an integer"))?
        .unwrap_or(50);
    if !(1..=200).contains(&limit) {
        return Err(invalid("limit must be between 1 and 200"));
    }
    Ok((cursor, limit))
}

fn require_provider(
    provider: Option<&TopologyProviderSaga>,
) -> Result<&TopologyProviderSaga, TopologyApiError> {
    provider.ok_or_else(|| TopologyApiError {
        status: 422,
        code: "TOPOLOGY_PROVIDER_MISSING",
        detail: "Topology apply requires configured Gateway and Auth management providers; the draft was not changed".to_string(),
    })
}

fn current_draft_spec(
    store: &DurableStore,
    topology_id: &str,
    expected_revision_id: &str,
) -> Result<TopologySpec, TopologyApiError> {
    let heads = store
        .topology_heads(topology_id)
        .map_err(storage_error)?
        .ok_or_else(|| not_found("topology", topology_id))?;
    if heads.draft_revision_id != expected_revision_id {
        return Err(TopologyApiError {
            status: 409,
            code: "TOPOLOGY_REVISION_CONFLICT",
            detail: format!(
                "expected draft {expected_revision_id}, current draft is {}",
                heads.draft_revision_id
            ),
        });
    }
    let revision = store
        .topology_revision(topology_id, expected_revision_id)
        .map_err(storage_error)?
        .ok_or_else(|| not_found("revision", expected_revision_id))?;
    Ok(revision.spec().clone())
}

fn persist_draft_edit(
    store: &DurableStore,
    topology_id: &str,
    expected_revision_id: &str,
    spec: TopologySpec,
    request: &ApiRequest,
    fallback_message: &str,
) -> Result<orchestrator_legacy::TopologyRevision, TopologyApiError> {
    validate_registered_services(store, &spec)?;
    store
        .create_next_topology_revision(
            topology_id,
            expected_revision_id,
            spec,
            now_marker(),
            actor(request),
            change_message(request, fallback_message),
        )
        .map_err(storage_error)
}

fn enqueue_apply(
    store: &DurableStore,
    topology_id: &str,
    revision_id: &str,
    action: &str,
    request: &ApiRequest,
    request_id: &str,
    apply_request: ApplyRequest,
) -> Result<ApiResponse, TopologyApiError> {
    let heads = store
        .topology_heads(topology_id)
        .map_err(storage_error)?
        .ok_or_else(|| not_found("topology", topology_id))?;
    if heads.draft_revision_id != revision_id {
        return Err(TopologyApiError {
            status: 409,
            code: "TOPOLOGY_REVISION_CONFLICT",
            detail: format!(
                "requested revision {revision_id} is not current draft {}",
                heads.draft_revision_id
            ),
        });
    }
    let revision = store
        .topology_revision(topology_id, revision_id)
        .map_err(storage_error)?
        .ok_or_else(|| not_found("revision", revision_id))?;
    validate_registered_services(store, revision.spec())?;
    let idempotency_key = request
        .headers
        .get("idempotency-key")
        .map(String::as_str)
        .unwrap_or_default();
    let digest = Sha256::digest(
        format!("{action}\0{topology_id}\0{revision_id}\0{idempotency_key}").as_bytes(),
    );
    let operation_id = format!("op-topology-{digest:x}");
    let previous_bindings = store
        .api_bindings_for_topology(topology_id)
        .map_err(storage_error)?;
    let staged_bindings = store
        .resolve_topology_api_bindings(revision.spec(), revision_id, &operation_id)
        .map_err(|error| TopologyApiError {
            status: 422,
            code: "TOPOLOGY_API_BINDING_INVALID",
            detail: error.to_string(),
        })?;
    let affected_consumers = previous_bindings
        .iter()
        .chain(staged_bindings.iter())
        .map(|binding| binding.consumer_deployment_id.clone())
        .filter(|deployment_id| !deployment_id.is_empty())
        .collect::<BTreeSet<_>>();
    let mut sibling_topology_ids = BTreeSet::new();
    for consumer in &affected_consumers {
        for binding in store
            .api_bindings_for_deployment(consumer)
            .map_err(storage_error)?
            .into_iter()
            .filter(|binding| {
                binding.desired_state == "ACTIVE"
                    && binding.state == orchestrator_storage::ApiBindingState::Active
                    && binding.topology_id != topology_id
            })
        {
            sibling_topology_ids.insert(binding.topology_id);
        }
    }
    let mut sibling_selections = BTreeMap::new();
    for selection in apply_request.topologies {
        let topology_id = selection.topology_id.trim();
        let revision_id = strong_etag_value(&selection.topology_etag)?;
        if topology_id.is_empty()
            || sibling_selections
                .insert(
                    topology_id.to_string(),
                    InstallTopologySelection {
                        topology_id: topology_id.to_string(),
                        revision_id,
                    },
                )
                .is_some()
        {
            return Err(invalid(
                "topologies must contain unique non-empty topology_id values",
            ));
        }
    }
    let supplied_siblings = sibling_selections.keys().cloned().collect::<BTreeSet<_>>();
    if supplied_siblings != sibling_topology_ids {
        return Err(TopologyApiError {
            status: 409,
            code: "TOPOLOGY_SIBLING_CAS_REQUIRED",
            detail: format!(
                "deployment-wide credential generation requires exact sibling topology CAS set {:?}; supplied {:?}",
                sibling_topology_ids, supplied_siblings
            ),
        });
    }
    // Validate every sibling head before creating any generation-only
    // immutable revision, preventing a late stale CAS from leaving drafts.
    for selection in sibling_selections.values() {
        selected_topology_spec(store, selection).map_err(store_topology_error)?;
    }
    let mut topology_applies = vec![StoreTopologyApplyPlan {
        topology_id: topology_id.to_string(),
        revision_id: revision_id.to_string(),
        staged_bindings,
        previous_bindings,
    }];
    for selection in sibling_selections.values() {
        topology_applies.push(
            propose_generation_sibling_topology(store, selection, &operation_id)
                .map_err(store_topology_error)?,
        );
    }
    topology_applies.sort_by(|left, right| left.topology_id.cmp(&right.topology_id));
    align_group_binding_generations(store, &mut topology_applies, &affected_consumers)
        .map_err(store_topology_error)?;
    let context_transitions =
        binding_context_transition_plans(store, &topology_applies, &affected_consumers)
            .map_err(store_topology_error)?;
    let prepare_steps = topology_applies
        .iter()
        .enumerate()
        .map(|(index, _)| format!("topology-binding-prepare-{index}"))
        .collect::<Vec<_>>();
    let finalize_step = "topology-binding-finalize-group".to_string();
    let abort_steps = topology_applies
        .iter()
        .enumerate()
        .map(|(index, _)| format!("topology-binding-abort-{index}"))
        .collect::<Vec<_>>();
    let context_steps = context_transitions
        .iter()
        .enumerate()
        .map(|(index, _)| format!("binding-context-apply-{index}"))
        .collect::<Vec<_>>();
    let health_steps = context_transitions
        .iter()
        .enumerate()
        .map(|(index, _)| format!("binding-context-health-{index}"))
        .collect::<Vec<_>>();
    let mut jobs = Vec::new();
    for (index, topology) in topology_applies.iter().enumerate() {
        jobs.push(PlannedJob {
            step_id: prepare_steps[index].clone(),
            node_id: "control-plane".to_string(),
            kind: JobKind::TopologyApply,
            depends_on: vec![],
            condition: PlannedJobCondition::OnSuccess,
            payload: json!({
                "topology_id": topology.topology_id,
                "revision_id": topology.revision_id,
                "phase": "PREPARE",
                "bindings": topology.staged_bindings,
                "previous_bindings": topology.previous_bindings,
            }),
            max_attempts: 1,
        });
    }
    for (index, transition) in context_transitions.iter().enumerate() {
        jobs.push(PlannedJob {
            step_id: context_steps[index].clone(),
            node_id: transition.node_id.clone(),
            kind: JobKind::BindingContextApply,
            depends_on: prepare_steps.clone(),
            condition: PlannedJobCondition::OnSuccess,
            payload: serde_json::to_value(&transition.forward).map_err(json_error_value)?,
            max_attempts: 1,
        });
        jobs.push(PlannedJob {
            step_id: health_steps[index].clone(),
            node_id: transition.node_id.clone(),
            kind: JobKind::Health,
            depends_on: vec![context_steps[index].clone()],
            condition: PlannedJobCondition::OnSuccess,
            payload: json!({"container_id": transition.container_id}),
            max_attempts: 3,
        });
    }
    let mut finalize_dependencies = prepare_steps.clone();
    finalize_dependencies.extend(health_steps.iter().cloned());
    jobs.push(PlannedJob {
        step_id: finalize_step.clone(),
        node_id: "control-plane".to_string(),
        kind: JobKind::TopologyApply,
        depends_on: finalize_dependencies.clone(),
        condition: PlannedJobCondition::OnSuccess,
        payload: json!({
            "phase": "FINALIZE_GROUP",
            "group": topology_applies.iter().map(|topology| json!({
                "topology_id": topology.topology_id,
                "revision_id": topology.revision_id,
            })).collect::<Vec<_>>(),
        }),
        max_attempts: 1,
    });
    let mut abort_dependencies = finalize_dependencies;
    abort_dependencies.extend(context_steps.iter().cloned());
    abort_dependencies.push(finalize_step);
    for (index, topology) in topology_applies.iter().enumerate() {
        jobs.push(PlannedJob {
            step_id: abort_steps[index].clone(),
            node_id: "control-plane".to_string(),
            kind: JobKind::TopologyApply,
            depends_on: abort_dependencies.clone(),
            condition: PlannedJobCondition::OnFailure,
            payload: json!({
                "topology_id": topology.topology_id,
                "revision_id": topology.revision_id,
                "phase": "ABORT",
                "bindings": topology.staged_bindings,
                "previous_bindings": topology.previous_bindings,
            }),
            max_attempts: 1,
        });
    }
    for (index, transition) in context_transitions.iter().enumerate() {
        let mut depends_on = abort_steps.clone();
        depends_on.push(context_steps[index].clone());
        jobs.push(PlannedJob {
            step_id: format!("binding-context-rollback-{index}"),
            node_id: transition.node_id.clone(),
            kind: JobKind::BindingContextApply,
            depends_on,
            condition: PlannedJobCondition::OnSuccess,
            payload: serde_json::to_value(&transition.rollback).map_err(json_error_value)?,
            max_attempts: 1,
        });
    }
    let plan = PlanOperation {
        operation_id: operation_id.clone(),
        action: action.to_string(),
        target_type: "Topology".to_string(),
        target_id: topology_id.to_string(),
        request: json!({
            "topology_id": topology_id,
            "revision_id": revision_id,
            "auto_enqueue": true,
        }),
        jobs,
    };
    let mut operations = store.operation_store();
    let mut jobs = store.job_store();
    let mut coordinator = OperationCoordinator::new(&mut operations, &mut jobs);
    coordinator.plan(plan, now_ms()).map_err(operation_error)?;
    coordinator
        .confirm(&operation_id, now_ms())
        .map_err(operation_error)?;
    let mut begun: Vec<&StoreTopologyApplyPlan> = Vec::new();
    for topology in &topology_applies {
        if let Err(error) = store.begin_topology_apply(
            &topology.topology_id,
            &topology.revision_id,
            &operation_id,
            &now_marker(),
        ) {
            for prior in begun.iter().rev() {
                let _ = store.finish_topology_apply(
                    &prior.topology_id,
                    &prior.revision_id,
                    &operation_id,
                    TopologyApplyOutcome::Failed,
                    &now_marker(),
                );
            }
            return Err(storage_error(error));
        }
        begun.push(topology);
    }
    let operation = match coordinator.enqueue(&operation_id, now_ms()) {
        Ok(operation) => operation,
        Err(error) => {
            for topology in begun.iter().rev() {
                let _ = store.finish_topology_apply(
                    &topology.topology_id,
                    &topology.revision_id,
                    &operation_id,
                    TopologyApplyOutcome::Failed,
                    &now_marker(),
                );
            }
            return Err(operation_error(error));
        }
    };
    Ok(success(
        202,
        json!({
            "operation_id": operation_id,
            "operation": operation,
            "topology_id": topology_id,
            "revision_id": revision_id,
        }),
        request_id,
    ))
}

fn validate_registered_services(
    store: &DurableStore,
    spec: &TopologySpec,
) -> Result<(), TopologyApiError> {
    let registered = store
        .registered_service_ids()
        .map_err(|error| TopologyApiError {
            status: 500,
            code: "TOPOLOGY_STORAGE_ERROR",
            detail: error.to_string(),
        })?;
    spec.validate_against_registered_services(&registered)
        .map_err(domain_error)?;
    match store.link_probe_source_endpoints(spec) {
        Ok(_) => Ok(()),
        Err(LinkProbeBindingError::Binding(detail)) => Err(TopologyApiError {
            status: 422,
            code: "TOPOLOGY_LINK_PROBE_RELEASE_BINDING_REQUIRED",
            detail,
        }),
        Err(LinkProbeBindingError::Capability(detail)) => Err(TopologyApiError {
            status: 422,
            code: "TOPOLOGY_LINK_PROBE_CAPABILITY_REQUIRED",
            detail,
        }),
        Err(LinkProbeBindingError::Storage(detail)) => Err(TopologyApiError {
            status: 500,
            code: "TOPOLOGY_STORAGE_ERROR",
            detail,
        }),
    }?;
    match store.validate_topology_api_bindings(spec) {
        Ok(()) => Ok(()),
        Err(TopologyApiBindingError::Binding(detail)) => Err(TopologyApiError {
            status: 422,
            code: "TOPOLOGY_API_BINDING_INVALID",
            detail,
        }),
        Err(TopologyApiBindingError::Contract(detail)) => Err(TopologyApiError {
            status: 422,
            code: "TOPOLOGY_SERVICE_CONTRACT_INVALID",
            detail,
        }),
        Err(TopologyApiBindingError::Storage(detail)) => Err(TopologyApiError {
            status: 500,
            code: "TOPOLOGY_STORAGE_ERROR",
            detail,
        }),
    }
}

fn parse_spec(body: &str) -> Result<TopologySpec, TopologyApiError> {
    serde_json::from_str(body).map_err(json_error)
}

fn required_if_match(request: &ApiRequest) -> Result<String, TopologyApiError> {
    let value = request
        .headers
        .get("if-match")
        .map(String::as_str)
        .unwrap_or_default()
        .trim();
    let Some(value) = value
        .strip_prefix('"')
        .and_then(|value| value.strip_suffix('"'))
    else {
        return Err(TopologyApiError {
            status: 428,
            code: "IF_MATCH_REQUIRED",
            detail: "a strong quoted If-Match revision ETag is required".to_string(),
        });
    };
    if value.trim().is_empty() {
        return Err(invalid("If-Match revision must not be empty"));
    }
    Ok(value.to_string())
}

fn strong_etag_value(value: &str) -> Result<String, TopologyApiError> {
    let value = value.trim();
    let Some(value) = value
        .strip_prefix('"')
        .and_then(|value| value.strip_suffix('"'))
    else {
        return Err(TopologyApiError {
            status: 428,
            code: "IF_MATCH_REQUIRED",
            detail: "sibling topology_etag must be a strong quoted revision ETag".to_string(),
        });
    };
    if value.trim().is_empty() {
        return Err(invalid("sibling topology_etag revision must not be empty"));
    }
    Ok(value.to_string())
}

fn store_topology_error(error: crate::store_v1_api::StoreApiError) -> TopologyApiError {
    TopologyApiError {
        status: error.status,
        code: error.code,
        detail: error.detail,
    }
}

fn json_error_value(error: serde_json::Error) -> TopologyApiError {
    TopologyApiError {
        status: 500,
        code: "TOPOLOGY_PLAN_SERIALIZATION_FAILED",
        detail: error.to_string(),
    }
}

fn actor(request: &ApiRequest) -> String {
    request
        .headers
        .get("x-actor-id")
        .map(String::as_str)
        .filter(|value| !value.trim().is_empty())
        .unwrap_or("desktop-admin")
        .to_string()
}

fn change_message(request: &ApiRequest, fallback: &str) -> String {
    request
        .headers
        .get("x-change-message")
        .map(String::as_str)
        .filter(|value| !value.trim().is_empty())
        .unwrap_or(fallback)
        .to_string()
}

fn now_marker() -> String {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    format!("unix-ms:{millis}")
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(i64::MAX as u128) as i64
}

fn etag(revision_id: &str) -> String {
    format!("\"{revision_id}\"")
}

fn success(status: u16, data: Value, request_id: &str) -> ApiResponse {
    let body = json!({
        "data": data,
        "meta": {"request_id": request_id, "api_version": "v1"},
    });
    let response = match status {
        201 => ApiResponse::created(body),
        202 => ApiResponse::accepted(body),
        _ => ApiResponse::ok(body),
    };
    response.with_header("X-Request-ID", request_id)
}

fn problem(
    status: u16,
    code: &'static str,
    detail: impl Into<String>,
    request_id: &str,
) -> ApiResponse {
    ApiResponse::problem(status, code, detail, request_id, None)
        .with_header("X-Request-ID", request_id)
}

#[derive(Debug)]
struct TopologyApiError {
    status: u16,
    code: &'static str,
    detail: String,
}

fn storage_error(error: DurableError) -> TopologyApiError {
    match error {
        DurableError::Conflict(detail) => TopologyApiError {
            status: 409,
            code: "TOPOLOGY_REVISION_CONFLICT",
            detail,
        },
        DurableError::Domain(detail) | DurableError::Invariant(detail) => TopologyApiError {
            status: 422,
            code: "TOPOLOGY_INVALID",
            detail,
        },
        other => TopologyApiError {
            status: 500,
            code: "TOPOLOGY_STORAGE_ERROR",
            detail: other.to_string(),
        },
    }
}

fn operation_error(error: orchestrator_control_plane::OperationError) -> TopologyApiError {
    let status = match error {
        orchestrator_control_plane::OperationError::NotFound(_) => 404,
        orchestrator_control_plane::OperationError::InvalidPlan(_) => 422,
        orchestrator_control_plane::OperationError::IdempotencyConflict
        | orchestrator_control_plane::OperationError::InvalidTransition { .. } => 409,
        orchestrator_control_plane::OperationError::Store(_)
        | orchestrator_control_plane::OperationError::Job(_) => 500,
    };
    TopologyApiError {
        status,
        code: if status == 409 {
            "TOPOLOGY_OPERATION_CONFLICT"
        } else if status == 422 {
            "TOPOLOGY_OPERATION_INVALID"
        } else {
            "TOPOLOGY_OPERATION_ERROR"
        },
        detail: error.to_string(),
    }
}

fn domain_error(error: orchestrator_legacy::OrchestratorError) -> TopologyApiError {
    TopologyApiError {
        status: 422,
        code: "TOPOLOGY_INVALID",
        detail: error.to_string(),
    }
}

fn json_error(error: serde_json::Error) -> TopologyApiError {
    TopologyApiError {
        status: 400,
        code: "INVALID_JSON",
        detail: error.to_string(),
    }
}

fn invalid(detail: impl Into<String>) -> TopologyApiError {
    TopologyApiError {
        status: 422,
        code: "TOPOLOGY_INVALID",
        detail: detail.into(),
    }
}

fn not_found(kind: &str, id: &str) -> TopologyApiError {
    TopologyApiError {
        status: 404,
        code: "TOPOLOGY_NOT_FOUND",
        detail: format!("{kind} {id} was not found"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use orchestrator_legacy::{
        OrchestratorStore, ServiceRelease, ServiceReleaseManifest, TopologyEndpointSpec,
        TopologyLinkSpec, service_manifest_from_release,
    };
    use orchestrator_runtime::{RuntimeDesiredState, RuntimeInstance, RuntimeObservedState};
    use orchestrator_storage::{
        RuntimeManagementMode, SqliteOrchestratorStore, StoredRuntimeInstance,
    };

    fn topology_spec(topology_id: &str) -> TopologySpec {
        TopologySpec::new(
            topology_id,
            "127.0.0.1:8080:gateway",
            "private",
            vec![
                TopologyEndpointSpec {
                    endpoint: "127.0.0.1:8080:gateway".to_string(),
                    service_id: "gateway".to_string(),
                    protocol: "https".to_string(),
                    health_path: "/healthz".to_string(),
                    display_name: "Gateway".to_string(),
                    note: String::new(),
                    config: json!({}),
                },
                TopologyEndpointSpec {
                    endpoint: "127.0.0.1:8081:worker".to_string(),
                    service_id: "worker".to_string(),
                    protocol: "https".to_string(),
                    health_path: "/healthz".to_string(),
                    display_name: "Worker".to_string(),
                    note: String::new(),
                    config: json!({}),
                },
            ],
            vec![TopologyLinkSpec {
                source_endpoint: "127.0.0.1:8080:gateway".to_string(),
                target_endpoint: "127.0.0.1:8081:worker".to_string(),
                protocol: "https".to_string(),
                auth_mode: "internal".to_string(),
                scope: "worker.invoke".to_string(),
                enabled: true,
                config_ref: String::new(),
                secret_ref: String::new(),
                policy: json!({}),
                api_bindings: Vec::new(),
            }],
        )
        .unwrap()
    }

    #[test]
    fn etags_are_strong_and_if_match_requires_quotes() {
        assert_eq!(etag("rev-1"), "\"rev-1\"");
        let request = ApiRequest {
            method: "POST".to_string(),
            path: "/api/v1/topologies/primary/revisions".to_string(),
            headers: [("if-match".to_string(), "\"rev-1\"".to_string())]
                .into_iter()
                .collect(),
            body: String::new(),
        };
        assert_eq!(required_if_match(&request).unwrap(), "rev-1");
    }

    #[test]
    fn topology_collection_uses_a_stable_cursor() {
        let directory = tempfile::tempdir().unwrap();
        let durable = DurableStore::Sqlite(
            SqliteOrchestratorStore::open(directory.path().join("orchestrator.db")).unwrap(),
        );
        for topology_id in ["topology-b", "topology-a"] {
            durable
                .create_initial_topology_revision(
                    topology_spec(topology_id),
                    "unix-ms:1".to_string(),
                    "admin".to_string(),
                    "initial".to_string(),
                )
                .unwrap();
        }

        let first = route_with_store(
            &durable,
            None,
            &ApiRequest {
                method: "GET".to_string(),
                path: "/api/v1/topologies?limit=1".to_string(),
                headers: Default::default(),
                body: String::new(),
            },
            &["api", "v1", "topologies"],
            "req-page-1",
        )
        .unwrap();
        assert_eq!(first.body["data"]["items"][0]["topology_id"], "topology-a");
        assert_eq!(first.body["data"]["next_cursor"], "topology-a");

        let second = route_with_store(
            &durable,
            None,
            &ApiRequest {
                method: "GET".to_string(),
                path: "/api/v1/topologies?limit=1&cursor=topology-a".to_string(),
                headers: Default::default(),
                body: String::new(),
            },
            &["api", "v1", "topologies"],
            "req-page-2",
        )
        .unwrap();
        assert_eq!(second.body["data"]["items"][0]["topology_id"], "topology-b");
        assert!(second.body["data"]["next_cursor"].is_null());
    }

    fn release_manifest(
        service_id: &str,
        version: &str,
        link_probe: bool,
    ) -> ServiceReleaseManifest {
        let apis = if link_probe {
            json!([{
                "api_id": "orchestrator.link-probe.v1",
                "protocol": "http",
                "port_name": "default",
                "path_prefix": "/probe",
                "methods": ["GET"],
                "visibility": "global",
                "auth_mode": "public",
                "permission": "public",
                "stability": "stable",
                "version": "v1"
            }])
        } else {
            json!([])
        };
        serde_json::from_value(json!({
            "schema_version": 1,
            "service_name": service_id,
            "version": version,
            "description": "Topology release binding fixture",
            "service_type": "backend-api",
            "source": {
                "kind": "url",
                "url": format!("https://catalog.example/{service_id}/{version}.json"),
                "checksum": format!("sha256:{}", "a".repeat(64))
            },
            "runtime": {
                "kind": "image",
                "image": format!("registry.example/{service_id}@sha256:{}", "b".repeat(64))
            },
            "backend": {"protocol": "http", "port": 8080, "health_path": "/health"},
            "apis": apis
        }))
        .unwrap()
    }

    fn register_release(sqlite: &mut SqliteOrchestratorStore, manifest: &ServiceReleaseManifest) {
        let source_url = manifest.source.url.clone();
        let service = service_manifest_from_release(manifest, &source_url).unwrap();
        sqlite
            .register_service_release_atomic(
                service,
                ServiceRelease {
                    service_name: manifest.service_name.clone(),
                    version: manifest.version.clone(),
                    release_url: source_url,
                    manifest: serde_json::to_value(manifest).unwrap(),
                    checksum: manifest.source.checksum.clone(),
                    created_at: "unix-ms:1".to_string(),
                },
            )
            .unwrap();
    }

    #[test]
    fn link_probe_capability_is_bound_to_the_exact_runtime_release_version() {
        let directory = tempfile::tempdir().unwrap();
        let mut sqlite =
            SqliteOrchestratorStore::open(directory.path().join("orchestrator.db")).unwrap();
        register_release(&mut sqlite, &release_manifest("gateway", "1.0.0", true));
        register_release(&mut sqlite, &release_manifest("gateway", "2.0.0", false));
        register_release(&mut sqlite, &release_manifest("worker", "1.0.0", false));
        let durable = DurableStore::Sqlite(sqlite);
        let mut source = StoredRuntimeInstance {
            node_id: "node-1".to_string(),
            instance: RuntimeInstance {
                deployment_id: "deployment-gateway-v2".to_string(),
                service_id: "gateway".to_string(),
                release_version: "2.0.0".to_string(),
                container_id: "container-gateway-v2".to_string(),
                artifact_digest: format!("sha256:{}", "b".repeat(64)),
                runtime_contract: orchestrator_runtime::RuntimeContract::standard_v1(),
                runtime_policy_sha256: String::new(),
                effective_runtime_sha256: String::new(),
                runtime_attested: false,
                desired_state: RuntimeDesiredState::Running,
                observed_state: RuntimeObservedState::Running,
                health: "HEALTHY".to_string(),
            },
            management_mode: RuntimeManagementMode::Managed,
            endpoint: "127.0.0.1:8080:gateway".to_string(),
            external_probe_protocol: String::new(),
            external_probe_health_path: String::new(),
            last_observed_at_ms: 0,
            drift_reason: String::new(),
            credential_expires_at_ms: 0,
            credential_last_success_at_ms: 0,
            credential_last_error: String::new(),
            updated_at: "unix-ms:1".to_string(),
        };
        durable.put_runtime_instance(&source).unwrap();

        let error =
            validate_registered_services(&durable, &topology_spec("mixed-version")).unwrap_err();
        assert_eq!(error.code, "TOPOLOGY_LINK_PROBE_CAPABILITY_REQUIRED");
        assert!(error.detail.contains("gateway@2.0.0"));

        source.instance.release_version = "1.0.0".to_string();
        durable.put_runtime_instance(&source).unwrap();
        validate_registered_services(&durable, &topology_spec("mixed-version")).unwrap();

        let mut duplicate = source.clone();
        duplicate.instance.deployment_id = "deployment-gateway-duplicate".to_string();
        duplicate.instance.container_id = "container-gateway-duplicate".to_string();
        durable.put_runtime_instance(&duplicate).unwrap();
        let duplicated =
            validate_registered_services(&durable, &topology_spec("mixed-version")).unwrap_err();
        assert_eq!(
            duplicated.code,
            "TOPOLOGY_LINK_PROBE_RELEASE_BINDING_REQUIRED"
        );
        assert!(duplicated.detail.contains("found 2"));
        durable
            .delete_runtime_instance("deployment-gateway-duplicate")
            .unwrap();

        source.instance.release_version = "3.0.0".to_string();
        durable.put_runtime_instance(&source).unwrap();
        let missing =
            validate_registered_services(&durable, &topology_spec("mixed-version")).unwrap_err();
        assert_eq!(missing.code, "TOPOLOGY_LINK_PROBE_RELEASE_BINDING_REQUIRED");
    }

    #[test]
    fn historical_diff_remains_available_after_registered_resources_disappear() {
        let directory = tempfile::tempdir().unwrap();
        let durable = DurableStore::Sqlite(
            SqliteOrchestratorStore::open(directory.path().join("orchestrator.db")).unwrap(),
        );
        let first = durable
            .create_initial_topology_revision(
                topology_spec("historical"),
                "unix-ms:1".to_string(),
                "admin".to_string(),
                "first".to_string(),
            )
            .unwrap();
        let mut changed = topology_spec("historical");
        changed.endpoints[0].note = "changed after services disappeared".to_string();
        let second = durable
            .create_next_topology_revision(
                "historical",
                first.revision_id(),
                changed,
                "unix-ms:2".to_string(),
                "admin".to_string(),
                "second".to_string(),
            )
            .unwrap();
        let response = route_with_store(
            &durable,
            None,
            &ApiRequest {
                method: "POST".to_string(),
                path: "/api/v1/topologies/historical:diff".to_string(),
                headers: Default::default(),
                body: json!({
                    "from_revision_id": first.revision_id(),
                    "to_revision_id": second.revision_id()
                })
                .to_string(),
            },
            &["api", "v1", "topologies", "historical:diff"],
            "req-historical-diff",
        )
        .unwrap();
        assert_eq!(response.status, 200, "{}", response.body);
        assert!(
            !response.body["data"]["diff"]["changes"]
                .as_array()
                .unwrap()
                .is_empty()
        );
    }
}
