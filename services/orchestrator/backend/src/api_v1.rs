//! Versioned public HTTP contract.
//!
//! Enforces the v1 envelope, authorization and mutation rules, then routes to
//! typed application handlers. The historical action router is not a fallback.

use crate::artifact_store::ArtifactStore;
use crate::audit::{MutationAudit, operation_id as audited_operation_id};
use crate::auth::Principal;
use crate::auth_permission_check::AuthPermissionChecker;
use crate::build_identity::BuildIdentity;
use crate::catalog_registry::CatalogRegistry;
use crate::contribution_ack;
use crate::contribution_snapshot::active_contribution_snapshot;
use crate::deployment_api;
use crate::diagnostics_api::handle_registry_request;
use crate::durable::{DurableError, DurableStore};
use crate::http::{ApiRequest, ApiResponse};
use crate::node_api;
use crate::operation_api;
use crate::registry::RegistryContext;
use crate::resource_api;
use crate::topology_api;
use crate::topology_provider::TopologyProviderSaga;
use crate::{market_api, store_v1_api, ui_layout};
use orchestrator_core::{V1_ACTIONS, v1_action};
use orchestrator_storage::{IdempotencyBegin, StoredIdempotentResponse};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

static REQUEST_SEQUENCE: AtomicU64 = AtomicU64::new(1);

pub(crate) fn is_v1_path(path: &str) -> bool {
    path == "/api/v1" || path.starts_with("/api/v1/")
}

pub(crate) fn is_lock_free_path(method: &str, path: &str) -> bool {
    method == "GET" && matches!(path, "/api/v1/healthz/live" | "/api/v1/healthz/ready")
}

pub(crate) fn lock_free_response(
    path: &str,
    durable_store: Option<&DurableStore>,
    startup_warnings: &[String],
    build: &BuildIdentity,
) -> ApiResponse {
    let request_id = next_request_id();
    let (status, data) = match path {
        "/api/v1/healthz/live" => (200, json!({"status": "ok"})),
        "/api/v1/healthz/ready" => match durable_store {
            Some(store) => match store.readiness() {
                Ok(readiness) => (
                    200,
                    json!({
                        "status": "ready",
                        "store": store.kind(),
                        "storage": readiness,
                        "recovery_complete": true,
                        "warnings": startup_warnings,
                        "build": build,
                    }),
                ),
                Err(error) => (
                    503,
                    json!({
                        "status": "not_ready",
                        "store": store.kind(),
                        "recovery_complete": true,
                        "warnings": startup_warnings,
                        "reason": error.to_string(),
                    }),
                ),
            },
            None => (
                503,
                json!({
                    "status": "not_ready",
                    "store": "memory",
                    "recovery_complete": true,
                    "warnings": startup_warnings,
                    "reason": "persistent storage is required for production readiness",
                }),
            ),
        },
        _ => (404, json!({"status": "not_found"})),
    };
    if status >= 400 {
        return ApiResponse::problem(
            status,
            "NOT_READY",
            data.get("reason")
                .and_then(Value::as_str)
                .unwrap_or("resource not found"),
            &request_id,
            None,
        )
        .with_header("X-Request-ID", request_id);
    }
    envelope(status, data, request_id)
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn handle_with_permission_checker(
    registry_context: &mut RegistryContext,
    durable_store: Option<&DurableStore>,
    topology_provider: Option<&TopologyProviderSaga>,
    catalog_registry: Option<&CatalogRegistry>,
    artifact_store: Option<&ArtifactStore>,
    store_state: &market_api::StoreState,
    repo_root: &Path,
    auth_permission_checker: Option<&AuthPermissionChecker>,
    mut request: ApiRequest,
    expected_internal_token: Option<&str>,
    principal: Option<&Principal>,
) -> ApiResponse {
    let request_id = next_request_id();
    let Some(principal) = principal else {
        return ApiResponse::problem(
            401,
            "UNAUTHORIZED",
            "a verified Desktop session, internal control-plane token, or OIDC principal is required",
            &request_id,
            None,
        )
        .with_header("X-Request-ID", request_id);
    };
    let path_without_query = request.path.split('?').next().unwrap_or("/");
    match authorization_target(&request.method, path_without_query) {
        V1AuthorizationTarget::Meta => {}
        V1AuthorizationTarget::Action(action_id) => {
            let descriptor = v1_action(action_id)
                .expect("v1 route authorization must reference the frozen action matrix");
            if !principal.allows(descriptor.role) {
                return ApiResponse::problem(
                    403,
                    "FORBIDDEN",
                    format!(
                        "principal {} cannot execute {action_id}; {} is required",
                        principal.id(),
                        descriptor.role.permission()
                    ),
                    &request_id,
                    None,
                )
                .with_header("X-Request-ID", request_id);
            }
        }
        V1AuthorizationTarget::Unknown => {
            return ApiResponse::problem(
                404,
                "ROUTE_NOT_FOUND",
                "the requested v1 route does not exist",
                &request_id,
                None,
            )
            .with_header("X-Request-ID", request_id);
        }
    }

    // Downstream compatibility handlers receive only the verified identity.
    // Any caller-supplied identity/role headers are removed before dispatch.
    for header in ["x-actor-id", "x-user-id", "x-role", "x-user-role"] {
        request.headers.remove(header);
    }
    request
        .headers
        .insert("x-actor-id".to_string(), principal.id().to_string());
    request
        .headers
        .insert("x-user-id".to_string(), principal.id().to_string());
    if is_mutation(&request.method)
        && request
            .headers
            .get("idempotency-key")
            .is_none_or(|value| value.trim().is_empty())
    {
        return ApiResponse::problem(
            400,
            "IDEMPOTENCY_KEY_REQUIRED",
            "mutating v1 requests require an Idempotency-Key header",
            &request_id,
            None,
        )
        .with_header("X-Request-ID", request_id);
    }

    let reservation = if is_mutation(&request.method) {
        let Some(store) = durable_store else {
            return ApiResponse::problem(
                503,
                "IDEMPOTENCY_STORAGE_UNAVAILABLE",
                "mutating v1 requests require durable idempotency storage",
                &request_id,
                None,
            )
            .with_header("X-Request-ID", request_id);
        };
        let key = request
            .headers
            .get("idempotency-key")
            .expect("validated above")
            .trim()
            .to_string();
        let scope = format!("{} {path_without_query}", request.method);
        let digest = request_digest(&request);
        match store.begin_idempotent_request(&scope, &key, &digest, now_ms()) {
            Ok(IdempotencyBegin::Started) => Some((scope, key, digest)),
            Ok(IdempotencyBegin::Replay(stored)) => {
                return ApiResponse {
                    status: stored.status,
                    body: stored.body,
                    content_type: stored.content_type,
                    headers: stored.headers,
                }
                .with_header("Idempotency-Replayed", "true");
            }
            Ok(IdempotencyBegin::InProgress) => {
                return ApiResponse::problem(
                    409,
                    "IDEMPOTENCY_IN_PROGRESS",
                    "an equivalent request is still in progress",
                    &request_id,
                    None,
                )
                .with_header("Retry-After", "1")
                .with_header("X-Request-ID", request_id);
            }
            Ok(IdempotencyBegin::NeedsAttention) => {
                return ApiResponse::problem(
                    409,
                    "IDEMPOTENCY_NEEDS_ATTENTION",
                    "the prior request may have performed an external side effect; inspect its Operation before retrying",
                    &request_id,
                    None,
                )
                .with_header("X-Request-ID", request_id);
            }
            Err(DurableError::Conflict(detail)) => {
                return ApiResponse::problem(
                    409,
                    "IDEMPOTENCY_CONFLICT",
                    detail,
                    &request_id,
                    None,
                )
                .with_header("X-Request-ID", request_id);
            }
            Err(DurableError::Invariant(detail) | DurableError::Domain(detail)) => {
                return ApiResponse::problem(
                    400,
                    "INVALID_IDEMPOTENCY_KEY",
                    detail,
                    &request_id,
                    None,
                )
                .with_header("X-Request-ID", request_id);
            }
            Err(error) => {
                return ApiResponse::problem(
                    500,
                    "IDEMPOTENCY_STORAGE_ERROR",
                    error.to_string(),
                    &request_id,
                    None,
                )
                .with_header("X-Request-ID", request_id);
            }
        }
    } else {
        None
    };

    let audit = if let Some((scope, key, digest)) = reservation.as_ref() {
        let store = durable_store.expect("reservation requires durable storage");
        match MutationAudit::begin(
            store,
            &request,
            &request_id,
            key,
            digest,
            principal,
            now_ms(),
        ) {
            Ok(audit) => Some(audit),
            Err(error) => {
                let (status, code) = match &error {
                    DurableError::Invariant(_) | DurableError::Domain(_) => {
                        (400, "INVALID_AUDIT_CONTEXT")
                    }
                    DurableError::Conflict(_) | DurableError::Storage(_) => {
                        (503, "AUDIT_STORAGE_UNAVAILABLE")
                    }
                };
                let release = store.abort_idempotent_request(scope, key, digest);
                let release_detail = match release {
                    Ok(()) => "the pre-dispatch idempotency reservation was released and may be retried immediately".to_string(),
                    Err(release_error) => format!(
                        "the pre-dispatch idempotency reservation could not be released and remains recoverable: {release_error}"
                    ),
                };
                let response = ApiResponse::problem(
                    status,
                    code,
                    format!(
                        "the mutation was not dispatched because its audit intent could not be durably recorded: {error}; {release_detail}"
                    ),
                    &request_id,
                    None,
                )
                .with_header("X-Request-ID", request_id);
                return if status == 503 {
                    response.with_header("Retry-After", "1")
                } else {
                    response
                };
            }
        }
    } else {
        None
    };

    let response = dispatch_authenticated(
        registry_context,
        durable_store,
        topology_provider,
        catalog_registry,
        artifact_store,
        store_state,
        repo_root,
        auth_permission_checker,
        request,
        expected_internal_token,
        principal,
        request_id.clone(),
    );
    if let Some(audit) = &audit
        && let Err(error) = audit.finish(
            durable_store.expect("audit requires durable storage"),
            &response,
            now_ms(),
        )
    {
        return ApiResponse::problem(
            500,
            "AUDIT_RESULT_COMMIT_FAILED",
            format!(
                "the mutation finished but its terminal audit result could not be durably recorded; the idempotency reservation remains recoverable and the request will not be executed again automatically: {error}"
            ),
            &request_id,
            audited_operation_id(&response).as_deref(),
        )
        .with_header("X-Request-ID", request_id);
    }
    if let Some((scope, key, digest)) = reservation {
        let stored = StoredIdempotentResponse {
            status: response.status,
            content_type: response.content_type.clone(),
            headers: response.headers.clone(),
            body: response.body.clone(),
        };
        if let Err(error) = durable_store
            .expect("reservation requires durable storage")
            .complete_idempotent_request(&scope, &key, &digest, &stored, now_ms())
        {
            return ApiResponse::problem(
                500,
                "IDEMPOTENCY_COMMIT_FAILED",
                format!(
                    "the request result could not be durably recorded and will not be executed again automatically: {error}"
                ),
                &request_id,
                response
                    .body
                    .pointer("/data/operation_id")
                    .and_then(Value::as_str),
            )
            .with_header("X-Request-ID", request_id);
        }
    }
    response
}

#[allow(clippy::too_many_arguments)]
fn dispatch_authenticated(
    registry_context: &mut RegistryContext,
    durable_store: Option<&DurableStore>,
    topology_provider: Option<&TopologyProviderSaga>,
    catalog_registry: Option<&CatalogRegistry>,
    artifact_store: Option<&ArtifactStore>,
    store_state: &market_api::StoreState,
    repo_root: &Path,
    auth_permission_checker: Option<&AuthPermissionChecker>,
    request: ApiRequest,
    expected_internal_token: Option<&str>,
    principal: &Principal,
    request_id: String,
) -> ApiResponse {
    let path = request.path.split('?').next().unwrap_or("/");
    if path == "/api/v1/ui/layout" {
        return ui_layout_response(durable_store, repo_root, &request, principal, &request_id);
    }
    if request.method == "POST" && path == "/api/v1/auth/permissions:check" {
        return permission_check_response(
            auth_permission_checker,
            &request,
            principal,
            &request_id,
        );
    }
    if request.method == "GET" && path == "/api/v1/contributions/snapshot" {
        let Some(storage) = durable_store else {
            return ApiResponse::problem(
                503,
                "CONTRIBUTION_STORAGE_UNAVAILABLE",
                "a durable store is required to publish the active Contribution snapshot",
                &request_id,
                None,
            )
            .with_header("X-Request-ID", request_id);
        };
        return match active_contribution_snapshot(storage, "default") {
            Ok(snapshot) => {
                let etag = snapshot
                    .get("digest")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                envelope(200, snapshot, request_id).with_header("ETag", etag)
            }
            Err(error) => ApiResponse::problem(
                503,
                "CONTRIBUTION_SNAPSHOT_UNAVAILABLE",
                error.to_string(),
                &request_id,
                None,
            )
            .with_header("Retry-After", "1")
            .with_header("X-Request-ID", request_id),
        };
    }
    if request.method == "POST" && path == "/api/v1/contributions/projections:ack" {
        return contribution_ack::response(durable_store, &request, principal, &request_id);
    }
    if let Some(response) = operation_api::route(durable_store, &request, &request_id) {
        return response;
    }
    if let Some(response) = deployment_api::route(durable_store, &request, &request_id) {
        return response;
    }
    if let Some(response) = resource_api::route(durable_store, &request, &request_id) {
        return response;
    }
    if let Some(response) = node_api::route(registry_context, durable_store, &request, &request_id)
    {
        return response;
    }
    if let Some(response) =
        topology_api::route(durable_store, topology_provider, &request, &request_id)
    {
        return response;
    }
    if let Some(response) = store_v1_api::route(
        store_state,
        registry_context,
        durable_store,
        catalog_registry,
        artifact_store,
        &request,
        &request_id,
    ) {
        return response;
    }
    if request.method == "POST"
        && path == "/api/v1/diagnostics"
        && let Err(detail) = validate_diagnostic_create_request(&request.body)
    {
        return ApiResponse::problem(400, "DIAGNOSTIC_REQUEST_INVALID", detail, &request_id, None)
            .with_header("X-Request-ID", request_id);
    }
    if request.method == "GET" && path == "/api/v1/capabilities" {
        let catalog_registry_present = durable_store.is_some() && catalog_registry.is_some();
        let catalog_has_sources = durable_store
            .zip(catalog_registry)
            .and_then(|(storage, registry)| registry.has_sources(storage).ok())
            .unwrap_or(false);
        let catalog_ready = durable_store
            .zip(catalog_registry)
            .and_then(|(storage, registry)| registry.has_enabled_sources(storage).ok())
            .unwrap_or(false);
        let supported = supported_v1_actions(
            durable_store.is_some(),
            topology_provider.is_some(),
            catalog_registry_present,
            catalog_has_sources,
            catalog_ready,
        );
        let actions = V1_ACTIONS
            .iter()
            .filter(|entry| supported.contains(entry.action_id) && principal.allows(entry.role))
            .map(|entry| {
                json!({
                    "action": entry.action_id,
                    "target_type": entry.target_type,
                    "capability_status": "AVAILABLE",
                    "required_permission": entry.role.permission(),
                    "asynchronous": entry.asynchronous,
                })
            })
            .collect::<Vec<_>>();
        return envelope(200, json!({"actions": actions}), request_id);
    }

    let Some(legacy_path) = compatibility_path(&request.path) else {
        return ApiResponse::problem(
            404,
            "ROUTE_NOT_FOUND",
            "the requested v1 route does not exist",
            &request_id,
            None,
        )
        .with_header("X-Request-ID", request_id);
    };
    let legacy_request = ApiRequest {
        path: legacy_path,
        ..request
    };
    let response =
        handle_registry_request(registry_context, legacy_request, expected_internal_token);
    if response.status >= 400 {
        let detail = response
            .body
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("request failed")
            .to_string();
        return ApiResponse::problem(
            response.status,
            "ORCHESTRATOR_REQUEST_FAILED",
            detail,
            &request_id,
            response.body.get("operation_id").and_then(Value::as_str),
        )
        .with_header("X-Request-ID", request_id);
    }
    if let Some(action_result) = response.body.get("action_result") {
        let action_status = action_result
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_ascii_uppercase();
        let failure = match action_status.as_str() {
            "FAILED" => Some((422, "ACTION_EXECUTION_FAILED")),
            "BLOCKED" => Some((409, "ACTION_EXECUTION_BLOCKED")),
            "UNSUPPORTED" => Some((501, "PUBLISHED_ACTION_UNSUPPORTED")),
            _ => None,
        };
        if let Some((status, code)) = failure {
            let detail = action_result
                .get("message")
                .or_else(|| action_result.get("error"))
                .and_then(Value::as_str)
                .unwrap_or("the action did not complete successfully");
            return ApiResponse::problem(
                status,
                code,
                detail,
                &request_id,
                action_result.get("operation_id").and_then(Value::as_str),
            )
            .with_header("X-Request-ID", request_id);
        }
    }
    let status = if is_async_action_path(path) && response.status < 300 {
        202
    } else {
        response.status
    };
    envelope(status, response.body, request_id)
}

fn request_digest(request: &ApiRequest) -> String {
    let mut hasher = Sha256::new();
    for value in [
        request.method.as_str(),
        request.path.as_str(),
        request
            .headers
            .get("content-type")
            .map(String::as_str)
            .unwrap_or_default(),
        request
            .headers
            .get("if-match")
            .map(String::as_str)
            .unwrap_or_default(),
        request.body.as_str(),
    ] {
        hasher.update((value.len() as u64).to_be_bytes());
        hasher.update(value.as_bytes());
    }
    format!("sha256:{:x}", hasher.finalize())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum V1AuthorizationTarget {
    Meta,
    Action(&'static str),
    Unknown,
}

fn authorization_target(method: &str, path: &str) -> V1AuthorizationTarget {
    let segments = path
        .trim_matches('/')
        .split('/')
        .filter(|segment| !segment.is_empty())
        .collect::<Vec<_>>();
    if segments.as_slice() == ["api", "v1"]
        || (method == "GET"
            && matches!(
                segments.as_slice(),
                ["api", "v1", "capabilities"]
                    | ["api", "v1", "contributions", "snapshot"]
                    | ["api", "v1", "healthz", "live"]
                    | ["api", "v1", "healthz", "ready"]
            ))
    {
        return V1AuthorizationTarget::Meta;
    }

    let action = match (method, segments.as_slice()) {
        ("GET" | "PUT", ["api", "v1", "ui", "layout"]) => {
            return V1AuthorizationTarget::Meta;
        }
        ("POST", ["api", "v1", "auth", "permissions:check"]) => {
            return V1AuthorizationTarget::Meta;
        }
        ("POST", ["api", "v1", "contributions", "projections:ack"]) => {
            return V1AuthorizationTarget::Meta;
        }
        ("GET", ["api", "v1", "store", "catalogs"]) => "catalog.list",
        ("GET", ["api", "v1", "store", "packages"]) => "catalog.search",
        ("POST", ["api", "v1", "store", "catalogs"]) => "catalog.register",
        ("DELETE", ["api", "v1", "store", "catalogs", _]) => "catalog.remove",
        ("POST", ["api", "v1", "store", "releases:validate"]) => "release.validate",
        ("POST", ["api", "v1", "store", "releases:import"]) => "release.import",
        ("POST", ["api", "v1", "store", "releases:install"]) => "release.install",
        ("POST", ["api", "v1", "store", "releases:upgrade"]) => "release.upgrade",
        ("POST", ["api", "v1", "store", "releases:rollback"]) => "release.rollback",
        ("POST", ["api", "v1", "store", "releases:delete"]) => "release.delete",

        ("GET", ["api", "v1", "nodes"]) => "node.list",
        ("GET", ["api", "v1", "nodes", _]) | ("GET", ["api", "v1", "nodes", _, "health"]) => {
            "node.health"
        }
        ("POST", ["api", "v1", "nodes", "enrollment-codes"]) => "node.register",
        ("POST", ["api", "v1", "nodes", action]) if action.ends_with(":revoke-certificates") => {
            "node.revoke"
        }
        ("POST", ["api", "v1", "nodes", action]) if action.ends_with(":drain") => "node.drain",
        ("DELETE", ["api", "v1", "nodes", _]) => "node.remove",

        ("GET", ["api", "v1", "deployments"]) => "deployment.list",
        ("GET", ["api", "v1", "deployments", _, "health"]) => "deployment.health",
        ("GET", ["api", "v1", "deployments", _, "bindings"]) => "deployment.get",
        ("GET", ["api", "v1", "deployments", _]) => "deployment.get",
        ("POST", ["api", "v1", "deployments", action]) if action.ends_with(":start") => {
            "deployment.start"
        }
        ("POST", ["api", "v1", "deployments", action]) if action.ends_with(":stop") => {
            "deployment.stop"
        }
        ("POST", ["api", "v1", "deployments", action]) if action.ends_with(":restart") => {
            "deployment.restart"
        }
        ("POST", ["api", "v1", "deployments", action]) if action.ends_with(":uninstall") => {
            "deployment.uninstall"
        }

        ("POST", ["api", "v1", "resources", action]) if action.ends_with(":purge") => {
            "resource.purge"
        }

        ("POST", ["api", "v1", "operations:plan"]) => "operation.plan",
        ("GET", ["api", "v1", "operations", _, "events"]) => "operation.events",
        ("GET", ["api", "v1", "operations"])
        | ("GET", ["api", "v1", "operations", _])
        | ("GET", ["api", "v1", "operations", _, "logs"]) => "operation.logs",
        ("POST", ["api", "v1", "operations", action]) if action.ends_with(":confirm") => {
            "operation.confirm"
        }
        ("POST", ["api", "v1", "operations", action]) if action.ends_with(":apply") => {
            "operation.apply"
        }
        ("POST", ["api", "v1", "operations", action]) if action.ends_with(":cancel") => {
            "operation.cancel"
        }
        ("POST", ["api", "v1", "operations", action]) if action.ends_with(":retry") => {
            "operation.retry"
        }
        ("POST", ["api", "v1", "operations", action]) if action.ends_with(":rollback") => {
            "operation.rollback"
        }

        ("GET", ["api", "v1", "topologies", _, "status"]) => "topology.status",
        ("GET", ["api", "v1", "topologies"])
        | ("GET", ["api", "v1", "topologies", _])
        | ("GET", ["api", "v1", "topologies", _, "revisions"])
        | ("GET", ["api", "v1", "topologies", _, "revisions", _]) => "topology.export",
        ("POST", ["api", "v1", "topologies"]) => "topology.draft",
        ("POST", ["api", "v1", "topologies", _, "revisions"]) => "topology.revision",
        ("PUT" | "DELETE", ["api", "v1", "topologies", _, "draft", "endpoints", _]) => {
            "topology.endpoint.edit"
        }
        ("PUT" | "DELETE", ["api", "v1", "topologies", _, "draft", "links", _, _]) => {
            "topology.link.edit"
        }
        ("POST", ["api", "v1", "topologies", action]) if action.ends_with(":validate") => {
            "topology.validate"
        }
        ("POST", ["api", "v1", "topologies", action]) if action.ends_with(":diff") => {
            "topology.diff"
        }
        ("POST", ["api", "v1", "topologies", action]) if action.ends_with(":apply") => {
            "topology.apply"
        }
        ("POST", ["api", "v1", "topologies", action]) if action.ends_with(":rollback") => {
            "topology.rollback"
        }

        ("POST", ["api", "v1", "diagnostics"]) => "diagnostic.create",
        ("GET", ["api", "v1", "diagnostics"]) => "diagnostic.list",
        ("GET", ["api", "v1", "diagnostics", report]) if report.ends_with(".json") => {
            "diagnostic.export"
        }
        ("GET", ["api", "v1", "diagnostics", report]) if report.ends_with(".md") => {
            "diagnostic.export"
        }
        ("GET", ["api", "v1", "diagnostics", _]) => "diagnostic.get",
        _ => return V1AuthorizationTarget::Unknown,
    };
    V1AuthorizationTarget::Action(action)
}

fn ui_layout_response(
    durable_store: Option<&DurableStore>,
    repo_root: &Path,
    request: &ApiRequest,
    principal: &Principal,
    request_id: &str,
) -> ApiResponse {
    let topology_id = request
        .path
        .split_once('?')
        .map(|(_, query)| query)
        .and_then(|query| {
            url::form_urlencoded::parse(query.as_bytes())
                .find(|(name, _)| name == "topology_id")
                .map(|(_, value)| value.into_owned())
        })
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| "primary".to_string());
    let result = match (request.method.as_str(), durable_store) {
        ("GET", Some(store)) => ui_layout::get_durable_layout(
            store,
            store.as_sqlite().map(|_| repo_root),
            principal.id(),
            &topology_id,
        ),
        ("PUT", Some(store)) => {
            ui_layout::put_durable_layout(store, principal.id(), &topology_id, &request.body)
        }
        ("GET", None) => ui_layout::get_layout(repo_root),
        ("PUT", None) => Err(anyhow::anyhow!(
            "durable storage is required to save v1 UI state"
        )),
        _ => Err(anyhow::anyhow!("unsupported UI layout method")),
    };
    match result {
        Ok(data) => envelope(200, data, request_id.to_string()),
        Err(error) => ApiResponse::problem(
            if durable_store.is_none() { 503 } else { 400 },
            if durable_store.is_none() {
                "UI_STATE_STORAGE_UNAVAILABLE"
            } else {
                "UI_STATE_INVALID"
            },
            error.to_string(),
            request_id,
            None,
        )
        .with_header("X-Request-ID", request_id),
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct PermissionCheckRequest {
    permissions: Vec<String>,
}

fn permission_check_response(
    checker: Option<&AuthPermissionChecker>,
    request: &ApiRequest,
    principal: &Principal,
    request_id: &str,
) -> ApiResponse {
    let payload = match serde_json::from_str::<PermissionCheckRequest>(&request.body) {
        Ok(payload) => payload,
        Err(error) => {
            return ApiResponse::problem(
                400,
                "PERMISSION_CHECK_REQUEST_INVALID",
                format!("permission check body must contain only a permissions array: {error}"),
                request_id,
                None,
            )
            .with_header("Cache-Control", "no-store")
            .with_header("X-Request-ID", request_id);
        }
    };
    if payload.permissions.is_empty()
        || payload.permissions.len() > 128
        || payload
            .permissions
            .iter()
            .any(|permission| !valid_permission_key(permission))
    {
        return ApiResponse::problem(
            400,
            "PERMISSION_CHECK_REQUEST_INVALID",
            "permissions must contain 1-128 unique lowercase namespaced permission keys",
            request_id,
            None,
        )
        .with_header("Cache-Control", "no-store")
        .with_header("X-Request-ID", request_id);
    }
    let mut unique = std::collections::BTreeSet::new();
    if payload
        .permissions
        .iter()
        .any(|permission| !unique.insert(permission.as_str()))
    {
        return ApiResponse::problem(
            400,
            "PERMISSION_CHECK_REQUEST_INVALID",
            "permissions must contain 1-128 unique lowercase namespaced permission keys",
            request_id,
            None,
        )
        .with_header("Cache-Control", "no-store")
        .with_header("X-Request-ID", request_id);
    }
    let effective = checker.and_then(|checker| checker.effective_permissions(principal));
    let decisions = payload
        .permissions
        .iter()
        .map(|permission| {
            json!({
                "permission": permission,
                "allowed": effective
                    .as_ref()
                    .is_some_and(|permissions| permissions.contains(permission)),
            })
        })
        .collect::<Vec<_>>();
    envelope(200, json!({"decisions": decisions}), request_id.to_string())
        .with_header("Cache-Control", "no-store")
}

fn valid_permission_key(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 256
        && value.contains('.')
        && value
            .bytes()
            .next()
            .is_some_and(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
        && value.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'.' | b'-')
        })
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(i64::MAX as u128) as i64
}

pub(crate) fn envelope(status: u16, data: Value, request_id: String) -> ApiResponse {
    let response_request_id = request_id.clone();
    let body = json!({
        "data": data,
        "meta": {
            "request_id": request_id,
            "api_version": "v1",
        }
    });
    let response = match status {
        201 => ApiResponse::created(body),
        202 => ApiResponse::accepted(body),
        _ => ApiResponse::ok(body),
    };
    response.with_header("X-Request-ID", response_request_id)
}

pub(crate) fn next_request_id() -> String {
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    let sequence = REQUEST_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    format!("req-{timestamp:032x}-{sequence:016x}")
}

fn is_mutation(method: &str) -> bool {
    matches!(method, "POST" | "PUT" | "PATCH" | "DELETE")
}

fn is_async_action_path(path: &str) -> bool {
    path.ends_with(":apply")
        || path.ends_with(":rollback")
        || path.ends_with(":install")
        || path.ends_with(":upgrade")
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct DiagnosticCreateRequest {}

fn validate_diagnostic_create_request(body: &str) -> Result<(), String> {
    serde_json::from_str::<DiagnosticCreateRequest>(body).map_err(|error| {
        format!("diagnostic request must match DiagnosticCreateRequest: {error}")
    })?;
    Ok(())
}

fn compatibility_path(path_and_query: &str) -> Option<String> {
    let (path, query) = path_and_query
        .split_once('?')
        .map(|(path, query)| (path, Some(query)))
        .unwrap_or((path_and_query, None));
    let suffix = path.strip_prefix("/api/v1")?;
    let mapped = match suffix {
        "/operations:plan" => "/operations/plan".to_string(),
        _ if suffix.starts_with("/operations/") && suffix.contains(':') => {
            suffix.replacen(':', "/", 1)
        }
        "" => "/health".to_string(),
        _ => suffix.to_string(),
    };
    Some(match query {
        Some(query) => format!("{mapped}?{query}"),
        None => mapped,
    })
}

fn supported_v1_actions(
    durable_topology: bool,
    topology_provider: bool,
    catalog_registry: bool,
    catalog_has_sources: bool,
    catalog_ready: bool,
) -> std::collections::BTreeSet<&'static str> {
    // Read-only diagnostics are backed by the registry_context projection. Every other
    // public action below needs durable state and is omitted when its provider
    // is unavailable; `/capabilities` must never advertise a request that can
    // only fail with `*_UNAVAILABLE`.
    let mut actions = ["diagnostic.list", "diagnostic.get", "diagnostic.export"]
        .into_iter()
        .collect::<std::collections::BTreeSet<_>>();
    if durable_topology {
        actions.extend([
            "release.delete",
            "node.list",
            "node.health",
            "node.register",
            "node.revoke",
            "node.drain",
            "node.remove",
            "deployment.list",
            "deployment.get",
            "deployment.start",
            "deployment.stop",
            "deployment.restart",
            "deployment.uninstall",
            "deployment.health",
            "resource.purge",
            "operation.plan",
            "operation.confirm",
            "operation.apply",
            "operation.cancel",
            "operation.retry",
            "operation.rollback",
            "operation.logs",
            "operation.events",
            "diagnostic.create",
            "topology.draft",
            "topology.revision",
            "topology.endpoint.edit",
            "topology.link.edit",
            "topology.validate",
            "topology.diff",
            "topology.status",
            "topology.export",
        ]);
    }
    if durable_topology && catalog_registry {
        actions.extend(["catalog.list", "catalog.register"]);
    }
    if durable_topology && catalog_registry && catalog_has_sources {
        actions.insert("catalog.remove");
    }
    if durable_topology && catalog_registry && catalog_ready {
        actions.extend([
            "catalog.search",
            "release.import",
            "release.validate",
            "release.install",
            "release.upgrade",
            "release.rollback",
        ]);
    }
    if durable_topology && topology_provider {
        actions.extend(["topology.apply", "topology.rollback"]);
    }
    actions
}
