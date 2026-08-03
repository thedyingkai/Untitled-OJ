use crate::artifact_store::{ArtifactRetentionPolicy, ArtifactStore, MAX_ARTIFACT_BYTES};
use crate::catalog_registry::{
    CatalogRegistry, CatalogRegistryError, CatalogSourceRegistration, PackageQuery,
    ResolvedCatalogPlan, VerifiedReleaseDocument,
};
use crate::durable::{DurableError, DurableStore};
use crate::http::{ApiRequest, ApiResponse, query_value};
use crate::{market_api, routes::status_for_error};
use orchestrator_control_plane::{
    DurableOperation, DurableOperationStatus, JobKind, JobStore, OperationCoordinator,
    OperationRepository, PlanOperation, PlannedJob, PlannedJobCondition,
};
use orchestrator_legacy::{
    ActionRequest, NodeRecord, OrchestratorActionConsole, ServiceRelease, ServiceReleaseManifest,
    parse_endpoint_id, validate_endpoint_id, validate_service_release,
};
use orchestrator_manager::MigrationPolicyV2;
use orchestrator_manager::catalog_v2::{ReleaseChannel, TargetPlatform};
use orchestrator_runtime::{
    ApiSurfaceSpec, ArtifactReference, AuthPipelineStep, AuthServiceIdentitySpec, ContainerSpec,
    GatewayPipelineStep, GatewayRouteSpec, HealthGatePolicy, OciImageReference, OciMigrationStep,
    PublishedEndpoint, PublishedPortProtocol, RedisNamespaceSpec, ReleasePipelinePayload,
    ReleaseProviderRevision, ReleaseReplacementPayload, ReplacementProviderSaga,
    RuntimeInstallPayload, RuntimeMaterializationStep, RuntimeObservedState, StorageResourceSpec,
    TypedProvisionerStep,
};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs;
use std::net::IpAddr;
use std::path::Path;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

static STORE_PLAN_LOCK: Mutex<()> = Mutex::new(());
const CONTROL_PLANE_NODE_ID: &str = "control-plane";

pub(crate) fn route(
    _state: &market_api::StoreState,
    console: &mut OrchestratorActionConsole,
    storage: Option<&DurableStore>,
    catalog_registry: Option<&CatalogRegistry>,
    artifact_store: Option<&ArtifactStore>,
    request: &ApiRequest,
    request_id: &str,
) -> Option<ApiResponse> {
    let path = request.path.split('?').next().unwrap_or("/");
    let response = match (request.method.as_str(), path) {
        ("GET", "/api/v1/store/catalogs") => {
            catalog_sources(storage, catalog_registry, request, request_id)
        }
        ("POST", "/api/v1/store/catalogs") => {
            register_catalog_source(storage, catalog_registry, request, request_id)
        }
        ("DELETE", _) if path.starts_with("/api/v1/store/catalogs/") => {
            delete_catalog_source(storage, catalog_registry, path, request_id)
        }
        ("GET", "/api/v1/store/packages") => {
            list_catalog_packages(console, storage, catalog_registry, request, request_id)
        }
        ("POST", "/api/v1/store/releases:import") => {
            let Some(storage) = storage else {
                return Some(problem(
                    503,
                    "STORE_STORAGE_UNAVAILABLE",
                    "release import requires durable Catalog source storage",
                    request_id,
                    None,
                ));
            };
            let Some(catalog_registry) = catalog_registry else {
                return Some(problem(
                    503,
                    "CATALOG_REGISTRY_UNAVAILABLE",
                    "release import requires explicitly configured trusted Catalog v2 sources",
                    request_id,
                    None,
                ));
            };
            import_release(console, storage, catalog_registry, request, request_id)
        }
        ("POST", "/api/v1/store/releases:validate") => {
            let Some(storage) = storage else {
                return Some(problem(
                    503,
                    "STORE_STORAGE_UNAVAILABLE",
                    "release validation requires durable Catalog source storage",
                    request_id,
                    None,
                ));
            };
            let Some(catalog_registry) = catalog_registry else {
                return Some(problem(
                    503,
                    "CATALOG_REGISTRY_UNAVAILABLE",
                    "release validation requires explicitly configured trusted Catalog v2 sources",
                    request_id,
                    None,
                ));
            };
            validate_release_catalog(storage, catalog_registry, request, request_id)
        }
        ("POST", "/api/v1/store/releases:install") => {
            let Some(storage) = storage else {
                return Some(problem(
                    503,
                    "STORE_STORAGE_UNAVAILABLE",
                    "Store installation requires durable Operation and Job storage",
                    request_id,
                    None,
                ));
            };
            let Some(catalog_registry) = catalog_registry else {
                return Some(problem(
                    503,
                    "CATALOG_REGISTRY_UNAVAILABLE",
                    "Store installation requires explicitly configured trusted Catalog v2 sources",
                    request_id,
                    None,
                ));
            };
            install_release(
                console,
                storage,
                catalog_registry,
                artifact_store,
                request,
                request_id,
            )
        }
        ("POST", "/api/v1/store/releases:delete") => {
            let Some(storage) = storage else {
                return Some(problem(
                    503,
                    "STORE_STORAGE_UNAVAILABLE",
                    "Store uninstall requires durable Operation and Job storage",
                    request_id,
                    None,
                ));
            };
            delete_release_metadata(console, storage, request, request_id)
        }
        ("POST", "/api/v1/store/releases:upgrade") => {
            let Some(storage) = storage else {
                return Some(problem(
                    503,
                    "STORE_STORAGE_UNAVAILABLE",
                    "Store upgrade requires durable Operation and Job storage",
                    request_id,
                    None,
                ));
            };
            let Some(catalog_registry) = catalog_registry else {
                return Some(problem(
                    503,
                    "CATALOG_REGISTRY_UNAVAILABLE",
                    "Store upgrade requires explicitly configured trusted Catalog v2 sources",
                    request_id,
                    None,
                ));
            };
            replace_release(
                console,
                storage,
                catalog_registry,
                artifact_store,
                request,
                request_id,
                ReplacementAction::Upgrade,
            )
        }
        ("POST", "/api/v1/store/releases:rollback") => {
            let Some(storage) = storage else {
                return Some(problem(
                    503,
                    "STORE_STORAGE_UNAVAILABLE",
                    "Store rollback requires durable Operation and Job storage",
                    request_id,
                    None,
                ));
            };
            let Some(catalog_registry) = catalog_registry else {
                return Some(problem(
                    503,
                    "CATALOG_REGISTRY_UNAVAILABLE",
                    "Store rollback requires explicitly configured trusted Catalog v2 sources",
                    request_id,
                    None,
                ));
            };
            replace_release(
                console,
                storage,
                catalog_registry,
                artifact_store,
                request,
                request_id,
                ReplacementAction::Rollback,
            )
        }
        _ if path.starts_with("/api/v1/store/") => Err(StoreApiError::new(
            404,
            "ROUTE_NOT_FOUND",
            "the requested Store v1 route does not exist",
        )),
        _ => return None,
    };
    Some(match response {
        Ok(response) => response,
        Err(error) => problem(
            error.status,
            error.code,
            error.detail,
            request_id,
            error.operation_id.as_deref(),
        ),
    })
}

fn catalog_sources(
    storage: Option<&DurableStore>,
    registry: Option<&CatalogRegistry>,
    request: &ApiRequest,
    request_id: &str,
) -> Result<ApiResponse, StoreApiError> {
    let (storage, registry) = require_catalog_registry(storage, registry)?;
    let query = request
        .path
        .split_once('?')
        .map(|(_, value)| value)
        .unwrap_or("");
    let cursor = query_value(query, "cursor").map_err(|error| {
        StoreApiError::new(
            400,
            "CATALOG_QUERY_INVALID",
            format!("invalid cursor: {error}"),
        )
    })?;
    let limit = query_value(query, "limit")
        .map_err(|error| {
            StoreApiError::new(
                400,
                "CATALOG_QUERY_INVALID",
                format!("invalid limit: {error}"),
            )
        })?
        .map(|value| {
            value.parse::<usize>().map_err(|error| {
                StoreApiError::new(
                    400,
                    "CATALOG_PAGE_LIMIT_INVALID",
                    format!("limit must be a positive integer: {error}"),
                )
            })
        })
        .transpose()?;
    let page = registry
        .source_page(storage, cursor.as_deref(), limit)
        .map_err(catalog_registry_error)?;
    Ok(success(
        200,
        json!({"items": page.items, "next_cursor": page.next_cursor}),
        request_id,
    ))
}

fn register_catalog_source(
    storage: Option<&DurableStore>,
    registry: Option<&CatalogRegistry>,
    request: &ApiRequest,
    request_id: &str,
) -> Result<ApiResponse, StoreApiError> {
    let (storage, registry) = require_catalog_registry(storage, registry)?;
    let source: CatalogSourceRegistration = parse_body(request)?;
    let source = registry
        .register_source(storage, source)
        .map_err(catalog_registry_error)?;
    Ok(success(201, json!({"source": source}), request_id))
}

fn delete_catalog_source(
    storage: Option<&DurableStore>,
    registry: Option<&CatalogRegistry>,
    path: &str,
    request_id: &str,
) -> Result<ApiResponse, StoreApiError> {
    let (storage, registry) = require_catalog_registry(storage, registry)?;
    let source_id = path
        .strip_prefix("/api/v1/store/catalogs/")
        .filter(|value| !value.is_empty() && !value.contains('/'))
        .ok_or_else(|| {
            StoreApiError::new(
                400,
                "CATALOG_SOURCE_INVALID",
                "catalog source id is missing or invalid",
            )
        })?;
    registry
        .delete_source(storage, source_id)
        .map_err(catalog_registry_error)?;
    Ok(success(
        200,
        json!({"source_id": source_id, "deleted": true}),
        request_id,
    ))
}

fn list_catalog_packages(
    console: &OrchestratorActionConsole,
    storage: Option<&DurableStore>,
    registry: Option<&CatalogRegistry>,
    request: &ApiRequest,
    request_id: &str,
) -> Result<ApiResponse, StoreApiError> {
    let (storage, registry) = require_catalog_registry(storage, registry)?;
    let query = package_query(request)?;
    let page = registry
        .packages(storage, &query)
        .map_err(catalog_registry_error)?;
    let installed = market_api::installed_services(console).map_err(manager_error)?;
    Ok(success(
        200,
        json!({
            "items": page.items,
            "installed": installed,
            "next_cursor": page.next_cursor,
        }),
        request_id,
    ))
}

fn require_catalog_registry<'a>(
    storage: Option<&'a DurableStore>,
    registry: Option<&'a CatalogRegistry>,
) -> Result<(&'a DurableStore, &'a CatalogRegistry), StoreApiError> {
    let storage = storage.ok_or_else(|| {
        StoreApiError::new(
            503,
            "STORE_STORAGE_UNAVAILABLE",
            "Catalog v2 requires durable storage",
        )
    })?;
    let registry = registry.ok_or_else(|| {
        StoreApiError::new(
            503,
            "CATALOG_REGISTRY_UNAVAILABLE",
            "trusted Catalog v2 keys and sources are not configured",
        )
    })?;
    Ok((storage, registry))
}

fn package_query(request: &ApiRequest) -> Result<PackageQuery, StoreApiError> {
    let query = request
        .path
        .split_once('?')
        .map(|(_, value)| value)
        .unwrap_or("");
    let value = |name: &str| {
        query_value(query, name).map_err(|error| {
            StoreApiError::new(
                400,
                "CATALOG_QUERY_INVALID",
                format!("invalid query parameter {name}: {error}"),
            )
        })
    };
    let channel = value("channel")?
        .map(
            |channel| match channel.trim().to_ascii_lowercase().as_str() {
                "stable" => Ok(ReleaseChannel::Stable),
                "beta" => Ok(ReleaseChannel::Beta),
                "nightly" => Ok(ReleaseChannel::Nightly),
                _ => Err(StoreApiError::new(
                    400,
                    "CATALOG_CHANNEL_INVALID",
                    "channel must be stable, beta, or nightly",
                )),
            },
        )
        .transpose()?;
    let os = value("os")?;
    let arch = value("arch")?;
    let variant = value("variant")?;
    let platform = match (os, arch) {
        (Some(os), Some(arch)) => {
            let mut platform = TargetPlatform::new(os, arch);
            if let Some(variant) = variant {
                platform = platform.with_variant(variant);
            }
            Some(platform)
        }
        (None, None) if variant.is_none() => None,
        _ => {
            return Err(StoreApiError::new(
                400,
                "CATALOG_PLATFORM_INVALID",
                "os and arch must be supplied together; variant requires both",
            ));
        }
    };
    let limit = value("limit")?
        .map(|limit| {
            limit.parse::<usize>().map_err(|error| {
                StoreApiError::new(
                    400,
                    "CATALOG_PAGE_LIMIT_INVALID",
                    format!("limit must be a positive integer: {error}"),
                )
            })
        })
        .transpose()?;
    Ok(PackageQuery {
        search: value("search")?,
        channel,
        platform,
        cursor: value("cursor")?,
        limit,
    })
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ImportReleaseRequest {
    service_id: String,
    target_node_id: String,
    #[serde(default)]
    catalog_source_id: String,
    #[serde(default)]
    version: String,
    #[serde(default = "default_release_channel")]
    channel: String,
}

fn import_release(
    console: &mut OrchestratorActionConsole,
    storage: &DurableStore,
    registry: &CatalogRegistry,
    request: &ApiRequest,
    request_id: &str,
) -> Result<ApiResponse, StoreApiError> {
    let input: ImportReleaseRequest = parse_body(request)?;
    let service_id = required_text(&input.service_id, "service_id")?;
    let node_id = required_text(&input.target_node_id, "target_node_id")?;
    let node = storage
        .get_node(node_id)
        .map_err(storage_error)?
        .ok_or_else(|| {
            StoreApiError::new(
                404,
                "STORE_TARGET_NODE_NOT_FOUND",
                format!("target Node {node_id} was not found"),
            )
        })?;
    let platform = target_platform(&node)?;
    let resolved = registry
        .resolve_install_plan(
            storage,
            non_empty(&input.catalog_source_id),
            service_id,
            non_empty(&input.version),
            parse_release_channel(&input.channel)?,
            platform.clone(),
        )
        .map_err(catalog_registry_error)?;
    // Catalog signatures, dependency resolution, metadata checksums and the
    // immutable OCI references are all verified before the first publication.
    // Import is metadata-only: it never creates an Operation or calls an Agent.
    let documents = registry
        .fetch_release_documents(storage, &resolved)
        .map_err(catalog_registry_error)?;
    let mut imported = Vec::with_capacity(documents.len());
    for document in &documents {
        imported.push(
            console
                .register_external_release_document(
                    &document.bytes,
                    &document.source_url,
                    &document.checksum,
                )
                .map_err(core_error)?,
        );
    }
    Ok(success(
        201,
        json!({
            "imported": imported,
            "catalog_source_id": resolved.source_id,
            "catalog_id": resolved.catalog_id,
            "verified_key_ids": resolved.verified_key_ids,
            "target_platform": platform,
            "side_effects": {
                "operations": 0,
                "jobs": 0,
                "runtime_calls": 0,
            },
        }),
        request_id,
    ))
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ValidateReleaseRequest {
    service_id: String,
    target_node_id: String,
    #[serde(default)]
    catalog_source_id: String,
    #[serde(default)]
    version: String,
    #[serde(default = "default_release_channel")]
    channel: String,
}

fn validate_release_catalog(
    storage: &DurableStore,
    registry: &CatalogRegistry,
    request: &ApiRequest,
    request_id: &str,
) -> Result<ApiResponse, StoreApiError> {
    let input: ValidateReleaseRequest = parse_body(request)?;
    let service_id = required_text(&input.service_id, "service_id")?;
    let node_id = required_text(&input.target_node_id, "target_node_id")?;
    let node = storage
        .get_node(node_id)
        .map_err(storage_error)?
        .ok_or_else(|| {
            StoreApiError::new(
                404,
                "STORE_TARGET_NODE_NOT_FOUND",
                format!("target Node {node_id} was not found"),
            )
        })?;
    if !node
        .labels
        .get("runtime")
        .and_then(Value::as_str)
        .is_some_and(|runtime| runtime.eq_ignore_ascii_case("docker"))
    {
        return Err(StoreApiError::new(
            422,
            "STORE_DOCKER_CAPABILITY_REQUIRED",
            format!("target Node {node_id} does not advertise runtime=docker"),
        ));
    }
    let platform = target_platform(&node)?;
    let resolved = registry
        .resolve_install_plan(
            storage,
            non_empty(&input.catalog_source_id),
            service_id,
            non_empty(&input.version),
            parse_release_channel(&input.channel)?,
            platform.clone(),
        )
        .map_err(catalog_registry_error)?;
    let documents = registry
        .fetch_release_documents(storage, &resolved)
        .map_err(catalog_registry_error)?;
    let metadata = documents
        .iter()
        .map(|document| {
            json!({
                "module_id": document.selection.module_id,
                "version": document.selection.release.version,
                "metadata_url": document.source_url,
                "metadata_sha256": document.checksum,
                "oci_image": document.selection.release.oci_image,
                "offline_oci_layout_verified": document
                    .offline_oci_layout
                    .as_ref()
                    .map(|path| path.display().to_string()),
            })
        })
        .collect::<Vec<_>>();
    Ok(success(
        200,
        json!({
            "valid": true,
            "catalog_source_id": resolved.source_id,
            "catalog_id": resolved.catalog_id,
            "verified_key_ids": resolved.verified_key_ids,
            "target_platform": platform,
            "plan": resolved.plan,
            "metadata": metadata,
            "side_effects": {
                "release_imports": 0,
                "operations": 0,
                "jobs": 0,
                "runtime_calls": 0,
            }
        }),
        request_id,
    ))
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct InstallReleaseRequest {
    #[serde(default)]
    service_id: String,
    #[serde(default)]
    catalog_source_id: String,
    #[serde(default)]
    source_url: String,
    #[serde(default, alias = "metadata_sha256")]
    checksum: String,
    #[serde(default)]
    version: String,
    #[serde(default = "default_release_channel")]
    channel: String,
    #[serde(default)]
    target_node_id: String,
    #[serde(default)]
    endpoint: String,
    #[serde(default = "default_managed_mode")]
    mode: String,
    #[serde(default = "default_true")]
    start: bool,
    #[serde(default = "default_apply_policy")]
    migration_policy: String,
    #[serde(default)]
    gateway_node_id: String,
    #[serde(default)]
    config: Value,
    #[serde(default)]
    secret_refs: BTreeMap<String, String>,
}

fn default_managed_mode() -> String {
    "MANAGED".to_string()
}

fn default_apply_policy() -> String {
    "APPLY".to_string()
}

fn default_true() -> bool {
    true
}

fn default_release_channel() -> String {
    "stable".to_string()
}

fn install_release(
    console: &mut OrchestratorActionConsole,
    storage: &DurableStore,
    catalog_registry: &CatalogRegistry,
    artifact_store: Option<&ArtifactStore>,
    request: &ApiRequest,
    request_id: &str,
) -> Result<ApiResponse, StoreApiError> {
    let input: InstallReleaseRequest = parse_body(request)?;
    if !input.source_url.trim().is_empty() || !input.checksum.trim().is_empty() {
        return Err(StoreApiError::new(
            422,
            "CATALOG_INSTALL_REQUIRED",
            "release.install resolves only trusted Catalog v2 content; use releases:import for an explicit metadata-only import",
        ));
    }
    let external = if input.mode.eq_ignore_ascii_case("MANAGED") {
        false
    } else if input.mode.eq_ignore_ascii_case("EXTERNAL") {
        true
    } else {
        return Err(StoreApiError::new(
            422,
            "STORE_INSTALL_MODE_INVALID",
            "mode must be MANAGED or EXTERNAL",
        ));
    };
    if !external && input.target_node_id.trim().is_empty() {
        return Err(StoreApiError::new(
            422,
            "STORE_TARGET_NODE_REQUIRED",
            "target_node_id is required for a Managed install",
        ));
    }
    if external && input.endpoint.trim().is_empty() {
        return Err(StoreApiError::new(
            422,
            "STORE_EXTERNAL_ENDPOINT_REQUIRED",
            "endpoint is required for an External install",
        ));
    }
    if external && !input.start {
        return Err(StoreApiError::new(
            422,
            "STORE_EXTERNAL_START_REQUIRED",
            "External install can only register an endpoint after it is healthy and running",
        ));
    }
    let node = non_empty(&input.target_node_id)
        .map(|node_id| {
            storage
                .get_node(node_id)
                .map_err(storage_error)?
                .ok_or_else(|| {
                    StoreApiError::new(
                        404,
                        "STORE_TARGET_NODE_NOT_FOUND",
                        format!("target Node {node_id} was not found"),
                    )
                })
        })
        .transpose()?;
    if !external {
        ensure_ready_docker_node(node.as_ref().expect("managed node was required"))?;
    }
    let platform = node
        .as_ref()
        .map(target_platform)
        .transpose()?
        .unwrap_or_else(host_platform);
    let service_id = required_text(&input.service_id, "service_id")?.to_string();
    let channel = parse_release_channel(&input.channel)?;
    let resolved = catalog_registry
        .resolve_install_plan(
            storage,
            non_empty(&input.catalog_source_id),
            &service_id,
            non_empty(&input.version),
            channel,
            platform.clone(),
        )
        .map_err(catalog_registry_error)?;
    let documents = catalog_registry
        .fetch_release_documents(storage, &resolved)
        .map_err(catalog_registry_error)?;
    let external_missing_dependencies = if external {
        missing_resolved_dependencies(storage, &resolved, &service_id)?
    } else {
        Vec::new()
    };
    if external && !external_missing_dependencies.is_empty() {
        let dependency_node = node.as_ref().ok_or_else(|| {
            StoreApiError::new(
                422,
                "STORE_TARGET_NODE_REQUIRED",
                "target_node_id is required when an External release has managed dependencies to install",
            )
        })?;
        ensure_ready_docker_node(dependency_node)?;
    }

    // All catalog, signature, dependency, metadata, checksum, and OCI checks
    // have completed before durable publication begins. Publication is atomic
    // per Service+Release and has no runtime side effect.
    let mut imported = Vec::with_capacity(documents.len());
    for document in &documents {
        imported.push(
            console
                .register_external_release_document(
                    &document.bytes,
                    &document.source_url,
                    &document.checksum,
                )
                .map_err(core_error)?,
        );
    }
    let selected = select_release(
        console,
        &service_id,
        Some(&resolved.plan.root.version.to_string()),
    )?;
    ensure_release_checksum(&selected.record)?;
    let root_release = resolved
        .plan
        .releases
        .last()
        .filter(|release| release.module_id == service_id)
        .ok_or_else(|| {
            StoreApiError::new(
                500,
                "CATALOG_PLAN_INVALID",
                "resolved dependency plan does not end with its requested root",
            )
        })?;
    let image =
        OciImageReference::parse(root_release.release.oci_image.as_str()).map_err(|error| {
            StoreApiError::new(
                422,
                "STORE_IMMUTABLE_IMAGE_REQUIRED",
                format!(
                    "release {}@{} must use repository@sha256:<64 lowercase hex>: {error}",
                    service_id, selected.version
                ),
            )
        })?;
    if external {
        validate_external_install_endpoint(
            &service_id,
            input.endpoint.trim(),
            &selected.manifest.backend.protocol,
        )?;
        return enqueue_external_install(
            console,
            storage,
            request,
            request_id,
            &input,
            &service_id,
            &selected,
            &resolved,
            root_release,
            image,
            platform,
            imported,
            &documents,
            node.as_ref(),
            &external_missing_dependencies,
            artifact_store,
        );
    }
    let node = node.expect("managed install requires a target Node");
    let root_deployment_id = deployment_id(&service_id, &selected.version, &node.node_id);
    let operation_id = operation_id("store-install", &root_deployment_id, request)?;
    let existing = storage.runtime_instances(None).map_err(storage_error)?;
    let missing = resolved
        .plan
        .releases
        .iter()
        .filter(|selection| {
            selection.module_id == service_id
                || !existing.iter().any(|deployment| {
                    deployment.instance.service_id == selection.module_id
                        && deployment.instance.observed_state == RuntimeObservedState::Running
                        && deployment.instance.health.eq_ignore_ascii_case("HEALTHY")
                        && artifact_matches(
                            selection.release.oci_image.as_str(),
                            &deployment.instance.artifact_digest,
                        )
                })
        })
        .collect::<Vec<_>>();
    let install_steps = missing
        .iter()
        .map(|selection| {
            (
                selection.module_id.clone(),
                format!(
                    "install-{}-{}",
                    selection.module_id, selection.release.version
                ),
            )
        })
        .collect::<BTreeMap<_, _>>();
    let mut jobs = Vec::new();
    let mut planned_deployments = Vec::new();
    let mut root_spec = None;
    let empty_secret_refs = BTreeMap::new();
    for selection in &missing {
        let release = select_release(
            console,
            &selection.module_id,
            Some(&selection.release.version.to_string()),
        )?;
        ensure_release_checksum(&release.record)?;
        let release_image = OciImageReference::parse(selection.release.oci_image.as_str())
            .map_err(|error| {
                StoreApiError::new(
                    422,
                    "STORE_IMMUTABLE_IMAGE_REQUIRED",
                    format!(
                        "release {}@{} has invalid immutable image: {error}",
                        selection.module_id, selection.release.version
                    ),
                )
            })?;
        let release_deployment_id = deployment_id(
            &selection.module_id,
            &selection.release.version,
            &node.node_id,
        );
        ensure_deployment_available(
            storage,
            &release_deployment_id,
            release_image.digest(),
            Some(&operation_id),
        )?;
        let spec = container_spec(
            &release_deployment_id,
            &selection.module_id,
            &selection.release.version,
            &release.record.checksum,
            &node,
            release_image,
            &release.manifest,
            if selection.module_id == service_id {
                managed_published_endpoint(
                    &input.endpoint,
                    &selection.module_id,
                    &node,
                    &release.manifest,
                )?
            } else {
                None
            },
        );
        if selection.module_id == service_id {
            root_spec = Some(spec.clone());
        }
        let runtime_install = RuntimeInstallPayload {
            spec,
            start: if selection.module_id == service_id {
                input.start
            } else {
                true
            },
            health_gate: HealthGatePolicy::default(),
            offline_oci_artifact: offline_artifact_for_release(
                storage,
                artifact_store,
                &documents,
                &selection.module_id,
                &selection.release.version,
            )?,
        };
        let pipeline = release_pipeline_payload(
            &release.manifest,
            &runtime_install,
            &node,
            &operation_id,
            &input.migration_policy,
            &input.gateway_node_id,
            if selection.module_id == service_id {
                &input.config
            } else {
                &Value::Null
            },
            if selection.module_id == service_id {
                &input.secret_refs
            } else {
                &empty_secret_refs
            },
        )?;
        let (kind, payload, max_attempts) = if let Some(pipeline) = pipeline {
            (
                JobKind::ReleasePipeline,
                serde_json::to_value(pipeline).map_err(|error| {
                    StoreApiError::new(
                        500,
                        "STORE_PIPELINE_INVALID",
                        format!("serialize release pipeline: {error}"),
                    )
                })?,
                1,
            )
        } else {
            (
                JobKind::Install,
                serde_json::to_value(runtime_install).map_err(|error| {
                    StoreApiError::new(
                        500,
                        "STORE_INSTALL_INVALID",
                        format!("serialize runtime install: {error}"),
                    )
                })?,
                3,
            )
        };
        let depends_on = selection
            .release
            .dependencies
            .iter()
            .filter_map(|dependency| install_steps.get(&dependency.module_id).cloned())
            .collect();
        let step_id = install_steps
            .get(&selection.module_id)
            .expect("missing releases were indexed")
            .clone();
        jobs.push(PlannedJob {
            step_id,
            node_id: node.node_id.clone(),
            kind,
            depends_on,
            condition: PlannedJobCondition::OnSuccess,
            payload,
            max_attempts,
        });
        planned_deployments.push((
            release_deployment_id,
            selection.release.oci_image.digest().as_str().to_string(),
        ));
    }
    // Every install/pipeline step compensates its own partially-created runtime
    // resources.  Successfully installed dependencies are intentionally retained:
    // another operation may have started referencing the shared deployment after
    // this plan was accepted, so an operation-local unconditional uninstall would
    // be unsafe.
    let spec = root_spec.ok_or_else(|| {
        StoreApiError::new(
            500,
            "CATALOG_PLAN_INVALID",
            "managed dependency plan did not include its requested root",
        )
    })?;
    let plan = PlanOperation {
        operation_id: operation_id.clone(),
        action: "release.install".to_string(),
        target_type: "Release".to_string(),
        target_id: format!("{service_id}@{}", selected.version),
        request: json!({
            "service_id": service_id,
            "version": selected.version,
            "target_node_id": node.node_id,
            "target_platform": platform,
            "mode": "MANAGED",
            "channel": channel,
            "deployment_id": root_deployment_id,
            "endpoint": spec
                .published_endpoint
                .as_ref()
                .map(|endpoint| endpoint.endpoint.as_str()),
            "planned_deployment_ids": planned_deployments
                .iter()
                .map(|(deployment_id, _)| deployment_id)
                .collect::<Vec<_>>(),
            "start": input.start,
            "migration_policy": input.migration_policy.to_ascii_uppercase(),
            "release_checksum": selected.record.checksum,
            "image": spec.image.to_string(),
            "catalog_source_id": resolved.source_id,
            "catalog_id": resolved.catalog_id,
            "catalog_verified_key_ids": resolved.verified_key_ids,
            "catalog_plan": resolved.plan,
            "auto_enqueue": true,
        }),
        jobs,
    };
    let _plan_guard = store_plan_guard()?;
    if let Some(endpoint) = spec.published_endpoint.as_ref() {
        ensure_endpoint_available(storage, endpoint, None, Some(&operation_id))?;
    }
    for (planned_deployment_id, digest) in &planned_deployments {
        ensure_deployment_available(storage, planned_deployment_id, digest, Some(&operation_id))?;
    }
    let operation = enqueue_plan(storage, plan)?;
    Ok(success(
        202,
        json!({
            "operation_id": operation_id,
            "operation": operation,
            "deployment_id": root_deployment_id,
            "endpoint": spec
                .published_endpoint
                .as_ref()
                .map(|endpoint| endpoint.endpoint.as_str()),
            "release": {
                "service_id": service_id,
                "version": selected.version,
                "checksum": selected.record.checksum,
                "image": root_release.release.oci_image,
                "target_platform": platform,
            },
            "imported": imported,
            "lifecycle": "Deploying",
            "installed": false,
        }),
        request_id,
    ))
}

#[allow(clippy::too_many_arguments)]
fn enqueue_external_install(
    console: &OrchestratorActionConsole,
    storage: &DurableStore,
    request: &ApiRequest,
    request_id: &str,
    input: &InstallReleaseRequest,
    service_id: &str,
    selected: &SelectedRelease,
    resolved: &ResolvedCatalogPlan,
    root_release: &orchestrator_manager::catalog_v2::ResolvedReleaseV2,
    image: OciImageReference,
    platform: TargetPlatform,
    imported: Vec<orchestrator_legacy::ExternalReleaseImport>,
    documents: &[VerifiedReleaseDocument],
    node: Option<&NodeRecord>,
    missing_dependencies: &[&orchestrator_manager::catalog_v2::ResolvedReleaseV2],
    artifact_store: Option<&ArtifactStore>,
) -> Result<ApiResponse, StoreApiError> {
    let endpoint = input.endpoint.trim();
    let root_external_deployment_id = deployment_id(
        service_id,
        &selected.version,
        &format!("external:{endpoint}"),
    );
    let operation_id = operation_id(
        "store-external-install",
        &root_external_deployment_id,
        request,
    )?;
    let image = image.to_string();
    let channel = parse_release_channel(&input.channel)?;
    let install_steps = missing_dependencies
        .iter()
        .map(|selection| {
            (
                selection.module_id.clone(),
                format!(
                    "install-{}-{}",
                    selection.module_id, selection.release.version
                ),
            )
        })
        .collect::<BTreeMap<_, _>>();
    let mut jobs = Vec::new();
    let mut planned_dependency_deployments = Vec::new();
    if !missing_dependencies.is_empty() {
        let node = node.expect("missing External dependencies require a validated Node");
        let empty_secret_refs = BTreeMap::new();
        for selection in missing_dependencies {
            let release = select_release(
                console,
                &selection.module_id,
                Some(&selection.release.version.to_string()),
            )?;
            ensure_release_checksum(&release.record)?;
            let release_image = OciImageReference::parse(selection.release.oci_image.as_str())
                .map_err(|error| {
                    StoreApiError::new(
                        422,
                        "STORE_IMMUTABLE_IMAGE_REQUIRED",
                        format!(
                            "dependency release {}@{} has invalid immutable image: {error}",
                            selection.module_id, selection.release.version
                        ),
                    )
                })?;
            let dependency_deployment_id = deployment_id(
                &selection.module_id,
                &selection.release.version,
                &node.node_id,
            );
            ensure_deployment_available(
                storage,
                &dependency_deployment_id,
                release_image.digest(),
                Some(&operation_id),
            )?;
            let spec = container_spec(
                &dependency_deployment_id,
                &selection.module_id,
                &selection.release.version,
                &release.record.checksum,
                node,
                release_image,
                &release.manifest,
                None,
            );
            let runtime_install = RuntimeInstallPayload {
                spec,
                start: true,
                health_gate: HealthGatePolicy::default(),
                offline_oci_artifact: offline_artifact_for_release(
                    storage,
                    artifact_store,
                    documents,
                    &selection.module_id,
                    &selection.release.version,
                )?,
            };
            let pipeline = release_pipeline_payload(
                &release.manifest,
                &runtime_install,
                node,
                &operation_id,
                &input.migration_policy,
                &input.gateway_node_id,
                &Value::Null,
                &empty_secret_refs,
            )?;
            let (kind, payload, max_attempts) = if let Some(pipeline) = pipeline {
                (
                    JobKind::ReleasePipeline,
                    serde_json::to_value(pipeline).map_err(|error| {
                        StoreApiError::new(
                            500,
                            "STORE_PIPELINE_INVALID",
                            format!("serialize dependency release pipeline: {error}"),
                        )
                    })?,
                    1,
                )
            } else {
                (
                    JobKind::Install,
                    serde_json::to_value(runtime_install).map_err(|error| {
                        StoreApiError::new(
                            500,
                            "STORE_INSTALL_INVALID",
                            format!("serialize dependency install: {error}"),
                        )
                    })?,
                    3,
                )
            };
            jobs.push(PlannedJob {
                step_id: install_steps
                    .get(&selection.module_id)
                    .expect("missing dependency was indexed")
                    .clone(),
                node_id: node.node_id.clone(),
                kind,
                depends_on: selection
                    .release
                    .dependencies
                    .iter()
                    .filter_map(|dependency| install_steps.get(&dependency.module_id).cloned())
                    .collect(),
                condition: PlannedJobCondition::OnSuccess,
                payload,
                max_attempts,
            });
            planned_dependency_deployments.push((
                dependency_deployment_id,
                selection.release.oci_image.digest().as_str().to_string(),
            ));
        }
    }
    let install_step_ids = jobs
        .iter()
        .map(|job| job.step_id.clone())
        .collect::<Vec<_>>();
    jobs.push(PlannedJob {
        step_id: "external-health".to_string(),
        node_id: CONTROL_PLANE_NODE_ID.to_string(),
        kind: JobKind::ExternalHealth,
        depends_on: install_step_ids.clone(),
        condition: PlannedJobCondition::OnSuccess,
        payload: json!({
            "deployment_id": root_external_deployment_id,
            "service_id": service_id,
            "version": selected.version,
            "endpoint": endpoint,
            "protocol": selected.manifest.backend.protocol,
            "health_path": selected.manifest.backend.health_path,
            "artifact_digest": image,
        }),
        max_attempts: 3,
    });
    let plan = PlanOperation {
        operation_id: operation_id.clone(),
        action: "release.install".to_string(),
        target_type: "Release".to_string(),
        target_id: format!("{service_id}@{}", selected.version),
        request: json!({
            "service_id": service_id,
            "version": selected.version,
            "target_node_id": non_empty(&input.target_node_id),
            "target_platform": platform,
            "deployment_id": root_external_deployment_id,
            "planned_deployment_ids": std::iter::once(&root_external_deployment_id)
                .chain(planned_dependency_deployments.iter().map(|(deployment_id, _)| deployment_id))
                .collect::<Vec<_>>(),
            "endpoint": endpoint,
            "mode": "EXTERNAL",
            "start": true,
            "channel": channel,
            "release_checksum": selected.record.checksum,
            "image": image,
            "catalog_source_id": resolved.source_id,
            "catalog_id": resolved.catalog_id,
            "catalog_verified_key_ids": resolved.verified_key_ids,
            "catalog_plan": resolved.plan,
            "auto_enqueue": true,
        }),
        jobs,
    };
    let _plan_guard = store_plan_guard()?;
    ensure_deployment_available(
        storage,
        &root_external_deployment_id,
        image_digest(&image)?,
        Some(&operation_id),
    )?;
    for (dependency_deployment_id, digest) in &planned_dependency_deployments {
        ensure_deployment_available(
            storage,
            dependency_deployment_id,
            digest,
            Some(&operation_id),
        )?;
    }
    let operation = enqueue_plan(storage, plan)?;
    Ok(success(
        202,
        json!({
            "operation_id": operation_id,
            "operation": operation,
            "deployment_id": root_external_deployment_id,
            "release": {
                "service_id": service_id,
                "version": selected.version,
                "checksum": selected.record.checksum,
                "image": root_release.release.oci_image,
                "target_platform": platform,
            },
            "endpoint": endpoint,
            "mode": "EXTERNAL",
            "imported": imported,
            "lifecycle": "Validating",
            "installed": false,
        }),
        request_id,
    ))
}

fn image_digest(image: &str) -> Result<&str, StoreApiError> {
    image
        .split_once('@')
        .map(|(_, digest)| digest)
        .ok_or_else(|| {
            StoreApiError::new(
                500,
                "STORE_IMMUTABLE_IMAGE_REQUIRED",
                "validated OCI image unexpectedly has no digest",
            )
        })
}

fn validate_external_install_endpoint(
    service_id: &str,
    endpoint: &str,
    protocol: &str,
) -> Result<(), StoreApiError> {
    if !matches!(protocol, "http" | "https" | "tcp" | "postgres" | "redis") {
        return Err(StoreApiError::new(
            422,
            "STORE_EXTERNAL_PROTOCOL_UNSUPPORTED",
            format!("External health provider does not support protocol {protocol}"),
        ));
    }
    if let Some((scheme, authority)) = endpoint.split_once("://") {
        if scheme != protocol
            || authority.trim().is_empty()
            || authority.chars().any(char::is_whitespace)
        {
            return Err(StoreApiError::new(
                422,
                "STORE_EXTERNAL_ENDPOINT_INVALID",
                format!("External endpoint must be a valid {protocol} URI"),
            ));
        }
        return Ok(());
    }
    orchestrator_legacy::validate_endpoint_service_name(endpoint, service_id).map_err(core_error)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ReplacementAction {
    Upgrade,
    Rollback,
}

impl ReplacementAction {
    fn action_id(self) -> &'static str {
        match self {
            Self::Upgrade => "release.upgrade",
            Self::Rollback => "release.rollback",
        }
    }

    fn operation_prefix(self) -> &'static str {
        match self {
            Self::Upgrade => "store-upgrade",
            Self::Rollback => "store-rollback",
        }
    }

    fn job_kind(self) -> JobKind {
        match self {
            Self::Upgrade => JobKind::Upgrade,
            Self::Rollback => JobKind::Rollback,
        }
    }

    fn lifecycle(self) -> &'static str {
        match self {
            Self::Upgrade => "Upgrading",
            Self::Rollback => "RollingBack",
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ReplaceReleaseRequest {
    deployment_id: String,
    #[serde(default)]
    catalog_source_id: String,
    #[serde(default)]
    version: String,
    #[serde(default)]
    channel: Option<String>,
    #[serde(default = "default_apply_policy")]
    migration_policy: String,
    #[serde(default)]
    endpoint: String,
    #[serde(default)]
    gateway_node_id: String,
    #[serde(default)]
    config: Value,
    #[serde(default)]
    secret_refs: BTreeMap<String, String>,
}

#[derive(Debug, Clone)]
struct ReleaseHistoryProof {
    operation_id: String,
    deployment_id: String,
    service_id: String,
    version: semver::Version,
    image: String,
    channel: ReleaseChannel,
    catalog_source_id: String,
    catalog_id: String,
    verified_key_ids: Vec<String>,
    updated_at_ms: i64,
}

fn replace_release(
    console: &mut OrchestratorActionConsole,
    storage: &DurableStore,
    catalog_registry: &CatalogRegistry,
    artifact_store: Option<&ArtifactStore>,
    request: &ApiRequest,
    request_id: &str,
    action: ReplacementAction,
) -> Result<ApiResponse, StoreApiError> {
    let input: ReplaceReleaseRequest = parse_body(request)?;
    let current_deployment_id = required_text(&input.deployment_id, "deployment_id")?;
    let current = storage
        .runtime_instance(current_deployment_id)
        .map_err(storage_error)?
        .ok_or_else(|| {
            StoreApiError::new(
                404,
                "STORE_DEPLOYMENT_NOT_FOUND",
                format!("deployment {current_deployment_id} was not found"),
            )
        })?;
    if current.instance.container_id.trim().is_empty()
        || current.instance.observed_state != RuntimeObservedState::Running
    {
        return Err(StoreApiError::new(
            409,
            "STORE_REPLACEMENT_SOURCE_NOT_RUNNING",
            format!(
                "deployment {current_deployment_id} must have a proven container and RUNNING observed state"
            ),
        ));
    }
    let node = storage
        .get_node(&current.node_id)
        .map_err(storage_error)?
        .ok_or_else(|| {
            StoreApiError::new(
                409,
                "STORE_DEPLOYMENT_NODE_MISSING",
                format!(
                    "deployment {current_deployment_id} references missing Node {}",
                    current.node_id
                ),
            )
        })?;
    ensure_ready_docker_node(&node)?;
    let platform = target_platform(&node)?;
    let history = release_history(storage, &current.instance.service_id)?;
    let current_proof = history
        .iter()
        .find(|proof| {
            proof.deployment_id == current.instance.deployment_id
                && artifact_matches(&proof.image, &current.instance.artifact_digest)
        })
        .cloned()
        .ok_or_else(|| {
            StoreApiError::new(
                422,
                "STORE_CURRENT_RELEASE_UNPROVEN",
                format!(
                    "deployment {current_deployment_id} has no successful trusted Catalog Operation proving its version and digest"
                ),
            )
        })?;
    let requested_channel = input
        .channel
        .as_deref()
        .map(parse_release_channel)
        .transpose()?;
    let requested_source = non_empty(&input.catalog_source_id);

    let rollback_proof = if action == ReplacementAction::Rollback {
        let requested_version = non_empty(&input.version)
            .map(semver::Version::parse)
            .transpose()
            .map_err(|error| {
                StoreApiError::new(
                    422,
                    "STORE_ROLLBACK_VERSION_INVALID",
                    format!("rollback version is not semver: {error}"),
                )
            })?;
        Some(
            history
                .iter()
                .find(|proof| {
                    proof.deployment_id != current.instance.deployment_id
                        && !artifact_matches(&proof.image, &current.instance.artifact_digest)
                        && requested_version
                            .as_ref()
                            .is_none_or(|version| version == &proof.version)
                        && requested_source
                            .is_none_or(|source| source == proof.catalog_source_id)
                        && requested_channel
                            .as_ref()
                            .is_none_or(|channel| channel == &proof.channel)
                })
                .cloned()
                .ok_or_else(|| {
                    StoreApiError::new(
                        422,
                        "STORE_ROLLBACK_HISTORY_UNPROVEN",
                        requested_version.map_or_else(
                            || {
                                format!(
                                    "service {} has no prior successful trusted Catalog release distinct from the current deployment",
                                    current.instance.service_id
                                )
                            },
                            |version| {
                                format!(
                                    "service {} has no successful trusted Catalog Operation proving rollback target {version}",
                                    current.instance.service_id
                                )
                            },
                        ),
                    )
                })?,
        )
    } else {
        None
    };
    let channel = match action {
        ReplacementAction::Upgrade => requested_channel.unwrap_or(ReleaseChannel::Stable),
        ReplacementAction::Rollback => requested_channel.unwrap_or_else(|| {
            rollback_proof
                .as_ref()
                .map(|proof| proof.channel)
                .unwrap_or(ReleaseChannel::Stable)
        }),
    };

    let target_version = match action {
        ReplacementAction::Upgrade => non_empty(&input.version).map(str::to_string),
        ReplacementAction::Rollback => rollback_proof
            .as_ref()
            .map(|proof| proof.version.to_string()),
    };
    let source_id = requested_source.or_else(|| {
        rollback_proof
            .as_ref()
            .map(|proof| proof.catalog_source_id.as_str())
            .or(Some(current_proof.catalog_source_id.as_str()))
    });
    let resolved = catalog_registry
        .resolve_install_plan(
            storage,
            source_id,
            &current.instance.service_id,
            target_version.as_deref(),
            channel,
            platform.clone(),
        )
        .map_err(catalog_registry_error)?;
    let root_release = resolved
        .plan
        .releases
        .last()
        .filter(|release| release.module_id == current.instance.service_id)
        .cloned()
        .ok_or_else(|| {
            StoreApiError::new(
                500,
                "CATALOG_PLAN_INVALID",
                "resolved replacement plan does not end with its requested root",
            )
        })?;
    if action == ReplacementAction::Upgrade && root_release.release.version <= current_proof.version
    {
        return Err(StoreApiError::new(
            422,
            "STORE_UPGRADE_VERSION_NOT_NEWER",
            format!(
                "upgrade target {} must be newer than proven current version {}; use rollback for an older release",
                root_release.release.version, current_proof.version
            ),
        ));
    }
    if artifact_matches(
        root_release.release.oci_image.as_str(),
        &current.instance.artifact_digest,
    ) {
        return Err(StoreApiError::new(
            409,
            "STORE_RELEASE_ALREADY_INSTALLED",
            format!(
                "deployment {current_deployment_id} already uses {}",
                root_release.release.oci_image
            ),
        ));
    }
    if let Some(proof) = &rollback_proof
        && proof.image != root_release.release.oci_image.as_str()
    {
        return Err(StoreApiError::new(
            422,
            "STORE_ROLLBACK_HISTORY_MISMATCH",
            format!(
                "trusted catalog now resolves {}@{} to {}, but historical Operation {} proves {}; rollback refuses a changed artifact",
                proof.service_id,
                proof.version,
                root_release.release.oci_image,
                proof.operation_id,
                proof.image
            ),
        ));
    }

    let documents = catalog_registry
        .fetch_release_documents(storage, &resolved)
        .map_err(catalog_registry_error)?;
    let missing_dependencies =
        missing_resolved_dependencies(storage, &resolved, &current.instance.service_id)?;
    let mut imported = Vec::with_capacity(documents.len());
    for document in &documents {
        imported.push(
            console
                .register_external_release_document(
                    &document.bytes,
                    &document.source_url,
                    &document.checksum,
                )
                .map_err(core_error)?,
        );
    }
    let selected = select_release(
        console,
        &current.instance.service_id,
        Some(&root_release.release.version.to_string()),
    )?;
    ensure_release_checksum(&selected.record)?;
    let image =
        OciImageReference::parse(root_release.release.oci_image.as_str()).map_err(|error| {
            StoreApiError::new(
                422,
                "STORE_IMMUTABLE_IMAGE_REQUIRED",
                format!("catalog replacement image is invalid: {error}"),
            )
        })?;
    let new_deployment_id = deployment_id(
        &current.instance.service_id,
        &root_release.release.version,
        &node.node_id,
    );
    let replacement_endpoint = if input.endpoint.trim().is_empty() {
        current.endpoint.as_str()
    } else {
        input.endpoint.trim()
    };
    let published_endpoint = managed_published_endpoint(
        replacement_endpoint,
        &current.instance.service_id,
        &node,
        &selected.manifest,
    )?;
    if let Some(endpoint) = published_endpoint.as_ref()
        && !current.endpoint.trim().is_empty()
        && endpoint_socket(&current.endpoint) == endpoint_socket(&endpoint.endpoint)
    {
        return Err(StoreApiError::new(
            409,
            "STORE_REPLACEMENT_ENDPOINT_CUTOVER_UNSUPPORTED",
            format!(
                "{} cannot reuse published endpoint {} while the proven old container remains Running; specify a different host port for an explicit coexistence cutover",
                action.action_id(),
                current.endpoint
            ),
        ));
    }
    let spec = container_spec(
        &new_deployment_id,
        &current.instance.service_id,
        &root_release.release.version,
        &selected.record.checksum,
        &node,
        image,
        &selected.manifest,
        published_endpoint,
    );
    let operation_target = format!("{}->{}", current.instance.deployment_id, new_deployment_id);
    let operation_id = operation_id(action.operation_prefix(), &operation_target, request)?;
    let replacement_install = RuntimeInstallPayload {
        spec: spec.clone(),
        start: true,
        health_gate: HealthGatePolicy::default(),
        offline_oci_artifact: offline_artifact_for_release(
            storage,
            artifact_store,
            &documents,
            &current.instance.service_id,
            &root_release.release.version,
        )?,
    };
    let desired_pipeline = release_pipeline_payload(
        &selected.manifest,
        &replacement_install,
        &node,
        &operation_id,
        &input.migration_policy,
        &input.gateway_node_id,
        &input.config,
        &input.secret_refs,
    )?;
    let previous_provider_revision =
        provider_revision_from_operation(storage, &current_proof.operation_id)?;
    let (materialization, migrations, desired_auth, desired_provisioners, desired_gateway) =
        desired_pipeline.map_or_else(
            || (None, Vec::new(), None, Vec::new(), None),
            |pipeline| {
                (
                    pipeline.materialization,
                    pipeline.migrations,
                    pipeline.auth,
                    pipeline.provisioners,
                    pipeline.gateway,
                )
            },
        );
    let desired_provider_revision = ReleaseProviderRevision {
        revision_id: operation_id.clone(),
        auth: desired_auth,
        provisioners: desired_provisioners,
        gateway: desired_gateway,
    };
    let provider_saga = (previous_provider_revision.has_managed_state()
        || desired_provider_revision.has_managed_state())
    .then_some(ReplacementProviderSaga {
        previous: previous_provider_revision,
        desired: desired_provider_revision,
    });
    let payload = ReleaseReplacementPayload {
        old_deployment_id: current.instance.deployment_id.clone(),
        old_container_id: current.instance.container_id.clone(),
        new_spec: spec.clone(),
        start: true,
        health_gate: HealthGatePolicy::default(),
        offline_oci_artifact: replacement_install.offline_oci_artifact,
        materialization,
        migrations,
        provider_saga,
    };
    payload.validate().map_err(|error| {
        StoreApiError::new(
            422,
            "STORE_REPLACEMENT_INVALID",
            format!("replacement plan is invalid: {error}"),
        )
    })?;
    let rollback_proof_operation_id = rollback_proof
        .as_ref()
        .map(|proof| proof.operation_id.clone());
    let install_steps = missing_dependencies
        .iter()
        .map(|selection| {
            (
                selection.module_id.clone(),
                format!(
                    "install-{}-{}",
                    selection.module_id, selection.release.version
                ),
            )
        })
        .collect::<BTreeMap<_, _>>();
    let mut jobs = Vec::new();
    let mut planned_dependency_deployments = Vec::new();
    let empty_secret_refs = BTreeMap::new();
    for dependency in &missing_dependencies {
        let release = select_release(
            console,
            &dependency.module_id,
            Some(&dependency.release.version.to_string()),
        )?;
        ensure_release_checksum(&release.record)?;
        let dependency_image = OciImageReference::parse(dependency.release.oci_image.as_str())
            .map_err(|error| {
                StoreApiError::new(
                    422,
                    "STORE_IMMUTABLE_IMAGE_REQUIRED",
                    format!(
                        "dependency release {}@{} has invalid immutable image: {error}",
                        dependency.module_id, dependency.release.version
                    ),
                )
            })?;
        let dependency_deployment_id = deployment_id(
            &dependency.module_id,
            &dependency.release.version,
            &node.node_id,
        );
        ensure_deployment_available(
            storage,
            &dependency_deployment_id,
            dependency_image.digest(),
            Some(&operation_id),
        )?;
        let dependency_spec = container_spec(
            &dependency_deployment_id,
            &dependency.module_id,
            &dependency.release.version,
            &release.record.checksum,
            &node,
            dependency_image,
            &release.manifest,
            None,
        );
        let install = RuntimeInstallPayload {
            spec: dependency_spec,
            start: true,
            health_gate: HealthGatePolicy::default(),
            offline_oci_artifact: offline_artifact_for_release(
                storage,
                artifact_store,
                &documents,
                &dependency.module_id,
                &dependency.release.version,
            )?,
        };
        let pipeline = release_pipeline_payload(
            &release.manifest,
            &install,
            &node,
            &operation_id,
            &input.migration_policy,
            &input.gateway_node_id,
            &Value::Null,
            &empty_secret_refs,
        )?;
        let (kind, payload, max_attempts) = if let Some(pipeline) = pipeline {
            (
                JobKind::ReleasePipeline,
                serde_json::to_value(pipeline).map_err(|error| {
                    StoreApiError::new(
                        500,
                        "STORE_PIPELINE_INVALID",
                        format!("serialize replacement dependency pipeline: {error}"),
                    )
                })?,
                1,
            )
        } else {
            (
                JobKind::Install,
                serde_json::to_value(install).map_err(|error| {
                    StoreApiError::new(
                        500,
                        "STORE_INSTALL_INVALID",
                        format!("serialize replacement dependency install: {error}"),
                    )
                })?,
                3,
            )
        };
        jobs.push(PlannedJob {
            step_id: install_steps
                .get(&dependency.module_id)
                .expect("missing replacement dependency was indexed")
                .clone(),
            node_id: node.node_id.clone(),
            kind,
            depends_on: dependency
                .release
                .dependencies
                .iter()
                .filter_map(|nested| install_steps.get(&nested.module_id).cloned())
                .collect(),
            condition: PlannedJobCondition::OnSuccess,
            payload,
            max_attempts,
        });
        planned_dependency_deployments.push((
            dependency_deployment_id,
            dependency.release.oci_image.digest().as_str().to_string(),
        ));
    }
    let dependency_steps = jobs
        .iter()
        .map(|job| job.step_id.clone())
        .collect::<Vec<_>>();
    jobs.push(PlannedJob {
        step_id: format!(
            "runtime-{}",
            match action {
                ReplacementAction::Upgrade => "upgrade",
                ReplacementAction::Rollback => "rollback",
            }
        ),
        node_id: node.node_id.clone(),
        kind: action.job_kind(),
        depends_on: dependency_steps,
        condition: PlannedJobCondition::OnSuccess,
        payload: serde_json::to_value(&payload).map_err(|error| {
            StoreApiError::new(
                500,
                "STORE_REPLACEMENT_INVALID",
                format!("serialize replacement payload: {error}"),
            )
        })?,
        max_attempts: 3,
    });
    let plan = PlanOperation {
        operation_id: operation_id.clone(),
        action: action.action_id().to_string(),
        target_type: "Release".to_string(),
        target_id: format!(
            "{}@{}",
            current.instance.service_id, root_release.release.version
        ),
        request: json!({
            "service_id": current.instance.service_id,
            "version": root_release.release.version,
            "image": root_release.release.oci_image,
            "deployment_id": new_deployment_id,
            "endpoint": spec
                .published_endpoint
                .as_ref()
                .map(|endpoint| endpoint.endpoint.as_str()),
            "planned_deployment_ids": std::iter::once(&new_deployment_id)
                .chain(planned_dependency_deployments.iter().map(|(deployment_id, _)| deployment_id))
                .collect::<Vec<_>>(),
            "replaces_deployment_id": current.instance.deployment_id,
            "previous_version": current_proof.version,
            "previous_image": current_proof.image,
            "previous_operation_id": current_proof.operation_id,
            "previous_catalog_id": current_proof.catalog_id,
            "previous_catalog_verified_key_ids": current_proof.verified_key_ids,
            "rollback_proof_operation_id": rollback_proof_operation_id,
            "target_node_id": node.node_id,
            "target_platform": platform,
            "start": true,
            "channel": channel,
            "migration_policy": input.migration_policy.to_ascii_uppercase(),
            "release_checksum": selected.record.checksum,
            "catalog_source_id": resolved.source_id,
            "catalog_id": resolved.catalog_id,
            "catalog_verified_key_ids": resolved.verified_key_ids,
            "catalog_plan": resolved.plan,
            "auto_enqueue": true,
        }),
        jobs,
    };
    let _plan_guard = store_plan_guard()?;
    ensure_no_active_deployment_mutation(storage, current_deployment_id, Some(&operation_id))?;
    ensure_no_active_replacement(storage, current_deployment_id, Some(&operation_id))?;
    ensure_deployment_available(
        storage,
        &new_deployment_id,
        root_release.release.oci_image.digest().as_str(),
        Some(&operation_id),
    )?;
    if let Some(endpoint) = spec.published_endpoint.as_ref() {
        ensure_endpoint_available(
            storage,
            endpoint,
            Some(current_deployment_id),
            Some(&operation_id),
        )?;
    }
    for (dependency_deployment_id, digest) in &planned_dependency_deployments {
        ensure_deployment_available(
            storage,
            dependency_deployment_id,
            digest,
            Some(&operation_id),
        )?;
    }
    let operation = enqueue_plan(storage, plan)?;
    Ok(success(
        202,
        json!({
            "operation_id": operation_id,
            "operation": operation,
            "deployment_id": new_deployment_id,
            "replaces_deployment_id": current_deployment_id,
            "endpoint": spec
                .published_endpoint
                .as_ref()
                .map(|endpoint| endpoint.endpoint.as_str()),
            "release": {
                "service_id": current.instance.service_id,
                "version": root_release.release.version,
                "checksum": selected.record.checksum,
                "image": root_release.release.oci_image,
                "target_platform": platform,
            },
            "imported": imported,
            "lifecycle": action.lifecycle(),
            "installed": false,
        }),
        request_id,
    ))
}

fn release_history(
    storage: &DurableStore,
    service_id: &str,
) -> Result<Vec<ReleaseHistoryProof>, StoreApiError> {
    let mut history = storage
        .operation_store()
        .list()
        .map_err(|error| StoreApiError::new(500, "STORE_OPERATION_ERROR", error.to_string()))?
        .into_iter()
        .filter_map(release_history_proof)
        .filter(|proof| proof.service_id == service_id)
        .collect::<Vec<_>>();
    history.sort_by(|left, right| right.updated_at_ms.cmp(&left.updated_at_ms));
    Ok(history)
}

fn provider_revision_from_operation(
    storage: &DurableStore,
    operation_id: &str,
) -> Result<ReleaseProviderRevision, StoreApiError> {
    let operation = storage
        .operation_store()
        .get(operation_id)
        .map_err(|error| StoreApiError::new(500, "STORE_OPERATION_ERROR", error.to_string()))?
        .ok_or_else(|| {
            StoreApiError::new(
                409,
                "STORE_PROVIDER_REVISION_MISSING",
                format!("proven release Operation {operation_id} no longer exists"),
            )
        })?;
    let service_id = operation
        .request
        .get("service_id")
        .and_then(Value::as_str)
        .unwrap_or_default();
    for planned in operation.planned_jobs.iter().rev() {
        match planned.kind {
            JobKind::ReleasePipeline => {
                let pipeline: ReleasePipelinePayload =
                    serde_json::from_value(planned.payload.clone()).map_err(|error| {
                        StoreApiError::new(
                            500,
                            "STORE_PROVIDER_REVISION_INVALID",
                            format!(
                                "decode provider revision from Operation {operation_id}: {error}"
                            ),
                        )
                    })?;
                if pipeline.install.spec.service_id == service_id {
                    return Ok(ReleaseProviderRevision {
                        revision_id: operation_id.to_string(),
                        auth: pipeline.auth,
                        provisioners: pipeline.provisioners,
                        gateway: pipeline.gateway,
                    });
                }
            }
            JobKind::Upgrade | JobKind::Rollback => {
                let replacement: ReleaseReplacementPayload =
                    serde_json::from_value(planned.payload.clone()).map_err(|error| {
                        StoreApiError::new(
                            500,
                            "STORE_PROVIDER_REVISION_INVALID",
                            format!(
                                "decode replacement provider revision from Operation {operation_id}: {error}"
                            ),
                        )
                    })?;
                if replacement.new_spec.service_id == service_id {
                    if let Some(saga) = replacement.provider_saga {
                        return Ok(saga.desired);
                    }
                    break;
                }
            }
            _ => {}
        }
    }
    Ok(ReleaseProviderRevision {
        revision_id: operation_id.to_string(),
        ..ReleaseProviderRevision::default()
    })
}

fn release_history_proof(operation: DurableOperation) -> Option<ReleaseHistoryProof> {
    if operation.status != DurableOperationStatus::Succeeded
        || !matches!(
            operation.action.as_str(),
            "release.install" | "release.upgrade" | "release.rollback"
        )
        || operation.request.get("start").and_then(Value::as_bool) != Some(true)
    {
        return None;
    }
    let request = operation.request.as_object()?;
    let service_id = request.get("service_id")?.as_str()?.trim();
    let version = semver::Version::parse(request.get("version")?.as_str()?).ok()?;
    let image = request.get("image")?.as_str()?.trim();
    OciImageReference::parse(image).ok()?;
    let deployment_id = request.get("deployment_id")?.as_str()?.trim();
    let catalog_source_id = request.get("catalog_source_id")?.as_str()?.trim();
    let catalog_id = request.get("catalog_id")?.as_str()?.trim();
    let verified_key_ids = request
        .get("catalog_verified_key_ids")?
        .as_array()?
        .iter()
        .map(|value| value.as_str().map(str::to_string))
        .collect::<Option<Vec<_>>>()?;
    let channel = match request.get("channel").and_then(Value::as_str) {
        Some(value) => history_release_channel(value)?,
        None => ReleaseChannel::Stable,
    };
    if service_id.is_empty()
        || deployment_id.is_empty()
        || catalog_source_id.is_empty()
        || catalog_id.is_empty()
        || verified_key_ids.is_empty()
        || verified_key_ids
            .iter()
            .any(|key_id| key_id.trim().is_empty())
    {
        return None;
    }
    Some(ReleaseHistoryProof {
        operation_id: operation.operation_id,
        deployment_id: deployment_id.to_string(),
        service_id: service_id.to_string(),
        version,
        image: image.to_string(),
        channel,
        catalog_source_id: catalog_source_id.to_string(),
        catalog_id: catalog_id.to_string(),
        verified_key_ids,
        updated_at_ms: operation.updated_at_ms,
    })
}

fn history_release_channel(value: &str) -> Option<ReleaseChannel> {
    match value.trim().to_ascii_lowercase().as_str() {
        "stable" => Some(ReleaseChannel::Stable),
        "beta" => Some(ReleaseChannel::Beta),
        "nightly" => Some(ReleaseChannel::Nightly),
        _ => None,
    }
}

fn ensure_no_active_replacement(
    storage: &DurableStore,
    deployment_id: &str,
    expected_operation_id: Option<&str>,
) -> Result<(), StoreApiError> {
    let active = storage
        .operation_store()
        .list()
        .map_err(|error| StoreApiError::new(500, "STORE_OPERATION_ERROR", error.to_string()))?
        .into_iter()
        .find(|operation| {
            !operation.status.is_terminal()
                && Some(operation.operation_id.as_str()) != expected_operation_id
                && (operation
                    .request
                    .get("replaces_deployment_id")
                    .and_then(Value::as_str)
                    == Some(deployment_id)
                    || operation.planned_jobs.iter().any(|job| {
                        job.payload.get("old_deployment_id").and_then(Value::as_str)
                            == Some(deployment_id)
                    }))
        });
    if let Some(operation) = active {
        Err(StoreApiError::new(
            409,
            "STORE_REPLACEMENT_IN_PROGRESS",
            format!(
                "deployment {deployment_id} already has active Operation {}",
                operation.operation_id
            ),
        ))
    } else {
        Ok(())
    }
}

fn ensure_no_active_deployment_mutation(
    storage: &DurableStore,
    deployment_id: &str,
    expected_operation_id: Option<&str>,
) -> Result<(), StoreApiError> {
    let active = storage
        .operation_store()
        .list()
        .map_err(|error| StoreApiError::new(500, "STORE_OPERATION_ERROR", error.to_string()))?
        .into_iter()
        .find(|operation| {
            !operation.status.is_terminal()
                && Some(operation.operation_id.as_str()) != expected_operation_id
                && operation
                    .request
                    .get("deployment_id")
                    .and_then(Value::as_str)
                    == Some(deployment_id)
        });
    if let Some(operation) = active {
        Err(StoreApiError::new(
            409,
            "STORE_DEPLOYMENT_MUTATION_IN_PROGRESS",
            format!(
                "deployment {deployment_id} already has active Operation {}",
                operation.operation_id
            ),
        ))
    } else {
        Ok(())
    }
}

fn artifact_matches(expected: &str, observed: &str) -> bool {
    expected == observed
        || artifact_digest(expected)
            .zip(artifact_digest(observed))
            .is_some_and(|(expected, observed)| expected == observed)
}

fn artifact_digest(value: &str) -> Option<&str> {
    value
        .strip_prefix("sha256:")
        .or_else(|| value.split_once("@sha256:").map(|(_, digest)| digest))
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ReleaseDeleteRequest {
    service_id: String,
    version: String,
}

fn delete_release_metadata(
    console: &mut OrchestratorActionConsole,
    storage: &DurableStore,
    request: &ApiRequest,
    request_id: &str,
) -> Result<ApiResponse, StoreApiError> {
    let input: ReleaseDeleteRequest = parse_body(request)?;
    let service_id = required_text(&input.service_id, "service_id")?;
    let version = required_text(&input.version, "version")?;
    let selected = select_release(console, service_id, Some(version))?;

    // RuntimeInstance does not duplicate mutable version labels. The trusted
    // successful Store Operation is the proof tying a deployment to its
    // immutable Catalog version. If that proof is missing, deletion fails
    // closed rather than orphaning metadata that may still be in use.
    let history = release_history(storage, service_id)?;
    for deployment in storage
        .runtime_instances(None)
        .map_err(storage_error)?
        .into_iter()
        .filter(|deployment| deployment.instance.service_id == service_id)
    {
        let proof = history
            .iter()
            .find(|proof| proof.deployment_id == deployment.instance.deployment_id)
            .ok_or_else(|| {
                StoreApiError::new(
                    409,
                    "STORE_RELEASE_REFERENCE_UNKNOWN",
                    format!(
                        "deployment {} may reference {service_id}@{} but has no successful trusted Store Operation proving its version",
                        deployment.instance.deployment_id, selected.version
                    ),
                )
            })?;
        if proof.version == selected.version {
            return Err(StoreApiError::new(
                409,
                "STORE_RELEASE_IN_USE",
                format!(
                    "release {service_id}@{} is referenced by deployment {}; uninstall or upgrade that Deployment first",
                    selected.version, deployment.instance.deployment_id
                ),
            ));
        }
    }

    let target = format!("{service_id}@{}", selected.version);
    let operation_id = operation_id("release-delete", &target, request)?;
    let result = console
        .dispatch(ActionRequest::new(
            operation_id,
            "release.delete",
            BTreeMap::from([
                ("service_id".to_string(), service_id.to_string()),
                ("version".to_string(), selected.version.to_string()),
                ("confirm".to_string(), "true".to_string()),
            ]),
        ))
        .map_err(core_error)?;
    if !result.status.eq_ignore_ascii_case("SUCCEEDED") {
        return Err(StoreApiError::new(
            409,
            "STORE_RELEASE_DELETE_REJECTED",
            format!("release {target} deletion ended in {}", result.status),
        ));
    }
    Ok(success(
        200,
        json!({
            "service_id": service_id,
            "version": selected.version,
            "deleted": true,
            "action_result": result,
        }),
        request_id,
    ))
}

struct SelectedRelease {
    record: ServiceRelease,
    manifest: ServiceReleaseManifest,
    version: semver::Version,
}

fn select_release(
    console: &OrchestratorActionConsole,
    service_id: &str,
    requested_version: Option<&str>,
) -> Result<SelectedRelease, StoreApiError> {
    let requested = requested_version
        .map(semver::Version::parse)
        .transpose()
        .map_err(|error| {
            StoreApiError::new(
                422,
                "STORE_VERSION_INVALID",
                format!("requested version is not semver: {error}"),
            )
        })?;
    let mut candidates = Vec::new();
    for record in console.service_releases().map_err(core_error)? {
        if record.service_name != service_id {
            continue;
        }
        let version = semver::Version::parse(record.version.trim()).map_err(|error| {
            StoreApiError::new(
                422,
                "STORE_RELEASE_INVALID",
                format!(
                    "registered release {service_id}@{} is not semver: {error}",
                    record.version
                ),
            )
        })?;
        if requested
            .as_ref()
            .is_some_and(|requested| requested != &version)
            || (requested.is_none() && !version.pre.is_empty())
        {
            continue;
        }
        let manifest: ServiceReleaseManifest = serde_json::from_value(record.manifest.clone())
            .map_err(|error| {
                StoreApiError::new(
                    422,
                    "STORE_RELEASE_INVALID",
                    format!(
                        "registered release {service_id}@{version} has an invalid manifest: {error}"
                    ),
                )
            })?;
        validate_service_release(&manifest).map_err(core_error)?;
        if manifest.service_name != record.service_name || manifest.version != record.version {
            return Err(StoreApiError::new(
                409,
                "STORE_RELEASE_IDENTITY_MISMATCH",
                format!(
                    "release record {}@{} does not match its manifest {}@{}",
                    record.service_name, record.version, manifest.service_name, manifest.version
                ),
            ));
        }
        candidates.push(SelectedRelease {
            record,
            manifest,
            version,
        });
    }
    candidates.sort_by(|left, right| left.version.cmp(&right.version));
    candidates.pop().ok_or_else(|| {
        StoreApiError::new(
            404,
            "STORE_RELEASE_NOT_FOUND",
            requested.map_or_else(
                || format!("service {service_id} has no stable registered release"),
                |version| format!("release {service_id}@{version} was not found"),
            ),
        )
    })
}

fn ensure_release_checksum(record: &ServiceRelease) -> Result<(), StoreApiError> {
    let checksum = record.checksum.trim();
    if is_sha256(checksum) {
        Ok(())
    } else {
        Err(StoreApiError::new(
            422,
            "STORE_RELEASE_CHECKSUM_REQUIRED",
            format!(
                "release {}@{} must have a verified sha256:<64 lowercase hex> metadata checksum",
                record.service_name, record.version
            ),
        ))
    }
}

#[allow(clippy::too_many_arguments)]
fn release_pipeline_payload(
    release: &ServiceReleaseManifest,
    install: &RuntimeInstallPayload,
    node: &NodeRecord,
    operation_id: &str,
    migration_policy: &str,
    gateway_node_id: &str,
    requested_config: &Value,
    requested_secret_refs: &BTreeMap<String, String>,
) -> Result<Option<ReleasePipelinePayload>, StoreApiError> {
    if !release.runtime.kind.eq_ignore_ascii_case("image") {
        return Err(StoreApiError::new(
            422,
            "STORE_RUNTIME_UNAVAILABLE",
            format!(
                "runtime kind {} is not enabled; v1 production Store installs use Docker image releases",
                release.runtime.kind
            ),
        ));
    }
    let migration_policy = match migration_policy.trim().to_ascii_uppercase().as_str() {
        "APPLY" => MigrationPolicyV2::Apply,
        "DRY_RUN" | "DRY-RUN" => MigrationPolicyV2::DryRun,
        "SKIP" => MigrationPolicyV2::Skip,
        _ => {
            return Err(StoreApiError::new(
                422,
                "STORE_MIGRATION_POLICY_INVALID",
                "migration_policy must be APPLY, DRY_RUN, or SKIP",
            ));
        }
    };
    let materialization =
        build_runtime_materialization(release, node, requested_config, requested_secret_refs)?;

    let mut provisioners = Vec::new();
    if !release.redis.is_empty() {
        let connection_id = provider_identifier(node, "redis", "connection_id")?;
        provisioners.push(TypedProvisionerStep::Redis {
            service_name: release.service_name.clone(),
            resources: release
                .redis
                .iter()
                .map(|resource| RedisNamespaceSpec {
                    name: resource.name.clone(),
                    kind: resource.kind.clone(),
                    connection_id: connection_id.clone(),
                    namespace: format!(
                        "ojos:{}:{}",
                        provider_token(&release.service_name),
                        provider_token(&resource.name)
                    ),
                    consumer_group: format!(
                        "ojos-{}-{}",
                        provider_token(&release.service_name),
                        provider_token(&resource.name)
                    ),
                })
                .collect(),
        });
    }
    if !release.storage.is_empty() {
        let (backend, connection_id) = storage_provider_selection(node)?;
        provisioners.push(TypedProvisionerStep::Storage {
            service_name: release.service_name.clone(),
            resources: release
                .storage
                .iter()
                .map(|resource| StorageResourceSpec {
                    object_type: resource.object_type.clone(),
                    bucket: resource.bucket.clone(),
                    prefix: if resource.path_prefix.trim().is_empty() {
                        format!(
                            "{}/{}",
                            provider_token(&release.service_name),
                            provider_token(&resource.object_type)
                        )
                    } else {
                        resource.path_prefix.trim_matches('/').to_string()
                    },
                    backend: backend.clone(),
                    connection_id: connection_id.clone(),
                })
                .collect(),
        });
    }
    if !release.apis.is_empty() || !release.required_apis.is_empty() {
        let registry_id = provider_identifier(node, "api_registry", "registry_id")?;
        provisioners.push(TypedProvisionerStep::ApiRegistry {
            service_name: release.service_name.clone(),
            registry_id,
            apis: release
                .apis
                .iter()
                .map(|api| ApiSurfaceSpec {
                    api_id: api.api_id.clone(),
                    protocol: api.protocol.clone(),
                    path_prefix: api.path_prefix.clone(),
                    methods: api.methods.clone(),
                    visibility: api.visibility.clone(),
                    auth_mode: api.auth_mode.clone(),
                    permission: api.permission.clone(),
                    version: api.version.clone(),
                })
                .collect(),
            required_apis: release.required_apis.clone(),
        });
    }
    if release.frontend.enabled {
        let asset_store_id = provider_identifier(node, "frontend", "asset_store_id")?;
        let metadata_sha256 = install
            .spec
            .labels
            .get("ojos.release_checksum")
            .cloned()
            .unwrap_or_default();
        if release.frontend.route_prefix.trim().is_empty()
            || release.frontend.remote_entry.trim().is_empty()
            || release.source.url.trim().is_empty()
            || !is_sha256(&metadata_sha256)
        {
            return Err(StoreApiError::new(
                422,
                "STORE_FRONTEND_DECLARATION_INVALID",
                "frontend release requires route_prefix, remote_entry, a signed source URL, and verified metadata checksum",
            ));
        }
        provisioners.push(TypedProvisionerStep::Frontend {
            service_name: release.service_name.clone(),
            asset_store_id,
            version: release.version.clone(),
            route_prefix: release.frontend.route_prefix.clone(),
            remote_entry: release.frontend.remote_entry.clone(),
            metadata_source_url: release.source.url.clone(),
            metadata_sha256,
        });
    }

    let auth = if !release.permissions.is_empty()
        || !release.service_identity.allowed_apis.is_empty()
        || !release.service_identity.service_name.is_empty()
    {
        require_node_provider(node, "auth")?;
        Some(AuthPipelineStep {
            service_name: release.service_name.clone(),
            permissions: release.permissions.clone(),
            service_identity: (!release.service_identity.service_name.trim().is_empty()
                || !release.service_identity.allowed_apis.is_empty())
            .then(|| AuthServiceIdentitySpec {
                service_name: release.service_name.clone(),
                allowed_apis: release.service_identity.allowed_apis.clone(),
                // Cross-release API grants are resolved by the Auth service;
                // Store does not invent permissions from an un-applied topology.
                grants: vec![],
            }),
        })
    } else {
        None
    };

    let mut migrations = Vec::with_capacity(release.migrations.len());
    if migration_policy != MigrationPolicyV2::Skip && !release.migrations.is_empty() {
        require_node_provider(node, "migration")?;
    }
    for migration in release
        .migrations
        .iter()
        .filter(|_| migration_policy != MigrationPolicyV2::Skip)
    {
        if migration.destructive {
            return Err(StoreApiError::new(
                422,
                "STORE_DESTRUCTIVE_MIGRATION_REJECTED",
                format!(
                    "migration {} is destructive; v1 Store requires a separately approved migration workflow",
                    migration.version
                ),
            ));
        }
        if !is_sha256(migration.checksum.trim()) {
            return Err(StoreApiError::new(
                422,
                "STORE_MIGRATION_CHECKSUM_REQUIRED",
                format!(
                    "migration {} requires sha256:<64 lowercase hex> checksum",
                    migration.version
                ),
            ));
        }
        let oci = migration.oci.as_ref().ok_or_else(|| {
            StoreApiError::new(
                422,
                "STORE_MIGRATION_OCI_REQUIRED",
                format!(
                    "migration {} must declare a signed one-shot OCI runner",
                    migration.version
                ),
            )
        })?;
        if oci.command.is_empty() || oci.timeout_ms == 0 || oci.timeout_ms > 60 * 60_000 {
            return Err(StoreApiError::new(
                422,
                "STORE_MIGRATION_OCI_INVALID",
                format!(
                    "migration {} OCI runner requires command and timeout_ms between 1 and 3600000",
                    migration.version
                ),
            ));
        }
        let image = OciImageReference::parse(&oci.image).map_err(|error| {
            StoreApiError::new(
                422,
                "STORE_MIGRATION_IMMUTABLE_IMAGE_REQUIRED",
                format!(
                    "migration {} OCI image is invalid: {error}",
                    migration.version
                ),
            )
        })?;
        migrations.push(OciMigrationStep {
            service_name: release.service_name.clone(),
            version: migration.version.clone(),
            checksum: migration.checksum.clone(),
            image,
            command: oci.command.clone(),
            environment: oci
                .env
                .iter()
                .map(|(key, value)| format!("{key}={value}"))
                .chain(std::iter::once(format!(
                    "ORCHESTRATOR_MIGRATION_DRY_RUN={}",
                    migration_policy == MigrationPolicyV2::DryRun
                )))
                .collect(),
            timeout_ms: oci.timeout_ms,
            dry_run: migration_policy == MigrationPolicyV2::DryRun,
        });
    }

    let gateway = if release.routes.is_empty() {
        if !gateway_node_id.trim().is_empty() {
            return Err(StoreApiError::new(
                422,
                "STORE_GATEWAY_NODE_UNUSED",
                "gateway_node_id cannot be set when the release declares no routes",
            ));
        }
        None
    } else {
        require_node_provider(node, "gateway")?;
        let gateway_node_id = required_text(gateway_node_id, "gateway_node_id")?;
        if node.host_ip.trim().is_empty() {
            return Err(StoreApiError::new(
                422,
                "STORE_NODE_HOST_REQUIRED",
                "target Node must advertise host_ip before Gateway routes can be published",
            ));
        }
        if !matches!(release.backend.protocol.as_str(), "http" | "https") {
            return Err(StoreApiError::new(
                422,
                "STORE_GATEWAY_PROTOCOL_UNSUPPORTED",
                "v1 Gateway route publication supports HTTP/HTTPS backends",
            ));
        }
        let upstream_base = format!(
            "{}://{}:{}",
            release.backend.protocol, node.host_ip, release.backend.port
        );
        let mut routes = Vec::with_capacity(release.routes.len());
        for (index, route) in release.routes.iter().enumerate() {
            if !matches!(route.target_type.as_str(), "endpoint" | "endpoint-group") {
                return Err(StoreApiError::new(
                    422,
                    "STORE_GATEWAY_ROUTE_UNSUPPORTED",
                    format!(
                        "route {} uses target_type={}; v1 pipeline only publishes endpoint routes",
                        route.path, route.target_type
                    ),
                ));
            }
            let methods = if route.method.trim().is_empty() {
                vec![
                    "GET".to_string(),
                    "POST".to_string(),
                    "PUT".to_string(),
                    "PATCH".to_string(),
                    "DELETE".to_string(),
                    "OPTIONS".to_string(),
                ]
            } else {
                vec![route.method.trim().to_ascii_uppercase()]
            };
            let permission = route.permission.trim();
            routes.push(GatewayRouteSpec {
                route_id: format!("{}:{}", release.service_name, index + 1),
                path_prefix: route.path.clone(),
                upstream_base: upstream_base.clone(),
                methods,
                auth_mode: if permission.is_empty() || permission == "public" {
                    "public".to_string()
                } else {
                    "user".to_string()
                },
                required_permission: if permission == "public" {
                    String::new()
                } else {
                    permission.to_string()
                },
            });
        }
        Some(GatewayPipelineStep {
            operation_id: operation_id.to_string(),
            service_name: release.service_name.clone(),
            node_id: gateway_node_id.to_string(),
            routes,
        })
    };

    if materialization.is_none()
        && auth.is_none()
        && provisioners.is_empty()
        && migrations.is_empty()
        && gateway.is_none()
    {
        Ok(None)
    } else {
        Ok(Some(ReleasePipelinePayload {
            install: install.clone(),
            materialization,
            auth,
            provisioners,
            migrations,
            gateway,
        }))
    }
}

fn build_runtime_materialization(
    release: &ServiceReleaseManifest,
    node: &NodeRecord,
    requested_config: &Value,
    requested_secret_refs: &BTreeMap<String, String>,
) -> Result<Option<RuntimeMaterializationStep>, StoreApiError> {
    let (config, schema_secrets) =
        validate_release_config(&release.config_schema, requested_config)?;
    let mut required_secrets = release.secrets.iter().cloned().collect::<BTreeSet<_>>();
    required_secrets.extend(schema_secrets);
    let supplied = requested_secret_refs
        .keys()
        .cloned()
        .collect::<BTreeSet<_>>();
    let missing = required_secrets
        .difference(&supplied)
        .cloned()
        .collect::<Vec<_>>();
    let unknown = supplied
        .difference(&required_secrets)
        .cloned()
        .collect::<Vec<_>>();
    if !missing.is_empty() || !unknown.is_empty() {
        return Err(StoreApiError::new(
            422,
            "STORE_SECRET_REFS_INVALID",
            format!(
                "secret_refs must exactly match the signed release declaration (missing: {}; unknown: {})",
                missing.join(", "),
                unknown.join(", ")
            ),
        ));
    }
    for (name, reference) in requested_secret_refs {
        if reference.trim().is_empty() {
            return Err(StoreApiError::new(
                422,
                "STORE_SECRET_REF_INVALID",
                format!("secret reference {name} must not be empty"),
            ));
        }
    }

    let mut environment_templates = release.runtime.env.clone();
    for key in config.keys() {
        environment_templates
            .entry(key.clone())
            .or_insert_with(|| format!("${{config.{key}}}"));
    }
    for key in &required_secrets {
        let environment_key = key
            .chars()
            .map(|character| {
                if character.is_ascii_alphanumeric() {
                    character.to_ascii_uppercase()
                } else {
                    '_'
                }
            })
            .collect::<String>();
        environment_templates
            .entry(environment_key)
            .or_insert_with(|| format!("${{secret.{key}}}"));
    }
    let needs_materialization = !config.is_empty()
        || !requested_secret_refs.is_empty()
        || release
            .runtime
            .env
            .values()
            .any(|value| value.contains("${"));
    if !needs_materialization {
        return Ok(None);
    }
    require_node_provider(node, "materialization")?;
    if !requested_secret_refs.is_empty() {
        if let Some(Value::Object(configuration)) = node_provider_label(node, "materialization") {
            let secret_provider = configuration
                .get("secret_provider")
                .and_then(Value::as_str)
                .map(str::trim)
                .unwrap_or_default();
            if secret_provider != "file" {
                return Err(StoreApiError::new(
                    422,
                    "STORE_PROVIDER_CONFIGURATION_INVALID",
                    format!(
                        "target Node {} providers.materialization.secret_provider must be file",
                        node.node_id
                    ),
                ));
            }
        }
        for (name, reference) in requested_secret_refs {
            if !reference
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
            {
                return Err(StoreApiError::new(
                    422,
                    "STORE_SECRET_REF_INVALID",
                    format!(
                        "secret reference {name} must name a file in the configured Node secret directory"
                    ),
                ));
            }
        }
    }
    Ok(Some(RuntimeMaterializationStep {
        config,
        secret_refs: requested_secret_refs.clone(),
        environment_templates,
    }))
}

fn validate_release_config(
    schema: &Value,
    requested: &Value,
) -> Result<(BTreeMap<String, String>, BTreeSet<String>), StoreApiError> {
    let requested = match requested {
        Value::Null => serde_json::Map::new(),
        Value::Object(values) => values.clone(),
        _ => {
            return Err(StoreApiError::new(
                422,
                "STORE_CONFIG_INVALID",
                "config must be a JSON object",
            ));
        }
    };
    let Some(schema) = schema.as_object() else {
        if schema.is_null() {
            if requested.is_empty() {
                return Ok((BTreeMap::new(), BTreeSet::new()));
            }
            return Err(StoreApiError::new(
                422,
                "STORE_CONFIG_UNKNOWN",
                "release declares no configurable fields",
            ));
        }
        return Err(StoreApiError::new(
            422,
            "STORE_CONFIG_SCHEMA_INVALID",
            "signed config_schema must be an object",
        ));
    };
    let (properties, required, allow_extra) = if schema.contains_key("properties") {
        let properties = schema
            .get("properties")
            .and_then(Value::as_object)
            .ok_or_else(|| {
                StoreApiError::new(
                    422,
                    "STORE_CONFIG_SCHEMA_INVALID",
                    "config_schema.properties must be an object",
                )
            })?;
        let required = schema
            .get("required")
            .and_then(Value::as_array)
            .map(|values| {
                values
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect::<BTreeSet<_>>()
            })
            .unwrap_or_default();
        let allow_extra = schema
            .get("additionalProperties")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        (properties, required, allow_extra)
    } else {
        (schema, BTreeSet::new(), false)
    };
    let unknown = requested
        .keys()
        .filter(|key| !properties.contains_key(*key))
        .cloned()
        .collect::<Vec<_>>();
    if !allow_extra && !unknown.is_empty() {
        return Err(StoreApiError::new(
            422,
            "STORE_CONFIG_UNKNOWN",
            format!(
                "config contains undeclared field(s): {}",
                unknown.join(", ")
            ),
        ));
    }
    let mut output = BTreeMap::new();
    let mut secrets = BTreeSet::new();
    for (name, declaration) in properties {
        let declaration = declaration.as_object().ok_or_else(|| {
            StoreApiError::new(
                422,
                "STORE_CONFIG_SCHEMA_INVALID",
                format!("config declaration {name} must be an object"),
            )
        })?;
        let kind = declaration
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or("string");
        if kind == "secret" {
            secrets.insert(name.clone());
            if requested.contains_key(name) {
                return Err(StoreApiError::new(
                    422,
                    "STORE_SECRET_VALUE_FORBIDDEN",
                    format!("config field {name} is secret; submit only secret_refs.{name}"),
                ));
            }
            continue;
        }
        let value = requested
            .get(name)
            .cloned()
            .or_else(|| declaration.get("default").cloned());
        let required =
            required.contains(name) || declaration.get("required") == Some(&Value::Bool(true));
        let Some(value) = value else {
            if required {
                return Err(StoreApiError::new(
                    422,
                    "STORE_CONFIG_REQUIRED",
                    format!("config field {name} is required"),
                ));
            }
            continue;
        };
        validate_config_value(name, kind, declaration, &value)?;
        output.insert(name.clone(), scalar_config_value(name, &value)?);
    }
    if allow_extra {
        for (name, value) in requested {
            if !properties.contains_key(&name) {
                output.insert(name.clone(), scalar_config_value(&name, &value)?);
            }
        }
    }
    Ok((output, secrets))
}

fn validate_config_value(
    name: &str,
    kind: &str,
    declaration: &serde_json::Map<String, Value>,
    value: &Value,
) -> Result<(), StoreApiError> {
    let valid = match kind {
        "string" => value.is_string(),
        "boolean" | "bool" => value.is_boolean(),
        "integer" => value.as_i64().is_some() || value.as_u64().is_some(),
        "number" => value.is_number(),
        "enum" => declaration
            .get("values")
            .or_else(|| declaration.get("enum"))
            .and_then(Value::as_array)
            .is_some_and(|values| values.contains(value)),
        other => {
            return Err(StoreApiError::new(
                422,
                "STORE_CONFIG_SCHEMA_INVALID",
                format!("config field {name} has unsupported type {other}"),
            ));
        }
    };
    if valid {
        Ok(())
    } else {
        Err(StoreApiError::new(
            422,
            "STORE_CONFIG_TYPE_INVALID",
            format!("config field {name} does not satisfy type {kind}"),
        ))
    }
}

fn scalar_config_value(name: &str, value: &Value) -> Result<String, StoreApiError> {
    match value {
        Value::String(value) => Ok(value.clone()),
        Value::Bool(value) => Ok(value.to_string()),
        Value::Number(value) => Ok(value.to_string()),
        _ => Err(StoreApiError::new(
            422,
            "STORE_CONFIG_TYPE_INVALID",
            format!("config field {name} must be a scalar"),
        )),
    }
}

fn provider_token(value: &str) -> String {
    value
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '-' | '_') {
                character.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect()
}

fn require_node_provider(node: &NodeRecord, provider: &str) -> Result<(), StoreApiError> {
    let advertised = node_provider_label(node, provider).is_some_and(|value| match value {
        Value::Bool(value) => *value,
        Value::String(value) => matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "1" | "true" | "enabled" | "ready"
        ),
        Value::Object(configuration) => configuration
            .get("enabled")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        _ => false,
    });
    if advertised {
        Ok(())
    } else {
        Err(StoreApiError::new(
            422,
            "STORE_PROVIDER_REQUIRED",
            format!(
                "target Node {} does not advertise providers.{provider}=true",
                node.node_id
            ),
        ))
    }
}

fn node_provider_label<'a>(node: &'a NodeRecord, provider: &str) -> Option<&'a Value> {
    node.labels
        .get("providers")
        .and_then(|providers| providers.get(provider))
        .or_else(|| node.labels.get(format!("provider.{provider}")))
}

fn provider_identifier(
    node: &NodeRecord,
    provider: &str,
    field: &str,
) -> Result<String, StoreApiError> {
    require_node_provider(node, provider)?;
    match node_provider_label(node, provider) {
        Some(Value::Object(configuration)) => configuration
            .get(field)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
            .ok_or_else(|| {
                StoreApiError::new(
                    422,
                    "STORE_PROVIDER_CONFIGURATION_INVALID",
                    format!(
                        "target Node {} providers.{provider}.{field} must be a non-empty identifier",
                        node.node_id
                    ),
                )
            }),
        Some(_) => Err(StoreApiError::new(
            422,
            "STORE_PROVIDER_CONFIGURATION_INVALID",
            format!(
                "target Node {} providers.{provider} must be an object with enabled=true and {field}",
                node.node_id
            ),
        )),
        None => unreachable!("require_node_provider accepted the provider"),
    }
}

fn storage_provider_selection(node: &NodeRecord) -> Result<(String, String), StoreApiError> {
    require_node_provider(node, "storage")?;
    let Some(Value::Object(configuration)) = node_provider_label(node, "storage") else {
        return Err(StoreApiError::new(
            422,
            "STORE_PROVIDER_CONFIGURATION_INVALID",
            format!(
                "target Node {} providers.storage must be an object with enabled, backend, and connection_id",
                node.node_id
            ),
        ));
    };
    let backend = configuration
        .get("backend")
        .and_then(Value::as_str)
        .map(str::trim)
        .unwrap_or_default();
    if !matches!(backend, "node_directory" | "s3") {
        return Err(StoreApiError::new(
            422,
            "STORE_PROVIDER_CONFIGURATION_INVALID",
            format!(
                "target Node {} providers.storage.backend must be node_directory or s3",
                node.node_id
            ),
        ));
    }
    let connection_id = provider_identifier(node, "storage", "connection_id")?;
    Ok((backend.to_string(), connection_id))
}

fn missing_resolved_dependencies<'a>(
    storage: &DurableStore,
    resolved: &'a ResolvedCatalogPlan,
    root_service_id: &str,
) -> Result<Vec<&'a orchestrator_manager::catalog_v2::ResolvedReleaseV2>, StoreApiError> {
    let deployments = storage.runtime_instances(None).map_err(storage_error)?;
    Ok(resolved
        .plan
        .releases
        .iter()
        .filter(|selection| selection.module_id != root_service_id)
        .filter(|selection| {
            let expected_digest = selection.release.oci_image.digest().as_str();
            !deployments.iter().any(|deployment| {
                deployment.instance.service_id == selection.module_id
                    && deployment.instance.observed_state == RuntimeObservedState::Running
                    && deployment.instance.health.eq_ignore_ascii_case("HEALTHY")
                    && (deployment.instance.artifact_digest == expected_digest
                        || deployment
                            .instance
                            .artifact_digest
                            .ends_with(expected_digest))
            })
        })
        .collect())
}

fn offline_artifact_for_release(
    storage: &DurableStore,
    artifact_store: Option<&ArtifactStore>,
    documents: &[VerifiedReleaseDocument],
    service_id: &str,
    version: &semver::Version,
) -> Result<Option<ArtifactReference>, StoreApiError> {
    let layout = documents
        .iter()
        .find(|document| {
            document.selection.module_id == service_id
                && &document.selection.release.version == version
        })
        .and_then(|document| document.offline_oci_layout.as_deref());
    let Some(layout) = layout else {
        return Ok(None);
    };
    let store = artifact_store.ok_or_else(|| {
        StoreApiError::new(
            503,
            "STORE_ARTIFACT_STORAGE_UNAVAILABLE",
            "offline OCI install requires configured durable artifact storage",
        )
    })?;
    let protected = storage
        .job_store()
        .list()
        .map_err(|error| {
            StoreApiError::new(
                500,
                "STORE_ARTIFACT_RETENTION_FAILED",
                format!("list durable Jobs before artifact retention: {error}"),
            )
        })?
        .into_iter()
        .filter(|job| !job.status.is_terminal())
        .filter_map(|job| {
            ["/offline_oci_artifact", "/install/offline_oci_artifact"]
                .iter()
                .filter_map(|pointer| job.payload.pointer(pointer))
                .find_map(|value| serde_json::from_value::<ArtifactReference>(value.clone()).ok())
        })
        .map(|reference| reference.artifact_id)
        .collect();
    let policy = ArtifactRetentionPolicy::from_env().map_err(|error| {
        StoreApiError::new(500, "STORE_ARTIFACT_RETENTION_FAILED", error.to_string())
    })?;
    store
        .collect_garbage(&protected, policy, SystemTime::now())
        .map_err(|error| {
            StoreApiError::new(500, "STORE_ARTIFACT_RETENTION_FAILED", error.to_string())
        })?;
    build_offline_oci_artifact(store, layout).map(Some)
}

fn build_offline_oci_artifact(
    store: &ArtifactStore,
    root: &Path,
) -> Result<ArtifactReference, StoreApiError> {
    let source_bytes = checked_layout_size(root)?;
    if source_bytes > MAX_ARTIFACT_BYTES {
        return Err(StoreApiError::new(
            422,
            "CATALOG_OFFLINE_OCI_TOO_LARGE",
            format!(
                "offline OCI layout {} is {} bytes; v1 Agent transfer limit is {} bytes",
                root.display(),
                source_bytes,
                MAX_ARTIFACT_BYTES
            ),
        ));
    }
    store.create_oci_archive(root).map_err(|error| {
        StoreApiError::new(
            422,
            "CATALOG_OFFLINE_OCI_ARCHIVE_FAILED",
            format!("persist offline OCI archive {}: {error}", root.display()),
        )
    })
}

fn checked_layout_size(path: &Path) -> Result<u64, StoreApiError> {
    let metadata = fs::symlink_metadata(path).map_err(|error| {
        StoreApiError::new(
            422,
            "CATALOG_OFFLINE_OCI_ARCHIVE_FAILED",
            format!("inspect {}: {error}", path.display()),
        )
    })?;
    if metadata.file_type().is_symlink() {
        return Err(StoreApiError::new(
            422,
            "CATALOG_OFFLINE_OCI_ARCHIVE_FAILED",
            format!("offline OCI layout contains symlink {}", path.display()),
        ));
    }
    if metadata.is_file() {
        return Ok(metadata.len());
    }
    if !metadata.is_dir() {
        return Err(StoreApiError::new(
            422,
            "CATALOG_OFFLINE_OCI_ARCHIVE_FAILED",
            format!(
                "offline OCI entry is neither file nor directory: {}",
                path.display()
            ),
        ));
    }
    let mut total = 0_u64;
    for entry in fs::read_dir(path).map_err(|error| {
        StoreApiError::new(
            422,
            "CATALOG_OFFLINE_OCI_ARCHIVE_FAILED",
            format!("read {}: {error}", path.display()),
        )
    })? {
        let entry = entry.map_err(|error| {
            StoreApiError::new(
                422,
                "CATALOG_OFFLINE_OCI_ARCHIVE_FAILED",
                format!("read {} entry: {error}", path.display()),
            )
        })?;
        total = total
            .checked_add(checked_layout_size(&entry.path())?)
            .ok_or_else(|| {
                StoreApiError::new(
                    422,
                    "CATALOG_OFFLINE_OCI_TOO_LARGE",
                    "offline OCI layout size overflow",
                )
            })?;
        if total > MAX_ARTIFACT_BYTES {
            break;
        }
    }
    Ok(total)
}

fn parse_release_channel(value: &str) -> Result<ReleaseChannel, StoreApiError> {
    match value.trim().to_ascii_lowercase().as_str() {
        "stable" | "" => Ok(ReleaseChannel::Stable),
        "beta" => Ok(ReleaseChannel::Beta),
        "nightly" => Ok(ReleaseChannel::Nightly),
        _ => Err(StoreApiError::new(
            422,
            "CATALOG_CHANNEL_INVALID",
            "channel must be stable, beta, or nightly",
        )),
    }
}

fn ensure_ready_docker_node(node: &NodeRecord) -> Result<(), StoreApiError> {
    if !node.status.eq_ignore_ascii_case("READY") {
        return Err(StoreApiError::new(
            409,
            "STORE_TARGET_NODE_NOT_READY",
            format!(
                "target Node {} is {}; Store runtime mutations require READY",
                node.node_id, node.status
            ),
        ));
    }
    if !node
        .labels
        .get("runtime")
        .and_then(Value::as_str)
        .is_some_and(|runtime| runtime.eq_ignore_ascii_case("docker"))
    {
        return Err(StoreApiError::new(
            422,
            "STORE_DOCKER_CAPABILITY_REQUIRED",
            format!(
                "target Node {} does not advertise runtime=docker",
                node.node_id
            ),
        ));
    }
    Ok(())
}

fn target_platform(node: &NodeRecord) -> Result<TargetPlatform, StoreApiError> {
    let nested = node.labels.get("platform");
    let os = node.labels.get("os").and_then(Value::as_str).or_else(|| {
        nested
            .and_then(|value| value.get("os"))
            .and_then(Value::as_str)
    });
    let arch = node.labels.get("arch").and_then(Value::as_str).or_else(|| {
        nested
            .and_then(|value| value.get("arch"))
            .and_then(Value::as_str)
    });
    let variant = node
        .labels
        .get("variant")
        .and_then(Value::as_str)
        .or_else(|| {
            nested
                .and_then(|value| value.get("variant"))
                .and_then(Value::as_str)
        });
    let embedded = node
        .labels
        .get("embedded")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let (os, arch) = match (os, arch) {
        (Some(os), Some(arch)) => (os.to_string(), arch.to_string()),
        (None, None) if embedded => (
            std::env::consts::OS.to_string(),
            std::env::consts::ARCH.to_string(),
        ),
        _ => {
            return Err(StoreApiError::new(
                422,
                "STORE_TARGET_PLATFORM_REQUIRED",
                format!(
                    "target Node {} must advertise both labels.os and labels.arch",
                    node.node_id
                ),
            ));
        }
    };
    if !valid_platform_token(&os)
        || !valid_platform_token(&arch)
        || variant.is_some_and(|value| !valid_platform_token(value))
    {
        return Err(StoreApiError::new(
            422,
            "STORE_TARGET_PLATFORM_INVALID",
            format!("target Node {} has invalid platform labels", node.node_id),
        ));
    }
    let platform = TargetPlatform::new(normalize_os(&os), normalize_arch(&arch));
    Ok(match variant {
        Some(variant) => platform.with_variant(variant),
        None => platform,
    })
}

fn host_platform() -> TargetPlatform {
    TargetPlatform::new(
        normalize_os(std::env::consts::OS),
        normalize_arch(std::env::consts::ARCH),
    )
}

// These values are the complete signed release/runtime binding and keeping
// them explicit makes it difficult for a caller to omit one accidentally.
#[allow(clippy::too_many_arguments)]
fn container_spec(
    deployment_id: &str,
    service_id: &str,
    version: &semver::Version,
    checksum: &str,
    node: &NodeRecord,
    image: OciImageReference,
    release: &ServiceReleaseManifest,
    published_endpoint: Option<PublishedEndpoint>,
) -> ContainerSpec {
    let mut command = Vec::new();
    if !release.runtime.command.trim().is_empty() {
        command.push(release.runtime.command.trim().to_string());
    }
    command.extend(release.runtime.args.iter().cloned());
    let environment = release
        .runtime
        .env
        .iter()
        .map(|(key, value)| format!("{key}={value}"))
        .collect();
    let labels = HashMap::from([
        ("ojos.release_version".to_string(), version.to_string()),
        ("ojos.release_checksum".to_string(), checksum.to_string()),
        ("ojos.target_node_id".to_string(), node.node_id.clone()),
    ]);
    ContainerSpec {
        deployment_id: deployment_id.to_string(),
        service_id: service_id.to_string(),
        generation: 1,
        image,
        command,
        environment,
        labels,
        published_endpoint,
    }
}

fn managed_published_endpoint(
    endpoint: &str,
    service_id: &str,
    node: &NodeRecord,
    release: &ServiceReleaseManifest,
) -> Result<Option<PublishedEndpoint>, StoreApiError> {
    let endpoint = endpoint.trim();
    if endpoint.is_empty() {
        return Ok(None);
    }
    validate_endpoint_id(endpoint).map_err(|error| {
        StoreApiError::new(
            422,
            "STORE_MANAGED_ENDPOINT_INVALID",
            format!("managed endpoint is invalid: {error}"),
        )
    })?;
    let identity = parse_endpoint_id(endpoint).map_err(|error| {
        StoreApiError::new(
            422,
            "STORE_MANAGED_ENDPOINT_INVALID",
            format!("managed endpoint is invalid: {error}"),
        )
    })?;
    if identity.service_name != service_id {
        return Err(StoreApiError::new(
            422,
            "STORE_MANAGED_ENDPOINT_SERVICE_MISMATCH",
            format!(
                "managed endpoint service {} must match release service {service_id}",
                identity.service_name
            ),
        ));
    }
    let advertised_host = identity.host.parse::<IpAddr>().map_err(|_| {
        StoreApiError::new(
            422,
            "STORE_MANAGED_ENDPOINT_INVALID",
            "managed endpoint host must be an IP address",
        )
    })?;
    let node_host = node.host_ip.parse::<IpAddr>().map_err(|_| {
        StoreApiError::new(
            422,
            "STORE_TARGET_NODE_ENDPOINT_UNAVAILABLE",
            format!(
                "target Node {} does not advertise a valid host_ip",
                node.node_id
            ),
        )
    })?;
    if advertised_host != node_host {
        return Err(StoreApiError::new(
            422,
            "STORE_MANAGED_ENDPOINT_HOST_MISMATCH",
            format!(
                "managed endpoint host {} must equal target Node host_ip {}",
                identity.host, node.host_ip
            ),
        ));
    }
    let host_port = identity.port.parse::<u16>().map_err(|_| {
        StoreApiError::new(
            422,
            "STORE_MANAGED_ENDPOINT_INVALID",
            "managed endpoint port must be between 1 and 65535",
        )
    })?;
    let application_protocol = release.backend.protocol.trim().to_ascii_lowercase();
    let published = PublishedEndpoint {
        endpoint: endpoint.to_string(),
        application_protocol,
        container_port: release.backend.port,
        host_port,
        transport_protocol: PublishedPortProtocol::Tcp,
    };
    published.validate().map_err(|error| {
        StoreApiError::new(
            422,
            "STORE_MANAGED_ENDPOINT_INVALID",
            format!("managed endpoint cannot be published: {error}"),
        )
    })?;
    Ok(Some(published))
}

fn endpoint_socket(endpoint: &str) -> Option<(IpAddr, u16)> {
    let identity = parse_endpoint_id(endpoint).ok()?;
    Some((identity.host.parse().ok()?, identity.port.parse().ok()?))
}

fn ensure_endpoint_available(
    storage: &DurableStore,
    endpoint: &PublishedEndpoint,
    excluded_deployment_id: Option<&str>,
    expected_operation_id: Option<&str>,
) -> Result<(), StoreApiError> {
    let desired_socket = endpoint_socket(&endpoint.endpoint).ok_or_else(|| {
        StoreApiError::new(
            422,
            "STORE_MANAGED_ENDPOINT_INVALID",
            "managed endpoint socket is invalid",
        )
    })?;
    if let Some(existing) = storage
        .runtime_instances(None)
        .map_err(storage_error)?
        .into_iter()
        .find(|stored| {
            Some(stored.instance.deployment_id.as_str()) != excluded_deployment_id
                && endpoint_socket(&stored.endpoint) == Some(desired_socket)
        })
    {
        return Err(StoreApiError::new(
            409,
            "STORE_MANAGED_ENDPOINT_IN_USE",
            format!(
                "managed endpoint socket {} is already owned by deployment {}",
                endpoint.endpoint, existing.instance.deployment_id
            ),
        ));
    }
    let operations = storage
        .operation_store()
        .list()
        .map_err(|error| StoreApiError::new(500, "STORE_OPERATION_ERROR", error.to_string()))?;
    if operations.iter().any(|operation| {
        !operation.status.is_terminal()
            && Some(operation.operation_id.as_str()) != expected_operation_id
            && operation
                .request
                .get("endpoint")
                .and_then(Value::as_str)
                .and_then(endpoint_socket)
                == Some(desired_socket)
    }) {
        return Err(StoreApiError::new(
            409,
            "STORE_MANAGED_ENDPOINT_RESERVED",
            format!(
                "managed endpoint socket {} is reserved by an active Operation",
                endpoint.endpoint
            ),
        ));
    }
    Ok(())
}

fn ensure_deployment_available(
    storage: &DurableStore,
    deployment_id: &str,
    expected_digest: &str,
    expected_operation_id: Option<&str>,
) -> Result<(), StoreApiError> {
    if let Some(existing) = storage
        .runtime_instance(deployment_id)
        .map_err(storage_error)?
    {
        let same_digest = existing.instance.artifact_digest == expected_digest
            || existing.instance.artifact_digest.ends_with(expected_digest);
        return Err(StoreApiError::new(
            409,
            if same_digest {
                "STORE_RELEASE_ALREADY_INSTALLED"
            } else {
                "STORE_DEPLOYMENT_ID_CONFLICT"
            },
            format!("deployment {deployment_id} already exists"),
        ));
    }
    let operations = storage
        .operation_store()
        .list()
        .map_err(|error| StoreApiError::new(500, "STORE_OPERATION_ERROR", error.to_string()))?;
    if operations.iter().any(|operation| {
        !operation.status.is_terminal()
            && Some(operation.operation_id.as_str()) != expected_operation_id
            && (operation
                .request
                .get("deployment_id")
                .and_then(Value::as_str)
                == Some(deployment_id)
                || operation
                    .request
                    .get("planned_deployment_ids")
                    .and_then(Value::as_array)
                    .is_some_and(|planned| {
                        planned
                            .iter()
                            .any(|value| value.as_str() == Some(deployment_id))
                    }))
    }) {
        return Err(StoreApiError::new(
            409,
            "STORE_INSTALL_IN_PROGRESS",
            format!("deployment {deployment_id} already has an active Operation"),
        ));
    }
    Ok(())
}

fn enqueue_plan(
    storage: &DurableStore,
    plan: PlanOperation,
) -> Result<orchestrator_control_plane::DurableOperation, StoreApiError> {
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

fn store_plan_guard() -> Result<std::sync::MutexGuard<'static, ()>, StoreApiError> {
    STORE_PLAN_LOCK.lock().map_err(|_| {
        StoreApiError::new(
            503,
            "STORE_PLANNER_UNAVAILABLE",
            "Store planner coordination lock is poisoned",
        )
    })
}

fn deployment_id(service_id: &str, version: &semver::Version, node_id: &str) -> String {
    let digest = Sha256::digest(format!("{service_id}\0{version}\0{node_id}").as_bytes());
    format!("deployment-{service_id}-{:x}", digest)[..56].to_string()
}

fn operation_id(
    prefix: &str,
    target_id: &str,
    request: &ApiRequest,
) -> Result<String, StoreApiError> {
    let key = request
        .headers
        .get("idempotency-key")
        .map(String::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| {
            StoreApiError::new(
                400,
                "IDEMPOTENCY_KEY_REQUIRED",
                "Store mutations require an Idempotency-Key header",
            )
        })?;
    let digest = Sha256::digest(format!("{prefix}\0{target_id}\0{key}").as_bytes());
    Ok(format!("op-{prefix}-{digest:x}"))
}

fn parse_body<T: for<'de> Deserialize<'de>>(request: &ApiRequest) -> Result<T, StoreApiError> {
    if request.body.trim().is_empty() {
        return Err(StoreApiError::new(
            400,
            "STORE_REQUEST_INVALID",
            "request body must be a JSON object",
        ));
    }
    serde_json::from_str(&request.body).map_err(|error| {
        StoreApiError::new(
            400,
            "STORE_REQUEST_INVALID",
            format!("invalid Store request: {error}"),
        )
    })
}

fn is_sha256(value: &str) -> bool {
    value.strip_prefix("sha256:").is_some_and(|hex| {
        hex.len() == 64
            && hex
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    })
}

fn required_text<'a>(value: &'a str, field: &str) -> Result<&'a str, StoreApiError> {
    non_empty(value).ok_or_else(|| {
        StoreApiError::new(422, "STORE_REQUEST_INVALID", format!("{field} is required"))
    })
}

fn non_empty(value: &str) -> Option<&str> {
    let value = value.trim();
    (!value.is_empty()).then_some(value)
}

fn valid_platform_token(value: &str) -> bool {
    let value = value.trim();
    !value.is_empty()
        && value.len() <= 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}

fn normalize_os(value: &str) -> String {
    match value.trim().to_ascii_lowercase().as_str() {
        "win32" => "windows".to_string(),
        "darwin" => "macos".to_string(),
        value => value.to_string(),
    }
}

fn normalize_arch(value: &str) -> String {
    match value.trim().to_ascii_lowercase().as_str() {
        "amd64" | "x64" => "x86_64".to_string(),
        "arm64" => "aarch64".to_string(),
        value => value.to_string(),
    }
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(i64::MAX as u128) as i64
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
    operation_id: Option<&str>,
) -> ApiResponse {
    ApiResponse::problem(status, code, detail, request_id, operation_id)
        .with_header("X-Request-ID", request_id)
}

#[derive(Debug)]
struct StoreApiError {
    status: u16,
    code: &'static str,
    detail: String,
    operation_id: Option<String>,
}

impl StoreApiError {
    fn new(status: u16, code: &'static str, detail: impl Into<String>) -> Self {
        Self {
            status,
            code,
            detail: detail.into(),
            operation_id: None,
        }
    }
}

fn manager_error(error: anyhow::Error) -> StoreApiError {
    StoreApiError::new(
        status_for_error(&error),
        "STORE_REQUEST_REJECTED",
        error.to_string(),
    )
}

fn catalog_registry_error(error: CatalogRegistryError) -> StoreApiError {
    StoreApiError::new(error.status(), error.code(), error.detail())
}

fn storage_error(error: DurableError) -> StoreApiError {
    let status = match &error {
        DurableError::Conflict(_) => 409,
        DurableError::Invariant(_) | DurableError::Domain(_) => 422,
        DurableError::Storage(_) => 500,
    };
    StoreApiError::new(status, "STORE_STORAGE_ERROR", error.to_string())
}

fn core_error(error: orchestrator_legacy::OrchestratorError) -> StoreApiError {
    StoreApiError::new(422, "STORE_RELEASE_INVALID", error.to_string())
}

fn operation_error(error: orchestrator_control_plane::OperationError) -> StoreApiError {
    let operation_id = match &error {
        orchestrator_control_plane::OperationError::NotFound(operation_id) => {
            Some(operation_id.clone())
        }
        _ => None,
    };
    StoreApiError {
        status: match &error {
            orchestrator_control_plane::OperationError::NotFound(_) => 404,
            orchestrator_control_plane::OperationError::InvalidPlan(_) => 422,
            orchestrator_control_plane::OperationError::IdempotencyConflict
            | orchestrator_control_plane::OperationError::InvalidTransition { .. } => 409,
            orchestrator_control_plane::OperationError::Store(_)
            | orchestrator_control_plane::OperationError::Job(_) => 500,
        },
        code: "STORE_OPERATION_ERROR",
        detail: error.to_string(),
        operation_id,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog_registry::CatalogSource;
    use base64::Engine as _;
    use base64::engine::general_purpose::STANDARD;
    use ed25519_dalek::{Signer, SigningKey};
    use orchestrator_control_plane::{
        ClaimRequest, CompleteRequest, CompletionStatus, JobStatus, JobStore, OperationRepository,
    };
    use orchestrator_legacy::{OrchestratorStore, ServiceRelease};
    use orchestrator_manager::catalog_v2::{
        CatalogModuleV2, CatalogReleaseV2, CatalogTrustStore, CatalogV2, Ed25519Signature,
        MetadataPackageV2, ReleaseDependencyV2, RuntimeCapabilityV2,
    };
    use orchestrator_runtime::{RuntimeDesiredState, RuntimeInstance};
    use orchestrator_storage::{SqliteOrchestratorStore, StoredRuntimeInstance};
    use semver::Version;
    use semver::VersionReq;
    use std::collections::BTreeMap;
    use std::fs;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::path::PathBuf;
    use std::thread;
    use tempfile::TempDir;

    const DIGEST: &str = "sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
    const UPGRADE_DIGEST: &str =
        "sha256:abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789";
    const DEPENDENCY_DIGEST: &str =
        "sha256:1111111111111111111111111111111111111111111111111111111111111111";
    const SECOND_ROOT_DIGEST: &str =
        "sha256:2222222222222222222222222222222222222222222222222222222222222222";
    const CHECKSUM: &str =
        "sha256:abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789";
    struct Fixture {
        state: market_api::StoreState,
        console: OrchestratorActionConsole,
        durable: DurableStore,
        registry: CatalogRegistry,
        artifact_store: ArtifactStore,
        sqlite: SqliteOrchestratorStore,
        _directory: TempDir,
    }

    fn repo_root() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(Path::parent)
            .and_then(Path::parent)
            .expect("workspace root")
            .to_path_buf()
    }

    fn release_manifest(service_id: &str, image: &str) -> ServiceReleaseManifest {
        serde_json::from_value(json!({
            "schema_version": 1,
            "service_name": service_id,
            "version": "1.2.3",
            "description": "test runtime-only release",
            "service_type": "backend-api",
            "source": {"kind": "url", "url": "https://catalog.example/release.yaml", "checksum": CHECKSUM},
            "runtime": {
                "kind": "image",
                "image": image,
                "command": "",
                "args": [],
                "env": {"LOG_LEVEL": "info"}
            },
            "backend": {"protocol": "http", "port": 8080, "health_path": "/health"},
            "migrations": [],
            "permissions": [],
            "routes": [],
            "apis": [],
            "redis": [],
            "storage": [],
            "dependencies": [],
            "required_apis": [],
            "config_schema": {},
            "secrets": []
        }))
        .unwrap()
    }

    fn fixture(manifest: ServiceReleaseManifest, node_status: &str) -> Fixture {
        fixture_with_initial_channel(manifest, node_status, ReleaseChannel::Stable)
    }

    fn fixture_with_initial_channel(
        manifest: ServiceReleaseManifest,
        node_status: &str,
        initial_channel: ReleaseChannel,
    ) -> Fixture {
        let directory = tempfile::tempdir().unwrap();
        let artifact_store = ArtifactStore::open(&directory.path().join("artifacts")).unwrap();
        let metadata_name = format!("{}.release.yaml", manifest.service_name);
        let metadata = serde_yaml::to_string(&manifest).unwrap();
        fs::write(directory.path().join(&metadata_name), metadata.as_bytes()).unwrap();
        let metadata_checksum = format!("sha256:{:x}", Sha256::digest(metadata.as_bytes()));
        let mut upgrade_manifest = manifest.clone();
        upgrade_manifest.version = "2.0.0".to_string();
        upgrade_manifest.runtime.image = format!("registry.example/ojos/api@{UPGRADE_DIGEST}");
        let upgrade_metadata_name = format!("{}.2.0.0.release.yaml", manifest.service_name);
        let upgrade_metadata = serde_yaml::to_string(&upgrade_manifest).unwrap();
        fs::write(
            directory.path().join(&upgrade_metadata_name),
            upgrade_metadata.as_bytes(),
        )
        .unwrap();
        let upgrade_metadata_checksum =
            format!("sha256:{:x}", Sha256::digest(upgrade_metadata.as_bytes()));
        let signing_key = SigningKey::from_bytes(&[7_u8; 32]);
        let mut catalog = CatalogV2 {
            schema_version: 2,
            id: "fixture-catalog".to_string(),
            name: "Fixture Catalog".to_string(),
            modules: vec![CatalogModuleV2 {
                id: manifest.service_name.clone(),
                name: manifest.service_name.clone(),
                description: "signed Store API fixture".to_string(),
                kind: manifest.service_type.clone(),
                tags: vec!["fixture".to_string()],
                releases: vec![
                    CatalogReleaseV2 {
                        version: Version::parse(&manifest.version).unwrap(),
                        channel: initial_channel,
                        platforms: vec![TargetPlatform::new("linux", "x86_64")],
                        min_orchestrator_version: Version::parse("0.1.0").unwrap(),
                        dependencies: Vec::new(),
                        runtime_capabilities: Vec::new(),
                        metadata: MetadataPackageV2 {
                            url: metadata_name,
                            sha256: metadata_checksum.parse().unwrap(),
                        },
                        oci_image: format!("registry.example/ojos/api@{DIGEST}")
                            .parse()
                            .unwrap(),
                    },
                    CatalogReleaseV2 {
                        version: Version::parse("2.0.0").unwrap(),
                        channel: ReleaseChannel::Stable,
                        platforms: vec![TargetPlatform::new("linux", "x86_64")],
                        min_orchestrator_version: Version::parse("0.1.0").unwrap(),
                        dependencies: Vec::new(),
                        runtime_capabilities: Vec::new(),
                        metadata: MetadataPackageV2 {
                            url: upgrade_metadata_name,
                            sha256: upgrade_metadata_checksum.parse().unwrap(),
                        },
                        oci_image: format!("registry.example/ojos/api@{UPGRADE_DIGEST}")
                            .parse()
                            .unwrap(),
                    },
                ],
            }],
            signatures: Vec::new(),
        };
        let signature = signing_key.sign(&catalog.signing_payload_jcs().unwrap());
        catalog.signatures.push(Ed25519Signature {
            key_id: "fixture-key".to_string(),
            algorithm: "Ed25519".to_string(),
            signature: STANDARD.encode(signature.to_bytes()),
        });
        fs::write(
            directory.path().join("catalog.json"),
            serde_json::to_vec_pretty(&catalog).unwrap(),
        )
        .unwrap();
        let mut trust = CatalogTrustStore::new();
        trust
            .insert("fixture-key", signing_key.verifying_key().to_bytes())
            .unwrap();
        let registry = CatalogRegistry::new(
            directory.path(),
            trust,
            vec![CatalogSource {
                id: "fixture-source".to_string(),
                url: "catalog.json".to_string(),
                required_key_id: "fixture-key".to_string(),
                auth_secret_ref: String::new(),
                enabled: true,
                offline_oci_layouts: BTreeMap::new(),
            }],
        )
        .unwrap();
        let mut sqlite =
            SqliteOrchestratorStore::open(directory.path().join("orchestrator.db")).unwrap();
        let console =
            OrchestratorActionConsole::load_with_store(repo_root(), "sqlite", sqlite.clone())
                .unwrap();
        sqlite
            .upsert_node(NodeRecord {
                node_id: "node-1".to_string(),
                host_ip: "127.0.0.2".to_string(),
                parent_node_id: String::new(),
                role: "standalone".to_string(),
                labels: json!({"runtime": "docker", "os": "linux", "arch": "amd64"}),
                status: node_status.to_string(),
                created_at: "t0".to_string(),
                updated_at: "t0".to_string(),
            })
            .unwrap();
        sqlite
            .upsert_service_release(ServiceRelease {
                service_name: manifest.service_name.clone(),
                version: manifest.version.clone(),
                release_url: manifest.source.url.clone(),
                manifest: serde_json::to_value(&manifest).unwrap(),
                checksum: CHECKSUM.to_string(),
                created_at: "t0".to_string(),
            })
            .unwrap();
        let durable = DurableStore::Sqlite(sqlite.clone());
        registry.bootstrap(&durable).unwrap();
        Fixture {
            state: market_api::StoreState::new(),
            console,
            durable,
            registry,
            artifact_store,
            sqlite,
            _directory: directory,
        }
    }

    fn dependency(module_id: &str, requirement: &str) -> ReleaseDependencyV2 {
        ReleaseDependencyV2 {
            module_id: module_id.to_string(),
            requirement: VersionReq::parse(requirement).unwrap(),
            channel: ReleaseChannel::Stable,
        }
    }

    fn extend_fixture_catalog(
        fixture: &Fixture,
        existing_root_dependencies: Vec<ReleaseDependencyV2>,
        additions: Vec<(ServiceReleaseManifest, Vec<ReleaseDependencyV2>)>,
    ) {
        let catalog_path = fixture._directory.path().join("catalog.json");
        let mut catalog: CatalogV2 =
            serde_json::from_slice(&fs::read(&catalog_path).unwrap()).unwrap();
        for release in &mut catalog.modules[0].releases {
            release.dependencies = existing_root_dependencies.clone();
            let metadata_path = fixture._directory.path().join(&release.metadata.url);
            let mut manifest: ServiceReleaseManifest =
                serde_yaml::from_slice(&fs::read(&metadata_path).unwrap()).unwrap();
            manifest.dependencies = existing_root_dependencies
                .iter()
                .map(|dependency| dependency.module_id.clone())
                .collect();
            let metadata = serde_yaml::to_string(&manifest).unwrap();
            fs::write(&metadata_path, metadata.as_bytes()).unwrap();
            release.metadata.sha256 = format!("sha256:{:x}", Sha256::digest(metadata.as_bytes()))
                .parse()
                .unwrap();
        }
        for (mut manifest, dependencies) in additions {
            manifest.dependencies = dependencies
                .iter()
                .map(|dependency| dependency.module_id.clone())
                .collect();
            let metadata_name = format!(
                "{}.{}.release.yaml",
                manifest.service_name, manifest.version
            );
            let metadata = serde_yaml::to_string(&manifest).unwrap();
            fs::write(
                fixture._directory.path().join(&metadata_name),
                metadata.as_bytes(),
            )
            .unwrap();
            let metadata_checksum = format!("sha256:{:x}", Sha256::digest(metadata.as_bytes()));
            catalog.modules.push(CatalogModuleV2 {
                id: manifest.service_name.clone(),
                name: manifest.service_name.clone(),
                description: "dependency DAG fixture".to_string(),
                kind: manifest.service_type.clone(),
                tags: vec!["fixture".to_string()],
                releases: vec![CatalogReleaseV2 {
                    version: Version::parse(&manifest.version).unwrap(),
                    channel: ReleaseChannel::Stable,
                    platforms: vec![TargetPlatform::new("linux", "x86_64")],
                    min_orchestrator_version: Version::parse("0.1.0").unwrap(),
                    dependencies,
                    runtime_capabilities: Vec::new(),
                    metadata: MetadataPackageV2 {
                        url: metadata_name,
                        sha256: metadata_checksum.parse().unwrap(),
                    },
                    oci_image: manifest.runtime.image.parse().unwrap(),
                }],
            });
        }
        catalog.signatures.clear();
        let signing_key = SigningKey::from_bytes(&[7_u8; 32]);
        let signature = signing_key.sign(&catalog.signing_payload_jcs().unwrap());
        catalog.signatures.push(Ed25519Signature {
            key_id: "fixture-key".to_string(),
            algorithm: "Ed25519".to_string(),
            signature: STANDARD.encode(signature.to_bytes()),
        });
        fs::write(catalog_path, serde_json::to_vec_pretty(&catalog).unwrap()).unwrap();
        fixture.registry.bootstrap(&fixture.durable).unwrap();
    }

    fn install_request(service_id: &str, key: &str) -> ApiRequest {
        ApiRequest {
            method: "POST".to_string(),
            path: "/api/v1/store/releases:install".to_string(),
            headers: BTreeMap::from([("idempotency-key".to_string(), key.to_string())]),
            body: json!({
                "service_id": service_id,
                "version": "1.2.3",
                "target_node_id": "node-1"
            })
            .to_string(),
        }
    }

    fn provider_rich_release(service_id: &str) -> ServiceReleaseManifest {
        let mut release =
            release_manifest(service_id, &format!("registry.example/ojos/api@{DIGEST}"));
        release.permissions = vec![format!("{service_id}.read")];
        release.frontend = serde_json::from_value(json!({
            "enabled": true,
            "route_prefix": format!("/{service_id}"),
            "remote_entry": "/remote-entry.js",
            "menu_items": []
        }))
        .unwrap();
        release.redis = serde_json::from_value(json!([{
            "name": "events",
            "kind": "stream",
            "usage": "durable service events"
        }]))
        .unwrap();
        release.storage = serde_json::from_value(json!([{
            "object_type": "document",
            "bucket": "service-data",
            "path_prefix": "/objects"
        }]))
        .unwrap();
        release.routes = serde_json::from_value(json!([{
            "path": format!("/{service_id}"),
            "method": "GET",
            "target_type": "endpoint-group",
            "target": format!("{service_id}[*]"),
            "permission": format!("{service_id}.read")
        }]))
        .unwrap();
        release.migrations = serde_json::from_value(json!([{
            "version": "0001",
            "path": format!("services/{service_id}/migrations/0001.sql"),
            "checksum": format!("sha256:{}", "3".repeat(64)),
            "destructive": false,
            "oci": {
                "image": format!("registry.example/ojos/migration@{DEPENDENCY_DIGEST}"),
                "command": ["migrate", "up"],
                "env": {},
                "timeout_ms": 30000
            }
        }]))
        .unwrap();
        release
    }

    fn complete_only_operation_job(
        fixture: &Fixture,
        operation_id: &str,
        lease_token: &str,
        now_ms: i64,
    ) {
        let mut jobs = fixture.durable.job_store();
        let claimed = jobs
            .claim(ClaimRequest {
                node_id: "node-1".to_string(),
                instance_id: "provider-history-worker".to_string(),
                lease_token: lease_token.to_string(),
                now_ms,
                lease_ms: 30_000,
            })
            .unwrap()
            .expect("one queued operation job");
        jobs.complete(CompleteRequest {
            job_id: claimed.job_id,
            lease_token: lease_token.to_string(),
            status: CompletionStatus::Succeeded,
            result: json!({"verified": true}),
            error_message: String::new(),
            now_ms: now_ms + 1,
            events: vec![],
        })
        .unwrap();
        let mut operations = fixture.durable.operation_store();
        let operation = OperationCoordinator::new(&mut operations, &mut jobs)
            .project(operation_id, now_ms + 2)
            .unwrap();
        assert_eq!(operation.status, DurableOperationStatus::Succeeded);
    }

    fn put_running_instance(
        fixture: &Fixture,
        service_id: &str,
        deployment_id: &str,
        container_id: &str,
        image: &str,
    ) {
        fixture
            .durable
            .put_runtime_instance(&StoredRuntimeInstance {
                node_id: "node-1".to_string(),
                instance: RuntimeInstance {
                    deployment_id: deployment_id.to_string(),
                    service_id: service_id.to_string(),
                    release_version: "1.2.3".to_string(),
                    container_id: container_id.to_string(),
                    artifact_digest: image.to_string(),
                    desired_state: RuntimeDesiredState::Running,
                    observed_state: RuntimeObservedState::Running,
                    health: "HEALTHY".to_string(),
                },
                management_mode: orchestrator_storage::RuntimeManagementMode::Managed,
                endpoint: String::new(),
                updated_at: "t0".to_string(),
            })
            .unwrap();
    }

    fn record_release_history(
        fixture: &Fixture,
        operation_id: &str,
        deployment_id: &str,
        service_id: &str,
        version: &str,
        image: &str,
        now_ms: i64,
    ) {
        record_release_history_with_channel(
            fixture,
            operation_id,
            deployment_id,
            service_id,
            version,
            image,
            ReleaseChannel::Stable,
            now_ms,
        );
    }

    #[allow(clippy::too_many_arguments)]
    fn record_release_history_with_channel(
        fixture: &Fixture,
        operation_id: &str,
        deployment_id: &str,
        service_id: &str,
        version: &str,
        image: &str,
        channel: ReleaseChannel,
        now_ms: i64,
    ) {
        let mut operations = fixture.durable.operation_store();
        let mut jobs = fixture.durable.job_store();
        let plan = PlanOperation {
            operation_id: operation_id.to_string(),
            action: "release.install".to_string(),
            target_type: "Release".to_string(),
            target_id: format!("{service_id}@{version}"),
            request: json!({
                "service_id": service_id,
                "version": version,
                "image": image,
                "deployment_id": deployment_id,
                "catalog_source_id": "fixture-source",
                "catalog_id": "fixture-catalog",
                "catalog_verified_key_ids": ["fixture-key"],
                "channel": channel,
                "start": true,
            }),
            jobs: vec![PlannedJob {
                step_id: "runtime-install".to_string(),
                node_id: "node-1".to_string(),
                kind: JobKind::Install,
                depends_on: vec![],
                condition: Default::default(),
                payload: json!({"historical": true}),
                max_attempts: 3,
            }],
        };
        {
            let mut coordinator = OperationCoordinator::new(&mut operations, &mut jobs);
            coordinator.plan(plan, now_ms).unwrap();
            coordinator.confirm(operation_id, now_ms + 1).unwrap();
            coordinator.enqueue(operation_id, now_ms + 2).unwrap();
        }
        let claimed = jobs
            .claim(ClaimRequest {
                node_id: "node-1".to_string(),
                instance_id: "history-worker".to_string(),
                lease_token: format!("lease-{operation_id}"),
                now_ms: now_ms + 3,
                lease_ms: 30_000,
            })
            .unwrap()
            .expect("history job");
        jobs.complete(CompleteRequest {
            job_id: claimed.job_id,
            lease_token: format!("lease-{operation_id}"),
            status: CompletionStatus::Succeeded,
            result: json!({"historical": true}),
            error_message: String::new(),
            now_ms: now_ms + 4,
            events: Vec::new(),
        })
        .unwrap();
        OperationCoordinator::new(&mut operations, &mut jobs)
            .project(operation_id, now_ms + 5)
            .unwrap();
    }

    #[test]
    fn import_only_registers_metadata_and_never_creates_operation_or_job() {
        let manifest = release_manifest(
            "fixture-api",
            &format!("registry.example/ojos/api@{DIGEST}"),
        );
        let mut fixture = fixture(manifest, "READY");
        let request = ApiRequest {
            method: "POST".to_string(),
            path: "/api/v1/store/releases:import".to_string(),
            headers: BTreeMap::from([("idempotency-key".to_string(), "import-only-1".to_string())]),
            body: json!({
                "service_id": "fixture-api",
                "version": "1.2.3",
                "target_node_id": "node-1",
            })
            .to_string(),
        };
        let response = route(
            &fixture.state,
            &mut fixture.console,
            Some(&fixture.durable),
            Some(&fixture.registry),
            Some(&fixture.artifact_store),
            &request,
            "request-import",
        )
        .unwrap();
        assert_eq!(response.status, 201, "{}", response.body);
        assert_eq!(
            response.body["data"]["imported"][0]["release"]["service_name"],
            "fixture-api"
        );
        assert_eq!(
            response.body["data"]["verified_key_ids"],
            json!(["fixture-key"])
        );
        assert!(fixture.durable.operation_store().list().unwrap().is_empty());
        assert_eq!(
            fixture
                .durable
                .job_store()
                .active_job_count("node-1")
                .unwrap(),
            0
        );
        assert!(fixture.durable.runtime_instances(None).unwrap().is_empty());
    }

    #[test]
    fn arbitrary_url_import_cannot_bypass_trusted_catalog_selection() {
        let manifest = release_manifest(
            "fixture-api",
            &format!("registry.example/ojos/api@{DIGEST}"),
        );
        let mut fixture = fixture(manifest, "READY");
        let releases_before = fixture.console.service_releases().unwrap();
        let request = ApiRequest {
            method: "POST".to_string(),
            path: "/api/v1/store/releases:import".to_string(),
            headers: BTreeMap::from([(
                "idempotency-key".to_string(),
                "untrusted-import-1".to_string(),
            )]),
            body: json!({
                "source_url": "https://untrusted.example/release.yaml",
                "metadata_sha256": CHECKSUM,
            })
            .to_string(),
        };
        let response = route(
            &fixture.state,
            &mut fixture.console,
            Some(&fixture.durable),
            Some(&fixture.registry),
            Some(&fixture.artifact_store),
            &request,
            "request-untrusted-import",
        )
        .unwrap();
        assert_eq!(response.status, 400, "{}", response.body);
        assert_eq!(
            response.body["code"], "STORE_REQUEST_INVALID",
            "unknown URL/checksum fields must be rejected rather than ignored"
        );
        assert_eq!(fixture.console.service_releases().unwrap(), releases_before);
        assert!(fixture.durable.operation_store().list().unwrap().is_empty());
        assert!(fixture.durable.runtime_instances(None).unwrap().is_empty());
    }

    #[test]
    fn managed_install_enqueues_exact_durable_container_job_without_fake_projection() {
        let service_id = "fixture-api";
        let mut fixture = fixture(
            release_manifest(service_id, &format!("registry.example/ojos/api@{DIGEST}")),
            "READY",
        );
        let response = route(
            &fixture.state,
            &mut fixture.console,
            Some(&fixture.durable),
            Some(&fixture.registry),
            Some(&fixture.artifact_store),
            &install_request(service_id, "install-1"),
            "request-1",
        )
        .unwrap();
        assert_eq!(response.status, 202, "{}", response.body);
        assert_eq!(response.body["data"]["installed"], false);
        assert_eq!(
            response.body["data"]["release"]["target_platform"],
            json!({"os": "linux", "arch": "x86_64"})
        );
        let operation_id = response.body["data"]["operation_id"].as_str().unwrap();
        let operation = fixture
            .durable
            .operation_store()
            .get(operation_id)
            .unwrap()
            .unwrap();
        assert_eq!(operation.action, "release.install");
        assert_eq!(operation.request["catalog_source_id"], "fixture-source");
        assert_eq!(operation.request["catalog_id"], "fixture-catalog");
        assert_eq!(
            operation.request["catalog_verified_key_ids"],
            json!(["fixture-key"])
        );
        assert_eq!(
            operation.request["catalog_plan"]["root"]["module_id"],
            service_id
        );
        assert_eq!(operation.planned_jobs.len(), 1);
        assert_eq!(operation.planned_jobs[0].kind, JobKind::Install);
        assert_eq!(operation.planned_jobs[0].node_id, "node-1");
        assert_eq!(
            operation.planned_jobs[0].payload["spec"]["image"],
            json!({
                "repository": "registry.example/ojos/api",
                "digest": DIGEST,
            })
        );
        assert_eq!(operation.planned_jobs[0].payload["start"], true);
        assert!(
            operation.planned_jobs[0]
                .payload
                .get("require_healthy")
                .is_none()
        );
        assert_eq!(
            operation.planned_jobs[0].payload["health_gate"],
            json!({
                "timeout_ms": 60_000,
                "poll_interval_ms": 1_000,
                "missing_healthcheck": "reject",
                "compensation_timeout_ms": 30_000,
            })
        );
        let binding = &operation.job_bindings[0];
        let job = fixture
            .durable
            .job_store()
            .get(&binding.job_id)
            .unwrap()
            .unwrap();
        assert_eq!(job.status, JobStatus::Queued);
        assert!(fixture.durable.runtime_instances(None).unwrap().is_empty());
    }

    #[test]
    fn managed_endpoint_install_replay_is_idempotent_and_changed_payload_conflicts() {
        let service_id = "fixture-api";
        let mut fixture = fixture(
            release_manifest(service_id, &format!("registry.example/ojos/api@{DIGEST}")),
            "READY",
        );
        let request = ApiRequest {
            method: "POST".to_string(),
            path: "/api/v1/store/releases:install".to_string(),
            headers: BTreeMap::from([(
                "idempotency-key".to_string(),
                "managed-endpoint-replay".to_string(),
            )]),
            body: json!({
                "service_id": service_id,
                "version": "1.2.3",
                "target_node_id": "node-1",
                "endpoint": "127.0.0.2:20000:fixture-api"
            })
            .to_string(),
        };
        let first = route(
            &fixture.state,
            &mut fixture.console,
            Some(&fixture.durable),
            Some(&fixture.registry),
            Some(&fixture.artifact_store),
            &request,
            "request-managed-endpoint-first",
        )
        .unwrap();
        assert_eq!(first.status, 202, "{}", first.body);
        let operation_id = first.body["data"]["operation_id"].clone();

        let replay = route(
            &fixture.state,
            &mut fixture.console,
            Some(&fixture.durable),
            Some(&fixture.registry),
            Some(&fixture.artifact_store),
            &request,
            "request-managed-endpoint-replay",
        )
        .unwrap();
        assert_eq!(replay.status, 202, "{}", replay.body);
        assert_eq!(replay.body["data"]["operation_id"], operation_id);
        assert_eq!(fixture.durable.operation_store().list().unwrap().len(), 1);

        let mut changed = request.clone();
        changed.body = json!({
            "service_id": service_id,
            "version": "1.2.3",
            "target_node_id": "node-1",
            "endpoint": "127.0.0.2:20001:fixture-api"
        })
        .to_string();
        let conflict = route(
            &fixture.state,
            &mut fixture.console,
            Some(&fixture.durable),
            Some(&fixture.registry),
            Some(&fixture.artifact_store),
            &changed,
            "request-managed-endpoint-conflict",
        )
        .unwrap();
        assert_eq!(conflict.status, 409, "{}", conflict.body);
        assert_eq!(conflict.body["code"], "STORE_OPERATION_ERROR");
        assert_eq!(fixture.durable.operation_store().list().unwrap().len(), 1);
    }

    #[test]
    fn dependency_dag_is_dependency_first_and_reserves_shared_deployments() {
        let first_root = "root-a";
        let dependency_id = "shared-db";
        let mut fixture = fixture(
            release_manifest(first_root, &format!("registry.example/ojos/api@{DIGEST}")),
            "READY",
        );
        let mut dependency_manifest = release_manifest(
            dependency_id,
            &format!("registry.example/ojos/shared-db@{DEPENDENCY_DIGEST}"),
        );
        dependency_manifest.version = "1.0.0".to_string();
        let second_root = "root-b";
        let second_manifest = release_manifest(
            second_root,
            &format!("registry.example/ojos/root-b@{SECOND_ROOT_DIGEST}"),
        );
        extend_fixture_catalog(
            &fixture,
            vec![dependency(dependency_id, "=1.0.0")],
            vec![
                (dependency_manifest, vec![]),
                (second_manifest, vec![dependency(dependency_id, "=1.0.0")]),
            ],
        );

        let first = route(
            &fixture.state,
            &mut fixture.console,
            Some(&fixture.durable),
            Some(&fixture.registry),
            Some(&fixture.artifact_store),
            &install_request(first_root, "shared-dependency-a"),
            "request-shared-a",
        )
        .unwrap();
        assert_eq!(first.status, 202, "{}", first.body);
        let operation_id = first.body["data"]["operation_id"].as_str().unwrap();
        let operation = fixture
            .durable
            .operation_store()
            .get(operation_id)
            .unwrap()
            .unwrap();
        assert_eq!(operation.planned_jobs.len(), 2);
        assert_eq!(operation.planned_jobs[0].kind, JobKind::Install);
        assert_eq!(
            operation.planned_jobs[0].payload["spec"]["service_id"],
            dependency_id
        );
        assert_eq!(
            operation.planned_jobs[1].depends_on,
            vec![operation.planned_jobs[0].step_id.clone()]
        );
        assert!(
            operation
                .planned_jobs
                .iter()
                .all(|job| job.kind != JobKind::Uninstall),
            "shared dependency compensation must never be an unconditional uninstall"
        );
        assert_eq!(operation.job_bindings.len(), 1);
        assert_eq!(
            operation.request["planned_deployment_ids"]
                .as_array()
                .unwrap()
                .len(),
            2
        );

        let second = route(
            &fixture.state,
            &mut fixture.console,
            Some(&fixture.durable),
            Some(&fixture.registry),
            Some(&fixture.artifact_store),
            &install_request(second_root, "shared-dependency-b"),
            "request-shared-b",
        )
        .unwrap();
        assert_eq!(second.status, 409, "{}", second.body);
        assert_eq!(second.body["code"], "STORE_INSTALL_IN_PROGRESS");
        assert_eq!(fixture.durable.operation_store().list().unwrap().len(), 1);
        assert_eq!(
            fixture
                .durable
                .job_store()
                .active_job_count("node-1")
                .unwrap(),
            1
        );
    }

    #[test]
    fn external_install_uses_control_plane_health_and_never_enqueues_docker() {
        let service_id = "external-api";
        let mut fixture = fixture(
            release_manifest(service_id, &format!("registry.example/ojos/api@{DIGEST}")),
            "READY",
        );
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(std::time::Duration::from_secs(2)))
                .unwrap();
            let mut request = [0_u8; 2048];
            let read = stream.read(&mut request).unwrap();
            assert!(
                std::str::from_utf8(&request[..read])
                    .expect("health request must be valid UTF-8")
                    .starts_with("GET /health HTTP/")
            );
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok")
                .unwrap();
        });
        let request = ApiRequest {
            method: "POST".to_string(),
            path: "/api/v1/store/releases:install".to_string(),
            headers: BTreeMap::from([(
                "idempotency-key".to_string(),
                "external-install-1".to_string(),
            )]),
            body: json!({
                "service_id": service_id,
                "version": "1.2.3",
                "target_node_id": "node-1",
                "mode": "EXTERNAL",
                "endpoint": endpoint,
            })
            .to_string(),
        };
        let response = route(
            &fixture.state,
            &mut fixture.console,
            Some(&fixture.durable),
            Some(&fixture.registry),
            Some(&fixture.artifact_store),
            &request,
            "request-external-install",
        )
        .unwrap();
        assert_eq!(response.status, 202, "{}", response.body);
        let operation_id = response.body["data"]["operation_id"].as_str().unwrap();
        let deployment_id = response.body["data"]["deployment_id"].as_str().unwrap();
        let operation = fixture
            .durable
            .operation_store()
            .get(operation_id)
            .unwrap()
            .unwrap();
        assert_eq!(operation.planned_jobs.len(), 1);
        assert_eq!(operation.planned_jobs[0].kind, JobKind::ExternalHealth);
        assert_eq!(operation.planned_jobs[0].node_id, CONTROL_PLANE_NODE_ID);
        assert_eq!(
            fixture
                .durable
                .job_store()
                .active_job_count("node-1")
                .unwrap(),
            0,
            "External install must not enqueue any Docker Agent job"
        );
        assert!(
            fixture
                .durable
                .runtime_instance(deployment_id)
                .unwrap()
                .is_none()
        );

        assert!(crate::topology_worker::process_one(&fixture.durable, None).unwrap());
        server.join().unwrap();
        let stored = fixture
            .durable
            .runtime_instance(deployment_id)
            .unwrap()
            .expect("healthy External projection");
        assert_eq!(
            stored.management_mode,
            orchestrator_storage::RuntimeManagementMode::External
        );
        assert_eq!(stored.endpoint, endpoint);
        assert!(stored.instance.container_id.is_empty());
        assert_eq!(
            stored.instance.observed_state,
            RuntimeObservedState::Running
        );
        assert_eq!(stored.instance.health, "HEALTHY");
        assert_eq!(
            fixture
                .durable
                .operation_store()
                .get(operation_id)
                .unwrap()
                .unwrap()
                .status,
            DurableOperationStatus::Succeeded
        );
    }

    #[test]
    fn unhealthy_external_install_leaves_no_runtime_projection() {
        let service_id = "external-failure-api";
        let mut fixture = fixture(
            release_manifest(service_id, &format!("registry.example/ojos/api@{DIGEST}")),
            "READY",
        );
        let request = ApiRequest {
            method: "POST".to_string(),
            path: "/api/v1/store/releases:install".to_string(),
            headers: BTreeMap::from([(
                "idempotency-key".to_string(),
                "external-install-failure-1".to_string(),
            )]),
            body: json!({
                "service_id": service_id,
                "version": "1.2.3",
                "target_node_id": "node-1",
                "mode": "EXTERNAL",
                "endpoint": "http://127.0.0.1:9",
            })
            .to_string(),
        };
        let response = route(
            &fixture.state,
            &mut fixture.console,
            Some(&fixture.durable),
            Some(&fixture.registry),
            Some(&fixture.artifact_store),
            &request,
            "request-external-failure",
        )
        .unwrap();
        assert_eq!(response.status, 202, "{}", response.body);
        let operation_id = response.body["data"]["operation_id"].as_str().unwrap();
        let deployment_id = response.body["data"]["deployment_id"].as_str().unwrap();
        assert!(crate::topology_worker::process_one(&fixture.durable, None).unwrap());
        assert!(
            fixture
                .durable
                .runtime_instance(deployment_id)
                .unwrap()
                .is_none()
        );
        let operation = fixture
            .durable
            .operation_store()
            .get(operation_id)
            .unwrap()
            .unwrap();
        assert_eq!(operation.status, DurableOperationStatus::Failed);
        assert!(operation.error_message.contains("health probe failed"));
        assert_eq!(
            fixture
                .durable
                .job_store()
                .active_job_count("node-1")
                .unwrap(),
            0
        );
    }

    #[test]
    fn upgrade_uses_catalog_digest_and_enqueues_safe_replacement_payload() {
        let service_id = "upgrade-api";
        let mut fixture = fixture(
            release_manifest(service_id, &format!("registry.example/ojos/api@{DIGEST}")),
            "READY",
        );
        let current_image = format!("registry.example/ojos/api@{DIGEST}");
        put_running_instance(
            &fixture,
            service_id,
            "deployment-current-v1",
            "container-current-v1",
            &current_image,
        );
        record_release_history(
            &fixture,
            "op-history-v1",
            "deployment-current-v1",
            service_id,
            "1.2.3",
            &current_image,
            100,
        );
        let request = ApiRequest {
            method: "POST".to_string(),
            path: "/api/v1/store/releases:upgrade".to_string(),
            headers: BTreeMap::from([(
                "idempotency-key".to_string(),
                "upgrade-release-1".to_string(),
            )]),
            body: json!({
                "deployment_id": "deployment-current-v1",
                "version": "2.0.0"
            })
            .to_string(),
        };
        let response = route(
            &fixture.state,
            &mut fixture.console,
            Some(&fixture.durable),
            Some(&fixture.registry),
            Some(&fixture.artifact_store),
            &request,
            "request-upgrade",
        )
        .unwrap();
        assert_eq!(response.status, 202, "{}", response.body);
        let operation_id = response.body["data"]["operation_id"].as_str().unwrap();
        let replay = route(
            &fixture.state,
            &mut fixture.console,
            Some(&fixture.durable),
            Some(&fixture.registry),
            Some(&fixture.artifact_store),
            &request,
            "request-upgrade-replay",
        )
        .unwrap();
        assert_eq!(replay.status, 202, "{}", replay.body);
        assert_eq!(replay.body["data"]["operation_id"], json!(operation_id));
        let mut changed = request.clone();
        changed.body = json!({
            "deployment_id": "deployment-current-v1",
            "version": "2.0.0",
            "migration_policy": "SKIP"
        })
        .to_string();
        let conflict = route(
            &fixture.state,
            &mut fixture.console,
            Some(&fixture.durable),
            Some(&fixture.registry),
            Some(&fixture.artifact_store),
            &changed,
            "request-upgrade-conflict",
        )
        .unwrap();
        assert_eq!(conflict.status, 409, "{}", conflict.body);
        assert_eq!(conflict.body["code"], "STORE_OPERATION_ERROR");
        let operation = fixture
            .durable
            .operation_store()
            .get(operation_id)
            .unwrap()
            .unwrap();
        assert_eq!(operation.action, "release.upgrade");
        assert_eq!(operation.planned_jobs[0].kind, JobKind::Upgrade);
        assert_eq!(
            operation.planned_jobs[0].payload["old_deployment_id"],
            "deployment-current-v1"
        );
        assert_eq!(
            operation.planned_jobs[0].payload["old_container_id"],
            "container-current-v1"
        );
        assert_eq!(operation.planned_jobs[0].payload["start"], true);
        assert_eq!(
            operation.planned_jobs[0].payload["new_spec"]["image"],
            json!({
                "repository": "registry.example/ojos/api",
                "digest": UPGRADE_DIGEST,
            })
        );
        assert_eq!(operation.request["previous_operation_id"], "op-history-v1");
        assert_eq!(operation.request["version"], "2.0.0");
        let projections = fixture.durable.runtime_instances(None).unwrap();
        assert_eq!(projections.len(), 1);
        assert_eq!(
            projections[0].instance.deployment_id,
            "deployment-current-v1"
        );
    }

    #[test]
    fn skip_omits_migrations_across_install_upgrade_and_rollback_provider_sagas() {
        let service_id = "provider-saga-api";
        let mut fixture = fixture(provider_rich_release(service_id), "READY");
        let mut node = fixture.durable.get_node("node-1").unwrap().unwrap();
        node.labels["providers"] = json!({
            "auth": true,
            "redis": {"enabled": true, "connection_id": "cache-main"},
            "storage": {
                "enabled": true,
                "backend": "node_directory",
                "connection_id": "node-files"
            },
            "frontend": {"enabled": true, "asset_store_id": "gateway-assets"},
            "gateway": true
        });
        fixture.durable.upsert_node(node).unwrap();

        let install = route(
            &fixture.state,
            &mut fixture.console,
            Some(&fixture.durable),
            Some(&fixture.registry),
            Some(&fixture.artifact_store),
            &ApiRequest {
                method: "POST".to_string(),
                path: "/api/v1/store/releases:install".to_string(),
                headers: BTreeMap::from([(
                    "idempotency-key".to_string(),
                    "provider-saga-install".to_string(),
                )]),
                body: json!({
                    "service_id": service_id,
                    "version": "1.2.3",
                    "target_node_id": "node-1",
                    "migration_policy": "SKIP",
                    "gateway_node_id": "node-1"
                })
                .to_string(),
            },
            "request-provider-saga-install",
        )
        .unwrap();
        assert_eq!(install.status, 202, "{}", install.body);
        let install_operation_id = install.body["data"]["operation_id"]
            .as_str()
            .unwrap()
            .to_string();
        let old_deployment_id = install.body["data"]["deployment_id"]
            .as_str()
            .unwrap()
            .to_string();
        let install_operation = fixture
            .durable
            .operation_store()
            .get(&install_operation_id)
            .unwrap()
            .unwrap();
        assert_eq!(
            install_operation.planned_jobs[0].kind,
            JobKind::ReleasePipeline
        );
        assert_eq!(
            install_operation.planned_jobs[0].payload["migrations"],
            json!([])
        );
        assert!(install_operation.planned_jobs[0].payload["auth"].is_object());
        assert_eq!(
            install_operation.planned_jobs[0].payload["provisioners"]
                .as_array()
                .unwrap()
                .len(),
            3
        );
        assert_eq!(
            install_operation.planned_jobs[0].payload["provisioners"][0]["resources"][0]["connection_id"],
            "cache-main"
        );
        assert_eq!(
            install_operation.planned_jobs[0].payload["provisioners"][1]["resources"][0]["connection_id"],
            "node-files"
        );
        assert_eq!(
            install_operation.planned_jobs[0].payload["provisioners"][2]["asset_store_id"],
            "gateway-assets"
        );
        assert!(install_operation.planned_jobs[0].payload["gateway"].is_object());
        complete_only_operation_job(&fixture, &install_operation_id, "lease-install", now_ms());
        let old_image = format!("registry.example/ojos/api@{DIGEST}");
        put_running_instance(
            &fixture,
            service_id,
            &old_deployment_id,
            "container-provider-v1",
            &old_image,
        );

        let upgrade = route(
            &fixture.state,
            &mut fixture.console,
            Some(&fixture.durable),
            Some(&fixture.registry),
            Some(&fixture.artifact_store),
            &ApiRequest {
                method: "POST".to_string(),
                path: "/api/v1/store/releases:upgrade".to_string(),
                headers: BTreeMap::from([(
                    "idempotency-key".to_string(),
                    "provider-saga-upgrade".to_string(),
                )]),
                body: json!({
                    "deployment_id": old_deployment_id,
                    "version": "2.0.0",
                    "migration_policy": "SKIP",
                    "gateway_node_id": "node-1"
                })
                .to_string(),
            },
            "request-provider-saga-upgrade",
        )
        .unwrap();
        assert_eq!(upgrade.status, 202, "{}", upgrade.body);
        let upgrade_operation_id = upgrade.body["data"]["operation_id"]
            .as_str()
            .unwrap()
            .to_string();
        let new_deployment_id = upgrade.body["data"]["deployment_id"]
            .as_str()
            .unwrap()
            .to_string();
        let upgrade_operation = fixture
            .durable
            .operation_store()
            .get(&upgrade_operation_id)
            .unwrap()
            .unwrap();
        let upgrade_payload = &upgrade_operation.planned_jobs[0].payload;
        assert_eq!(upgrade_payload["migrations"], json!([]));
        assert_eq!(
            upgrade_payload["provider_saga"]["previous"]["revision_id"],
            install_operation_id
        );
        assert_eq!(
            upgrade_payload["provider_saga"]["desired"]["revision_id"],
            upgrade_operation_id
        );
        assert_eq!(
            upgrade_payload["provider_saga"]["previous"]["provisioners"]
                .as_array()
                .unwrap()
                .len(),
            3
        );
        assert!(upgrade_payload["provider_saga"]["previous"]["gateway"].is_object());
        complete_only_operation_job(&fixture, &upgrade_operation_id, "lease-upgrade", now_ms());
        fixture
            .durable
            .delete_runtime_instance(&old_deployment_id)
            .unwrap();
        let new_image = format!("registry.example/ojos/api@{UPGRADE_DIGEST}");
        put_running_instance(
            &fixture,
            service_id,
            &new_deployment_id,
            "container-provider-v2",
            &new_image,
        );

        let rollback = route(
            &fixture.state,
            &mut fixture.console,
            Some(&fixture.durable),
            Some(&fixture.registry),
            Some(&fixture.artifact_store),
            &ApiRequest {
                method: "POST".to_string(),
                path: "/api/v1/store/releases:rollback".to_string(),
                headers: BTreeMap::from([(
                    "idempotency-key".to_string(),
                    "provider-saga-rollback".to_string(),
                )]),
                body: json!({
                    "deployment_id": new_deployment_id,
                    "version": "1.2.3",
                    "migration_policy": "SKIP",
                    "gateway_node_id": "node-1"
                })
                .to_string(),
            },
            "request-provider-saga-rollback",
        )
        .unwrap();
        assert_eq!(rollback.status, 202, "{}", rollback.body);
        let rollback_operation_id = rollback.body["data"]["operation_id"]
            .as_str()
            .unwrap()
            .to_string();
        let rollback_operation = fixture
            .durable
            .operation_store()
            .get(&rollback_operation_id)
            .unwrap()
            .unwrap();
        let rollback_payload = &rollback_operation.planned_jobs[0].payload;
        assert_eq!(rollback_payload["migrations"], json!([]));
        assert_eq!(
            rollback_payload["provider_saga"]["previous"]["revision_id"],
            upgrade_operation_id
        );
        assert_eq!(
            rollback_payload["provider_saga"]["desired"]["revision_id"],
            rollback_operation_id
        );
        assert_eq!(
            rollback_operation.request["rollback_proof_operation_id"],
            install_operation_id
        );
    }

    #[test]
    fn upgrade_waits_for_missing_dependencies_and_preserves_old_instance_on_failure() {
        let service_id = "upgrade-with-dependency";
        let dependency_id = "upgrade-shared-db";
        let mut fixture = fixture(
            release_manifest(service_id, &format!("registry.example/ojos/api@{DIGEST}")),
            "READY",
        );
        let mut dependency_manifest = release_manifest(
            dependency_id,
            &format!("registry.example/ojos/shared-db@{DEPENDENCY_DIGEST}"),
        );
        dependency_manifest.version = "1.0.0".to_string();
        extend_fixture_catalog(
            &fixture,
            vec![dependency(dependency_id, "=1.0.0")],
            vec![(dependency_manifest, vec![])],
        );
        let current_image = format!("registry.example/ojos/api@{DIGEST}");
        put_running_instance(
            &fixture,
            service_id,
            "deployment-upgrade-old",
            "container-upgrade-old",
            &current_image,
        );
        record_release_history(
            &fixture,
            "op-history-upgrade-dependency",
            "deployment-upgrade-old",
            service_id,
            "1.2.3",
            &current_image,
            100,
        );
        let response = route(
            &fixture.state,
            &mut fixture.console,
            Some(&fixture.durable),
            Some(&fixture.registry),
            Some(&fixture.artifact_store),
            &ApiRequest {
                method: "POST".to_string(),
                path: "/api/v1/store/releases:upgrade".to_string(),
                headers: BTreeMap::from([(
                    "idempotency-key".to_string(),
                    "upgrade-dependency-1".to_string(),
                )]),
                body: json!({
                    "deployment_id": "deployment-upgrade-old",
                    "version": "2.0.0"
                })
                .to_string(),
            },
            "request-upgrade-dependency",
        )
        .unwrap();
        assert_eq!(response.status, 202, "{}", response.body);
        let operation_id = response.body["data"]["operation_id"].as_str().unwrap();
        let operation = fixture
            .durable
            .operation_store()
            .get(operation_id)
            .unwrap()
            .unwrap();
        assert_eq!(operation.planned_jobs.len(), 2);
        assert_eq!(operation.planned_jobs[0].kind, JobKind::Install);
        assert_eq!(operation.planned_jobs[1].kind, JobKind::Upgrade);
        assert_eq!(
            operation.planned_jobs[1].depends_on,
            vec![operation.planned_jobs[0].step_id.clone()]
        );
        assert_eq!(operation.job_bindings.len(), 1);

        let mut jobs = fixture.durable.job_store();
        let leased = jobs
            .claim(ClaimRequest {
                node_id: "node-1".to_string(),
                instance_id: "dependency-worker".to_string(),
                lease_token: "dependency-failed".to_string(),
                now_ms: now_ms(),
                lease_ms: 30_000,
            })
            .unwrap()
            .unwrap();
        assert_eq!(leased.kind, JobKind::Install);
        jobs.complete(CompleteRequest {
            job_id: leased.job_id,
            lease_token: "dependency-failed".to_string(),
            status: CompletionStatus::Failed,
            result: json!({"failure": "dependency unavailable"}),
            error_message: "dependency unavailable".to_string(),
            now_ms: now_ms(),
            events: vec![],
        })
        .unwrap();
        let mut operations = fixture.durable.operation_store();
        let failed = OperationCoordinator::new(&mut operations, &mut jobs)
            .project(operation_id, now_ms())
            .unwrap();
        assert_eq!(failed.status, DurableOperationStatus::Failed);
        assert_eq!(
            failed.result[operation.planned_jobs[1].step_id.as_str()]["status"],
            "SKIPPED"
        );
        assert!(
            fixture
                .durable
                .runtime_instance("deployment-upgrade-old")
                .unwrap()
                .is_some(),
            "dependency failure must not touch the proven old runtime"
        );
        assert_eq!(fixture.durable.runtime_instances(None).unwrap().len(), 1);
    }

    #[test]
    fn rollback_requires_proven_history_and_uses_the_exact_historical_digest() {
        let service_id = "rollback-api";
        let mut fixture = fixture(
            release_manifest(service_id, &format!("registry.example/ojos/api@{DIGEST}")),
            "READY",
        );
        let current_image = format!("registry.example/ojos/api@{UPGRADE_DIGEST}");
        put_running_instance(
            &fixture,
            service_id,
            "deployment-current-v2",
            "container-current-v2",
            &current_image,
        );
        record_release_history(
            &fixture,
            "op-history-current-v2",
            "deployment-current-v2",
            service_id,
            "2.0.0",
            &current_image,
            200,
        );
        let rollback_request = |key: &str| ApiRequest {
            method: "POST".to_string(),
            path: "/api/v1/store/releases:rollback".to_string(),
            headers: BTreeMap::from([("idempotency-key".to_string(), key.to_string())]),
            body: json!({
                "deployment_id": "deployment-current-v2",
                "version": "1.2.3"
            })
            .to_string(),
        };
        let unproven = route(
            &fixture.state,
            &mut fixture.console,
            Some(&fixture.durable),
            Some(&fixture.registry),
            Some(&fixture.artifact_store),
            &rollback_request("rollback-unproven-1"),
            "request-rollback-unproven",
        )
        .unwrap();
        assert_eq!(unproven.status, 422, "{}", unproven.body);
        assert_eq!(unproven.body["code"], "STORE_ROLLBACK_HISTORY_UNPROVEN");

        let historical_image = format!("registry.example/ojos/api@{DIGEST}");
        record_release_history(
            &fixture,
            "op-history-prior-v1",
            "deployment-historical-v1",
            service_id,
            "1.2.3",
            &historical_image,
            100,
        );
        let response = route(
            &fixture.state,
            &mut fixture.console,
            Some(&fixture.durable),
            Some(&fixture.registry),
            Some(&fixture.artifact_store),
            &rollback_request("rollback-proven-1"),
            "request-rollback-proven",
        )
        .unwrap();
        assert_eq!(response.status, 202, "{}", response.body);
        let operation_id = response.body["data"]["operation_id"].as_str().unwrap();
        let operation = fixture
            .durable
            .operation_store()
            .get(operation_id)
            .unwrap()
            .unwrap();
        assert_eq!(operation.action, "release.rollback");
        assert_eq!(operation.planned_jobs[0].kind, JobKind::Rollback);
        assert_eq!(
            operation.request["rollback_proof_operation_id"],
            "op-history-prior-v1"
        );
        assert_eq!(
            operation.planned_jobs[0].payload["new_spec"]["image"],
            json!({
                "repository": "registry.example/ojos/api",
                "digest": DIGEST,
            })
        );
    }

    #[test]
    fn rollback_without_explicit_channel_reuses_the_proven_historical_channel() {
        let service_id = "rollback-beta-api";
        let mut fixture = fixture_with_initial_channel(
            release_manifest(service_id, &format!("registry.example/ojos/api@{DIGEST}")),
            "READY",
            ReleaseChannel::Beta,
        );
        let current_image = format!("registry.example/ojos/api@{UPGRADE_DIGEST}");
        put_running_instance(
            &fixture,
            service_id,
            "deployment-current-stable-v2",
            "container-current-stable-v2",
            &current_image,
        );
        record_release_history_with_channel(
            &fixture,
            "op-history-current-stable-v2",
            "deployment-current-stable-v2",
            service_id,
            "2.0.0",
            &current_image,
            ReleaseChannel::Stable,
            200,
        );
        let historical_image = format!("registry.example/ojos/api@{DIGEST}");
        record_release_history_with_channel(
            &fixture,
            "op-history-beta-v1",
            "deployment-historical-beta-v1",
            service_id,
            "1.2.3",
            &historical_image,
            ReleaseChannel::Beta,
            100,
        );
        let request = ApiRequest {
            method: "POST".to_string(),
            path: "/api/v1/store/releases:rollback".to_string(),
            headers: BTreeMap::from([(
                "idempotency-key".to_string(),
                "rollback-beta-history-1".to_string(),
            )]),
            body: json!({
                "deployment_id": "deployment-current-stable-v2",
                "version": "1.2.3"
            })
            .to_string(),
        };
        let response = route(
            &fixture.state,
            &mut fixture.console,
            Some(&fixture.durable),
            Some(&fixture.registry),
            Some(&fixture.artifact_store),
            &request,
            "request-rollback-beta",
        )
        .unwrap();
        assert_eq!(response.status, 202, "{}", response.body);
        let operation_id = response.body["data"]["operation_id"].as_str().unwrap();
        let operation = fixture
            .durable
            .operation_store()
            .get(operation_id)
            .unwrap()
            .unwrap();
        assert_eq!(operation.request["channel"], "beta");
        assert_eq!(
            operation.request["rollback_proof_operation_id"],
            "op-history-beta-v1"
        );
        assert_eq!(
            operation.planned_jobs[0].payload["new_spec"]["image"]["digest"],
            DIGEST
        );
    }

    #[test]
    fn catalog_source_and_package_routes_use_the_trusted_registry() {
        let service_id = "catalog-api";
        let mut fixture = fixture(
            release_manifest(service_id, &format!("registry.example/ojos/api@{DIGEST}")),
            "READY",
        );
        let register = ApiRequest {
            method: "POST".to_string(),
            path: "/api/v1/store/catalogs".to_string(),
            headers: BTreeMap::from([(
                "idempotency-key".to_string(),
                "catalog-register-1".to_string(),
            )]),
            body: json!({
                "id": "fixture-source-copy",
                "url": "catalog.json",
                "required_key_id": "fixture-key",
                "enabled": true
            })
            .to_string(),
        };
        let registered = route(
            &fixture.state,
            &mut fixture.console,
            Some(&fixture.durable),
            Some(&fixture.registry),
            Some(&fixture.artifact_store),
            &register,
            "request-catalog-register",
        )
        .unwrap();
        assert_eq!(registered.status, 201, "{}", registered.body);

        let first_catalog_page_request = ApiRequest {
            method: "GET".to_string(),
            path: "/api/v1/store/catalogs?limit=1".to_string(),
            headers: BTreeMap::new(),
            body: String::new(),
        };
        let first_catalog_page = route(
            &fixture.state,
            &mut fixture.console,
            Some(&fixture.durable),
            Some(&fixture.registry),
            Some(&fixture.artifact_store),
            &first_catalog_page_request,
            "request-catalog-page-1",
        )
        .unwrap();
        assert_eq!(first_catalog_page.status, 200);
        assert_eq!(
            first_catalog_page.body["data"]["items"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
        let cursor = first_catalog_page.body["data"]["next_cursor"]
            .as_str()
            .expect("second catalog page cursor");
        let second_catalog_page_request = ApiRequest {
            method: "GET".to_string(),
            path: format!("/api/v1/store/catalogs?limit=1&cursor={cursor}"),
            headers: BTreeMap::new(),
            body: String::new(),
        };
        let second_catalog_page = route(
            &fixture.state,
            &mut fixture.console,
            Some(&fixture.durable),
            Some(&fixture.registry),
            Some(&fixture.artifact_store),
            &second_catalog_page_request,
            "request-catalog-page-2",
        )
        .unwrap();
        assert_eq!(second_catalog_page.status, 200);
        assert_eq!(
            second_catalog_page.body["data"]["items"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
        assert!(second_catalog_page.body["data"]["next_cursor"].is_null());

        let delete = ApiRequest {
            method: "DELETE".to_string(),
            path: "/api/v1/store/catalogs/fixture-source-copy".to_string(),
            headers: BTreeMap::from([(
                "idempotency-key".to_string(),
                "catalog-delete-1".to_string(),
            )]),
            body: "{}".to_string(),
        };
        let deleted = route(
            &fixture.state,
            &mut fixture.console,
            Some(&fixture.durable),
            Some(&fixture.registry),
            Some(&fixture.artifact_store),
            &delete,
            "request-catalog-delete",
        )
        .unwrap();
        assert_eq!(deleted.status, 200, "{}", deleted.body);

        let packages = ApiRequest {
            method: "GET".to_string(),
            path:
                "/api/v1/store/packages?search=catalog&channel=stable&os=linux&arch=amd64&limit=1"
                    .to_string(),
            headers: BTreeMap::new(),
            body: String::new(),
        };
        let packages = route(
            &fixture.state,
            &mut fixture.console,
            Some(&fixture.durable),
            Some(&fixture.registry),
            Some(&fixture.artifact_store),
            &packages,
            "request-catalog-packages",
        )
        .unwrap();
        assert_eq!(packages.status, 200, "{}", packages.body);
        assert_eq!(packages.body["data"]["items"][0]["module_id"], service_id);
        assert_eq!(
            packages.body["data"]["items"][0]["oci_image"],
            format!("registry.example/ojos/api@{DIGEST}")
        );
    }

    #[test]
    fn release_validate_checks_trusted_plan_without_publishing_or_enqueuing() {
        let service_id = "validate-api";
        let mut fixture = fixture(
            release_manifest(service_id, &format!("registry.example/ojos/api@{DIGEST}")),
            "READY",
        );
        let releases_before = fixture.console.service_releases().unwrap();
        let request = ApiRequest {
            method: "POST".to_string(),
            path: "/api/v1/store/releases:validate".to_string(),
            headers: BTreeMap::from([(
                "idempotency-key".to_string(),
                "validate-release-1".to_string(),
            )]),
            body: json!({
                "service_id": service_id,
                "target_node_id": "node-1",
                "version": "2.0.0",
                "channel": "stable"
            })
            .to_string(),
        };
        let response = route(
            &fixture.state,
            &mut fixture.console,
            Some(&fixture.durable),
            Some(&fixture.registry),
            Some(&fixture.artifact_store),
            &request,
            "request-release-validate",
        )
        .unwrap();
        assert_eq!(response.status, 200, "{}", response.body);
        assert_eq!(response.body["data"]["valid"], true);
        assert_eq!(
            response.body["data"]["side_effects"],
            json!({
                "release_imports": 0,
                "operations": 0,
                "jobs": 0,
                "runtime_calls": 0,
            })
        );
        assert_eq!(fixture.console.service_releases().unwrap(), releases_before);
        assert!(fixture.durable.operation_store().list().unwrap().is_empty());
        assert_eq!(
            fixture
                .durable
                .job_store()
                .active_job_count("node-1")
                .unwrap(),
            0
        );
        assert!(fixture.durable.runtime_instances(None).unwrap().is_empty());
    }

    #[test]
    fn metadata_cannot_replace_the_catalog_oci_digest_with_a_mutable_tag() {
        let service_id = "tagged-api";
        let mut fixture = fixture(
            release_manifest(service_id, "registry.example/ojos/api:latest"),
            "READY",
        );
        let response = route(
            &fixture.state,
            &mut fixture.console,
            Some(&fixture.durable),
            Some(&fixture.registry),
            Some(&fixture.artifact_store),
            &install_request(service_id, "tagged-1"),
            "request-2",
        )
        .unwrap();
        assert_eq!(response.status, 422, "{}", response.body);
        assert_eq!(response.body["code"], "CATALOG_METADATA_OCI_MISMATCH");
        assert!(fixture.durable.operation_store().list().unwrap().is_empty());
    }

    #[test]
    fn catalog_metadata_checksum_mismatch_fails_before_publication_or_runtime_work() {
        let service_id = "tampered-api";
        let mut fixture = fixture(
            release_manifest(service_id, &format!("registry.example/ojos/api@{DIGEST}")),
            "READY",
        );
        fs::write(
            fixture
                ._directory
                .path()
                .join(format!("{service_id}.release.yaml")),
            b"tampered after catalog signing",
        )
        .unwrap();
        let releases_before = fixture.console.service_releases().unwrap();
        let response = route(
            &fixture.state,
            &mut fixture.console,
            Some(&fixture.durable),
            Some(&fixture.registry),
            Some(&fixture.artifact_store),
            &install_request(service_id, "tampered-1"),
            "request-tampered",
        )
        .unwrap();
        assert_eq!(response.status, 422, "{}", response.body);
        assert_eq!(response.body["code"], "CATALOG_METADATA_CHECKSUM_MISMATCH");
        assert_eq!(fixture.console.service_releases().unwrap(), releases_before);
        assert!(fixture.durable.operation_store().list().unwrap().is_empty());
        assert!(fixture.durable.runtime_instances(None).unwrap().is_empty());
    }

    #[test]
    fn signed_runtime_capability_must_match_the_exact_release_manifest() {
        let service_id = "capability-mismatch-api";
        let mut fixture = fixture(
            release_manifest(service_id, &format!("registry.example/ojos/api@{DIGEST}")),
            "READY",
        );
        let catalog_path = fixture._directory.path().join("catalog.json");
        let mut catalog: CatalogV2 =
            serde_json::from_slice(&fs::read(&catalog_path).unwrap()).unwrap();
        catalog.modules[0].releases[0].runtime_capabilities =
            vec![RuntimeCapabilityV2::LinkProbeV1];
        catalog.signatures.clear();
        let signing_key = SigningKey::from_bytes(&[7_u8; 32]);
        let signature = signing_key.sign(&catalog.signing_payload_jcs().unwrap());
        catalog.signatures.push(Ed25519Signature {
            key_id: "fixture-key".to_string(),
            algorithm: "Ed25519".to_string(),
            signature: STANDARD.encode(signature.to_bytes()),
        });
        fs::write(&catalog_path, serde_json::to_vec_pretty(&catalog).unwrap()).unwrap();

        let releases_before = fixture.console.service_releases().unwrap();
        let response = route(
            &fixture.state,
            &mut fixture.console,
            Some(&fixture.durable),
            Some(&fixture.registry),
            Some(&fixture.artifact_store),
            &install_request(service_id, "capability-mismatch-1"),
            "request-capability-mismatch",
        )
        .unwrap();
        assert_eq!(response.status, 422, "{}", response.body);
        assert_eq!(
            response.body["code"],
            "CATALOG_METADATA_CAPABILITY_MISMATCH"
        );
        assert_eq!(fixture.console.service_releases().unwrap(), releases_before);
        assert!(fixture.durable.operation_store().list().unwrap().is_empty());
        assert!(fixture.durable.runtime_instances(None).unwrap().is_empty());
    }

    #[test]
    fn missing_pipeline_provider_and_non_ready_node_fail_during_plan() {
        let service_id = "provider-api";
        let mut release =
            release_manifest(service_id, &format!("registry.example/ojos/api@{DIGEST}"));
        release.permissions.push("provider.read".to_string());
        let mut provider_fixture = fixture(release, "READY");
        let provider_response = route(
            &provider_fixture.state,
            &mut provider_fixture.console,
            Some(&provider_fixture.durable),
            Some(&provider_fixture.registry),
            Some(&provider_fixture.artifact_store),
            &install_request(service_id, "provider-1"),
            "request-3",
        )
        .unwrap();
        assert_eq!(provider_response.status, 422);
        assert_eq!(provider_response.body["code"], "STORE_PROVIDER_REQUIRED");
        assert!(
            provider_fixture
                .durable
                .operation_store()
                .list()
                .unwrap()
                .is_empty()
        );

        let service_id = "offline-api";
        let mut offline_fixture = fixture(
            release_manifest(service_id, &format!("registry.example/ojos/api@{DIGEST}")),
            "OFFLINE",
        );
        let offline_response = route(
            &offline_fixture.state,
            &mut offline_fixture.console,
            Some(&offline_fixture.durable),
            Some(&offline_fixture.registry),
            Some(&offline_fixture.artifact_store),
            &install_request(service_id, "offline-1"),
            "request-4",
        )
        .unwrap();
        assert_eq!(offline_response.status, 409);
        assert_eq!(offline_response.body["code"], "STORE_TARGET_NODE_NOT_READY");
        assert!(
            offline_fixture
                .sqlite
                .runtime_instances(None)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn typed_provider_advertisement_requires_provider_specific_configuration() {
        let service_id = "provider-config-api";
        let mut release =
            release_manifest(service_id, &format!("registry.example/ojos/api@{DIGEST}"));
        release.redis = serde_json::from_value(json!([{
            "name": "events",
            "kind": "stream",
            "usage": "events"
        }]))
        .unwrap();
        let mut fixture = fixture(release, "READY");
        let mut node = fixture.durable.get_node("node-1").unwrap().unwrap();
        node.labels["providers"] = json!({
            "redis": {"enabled": true}
        });
        fixture.durable.upsert_node(node).unwrap();
        let response = route(
            &fixture.state,
            &mut fixture.console,
            Some(&fixture.durable),
            Some(&fixture.registry),
            Some(&fixture.artifact_store),
            &install_request(service_id, "provider-config-invalid"),
            "request-provider-config-invalid",
        )
        .unwrap();
        assert_eq!(response.status, 422, "{}", response.body);
        assert_eq!(
            response.body["code"],
            "STORE_PROVIDER_CONFIGURATION_INVALID"
        );
        assert!(fixture.durable.operation_store().list().unwrap().is_empty());
    }

    #[test]
    fn typed_config_and_secret_refs_are_materialized_without_persisting_secret_values() {
        let service_id = "configured-api";
        let mut release =
            release_manifest(service_id, &format!("registry.example/ojos/api@{DIGEST}"));
        release.config_schema = json!({
            "type": "object",
            "properties": {
                "workers": {"type": "integer"},
                "db_password": {"type": "secret"}
            },
            "required": ["workers", "db_password"],
            "additionalProperties": false
        });
        release
            .runtime
            .env
            .insert("WORKERS".to_string(), "${config.workers}".to_string());
        release.runtime.env.insert(
            "DB_PASSWORD".to_string(),
            "${secret.db_password}".to_string(),
        );
        release.permissions.push("configured.read".to_string());
        let mut configured_fixture = fixture(release, "READY");
        let mut node = configured_fixture
            .durable
            .get_node("node-1")
            .unwrap()
            .unwrap();
        node.labels["providers"] = json!({
            "materialization": {"enabled": true, "secret_provider": "file"},
            "auth": true
        });
        configured_fixture.durable.upsert_node(node).unwrap();

        let request = ApiRequest {
            method: "POST".to_string(),
            path: "/api/v1/store/releases:install".to_string(),
            headers: BTreeMap::from([(
                "idempotency-key".to_string(),
                "configured-install-1".to_string(),
            )]),
            body: json!({
                "service_id": service_id,
                "version": "1.2.3",
                "target_node_id": "node-1",
                "config": {"workers": 4},
                "secret_refs": {"db_password": "configured-api-db-password"}
            })
            .to_string(),
        };
        let response = route(
            &configured_fixture.state,
            &mut configured_fixture.console,
            Some(&configured_fixture.durable),
            Some(&configured_fixture.registry),
            Some(&configured_fixture.artifact_store),
            &request,
            "request-configured-install",
        )
        .unwrap();
        assert_eq!(response.status, 202, "{}", response.body);
        let operation_id = response.body["data"]["operation_id"].as_str().unwrap();
        let operation = configured_fixture
            .durable
            .operation_store()
            .get(operation_id)
            .unwrap()
            .unwrap();
        assert_eq!(operation.planned_jobs.len(), 1);
        assert_eq!(operation.planned_jobs[0].kind, JobKind::ReleasePipeline);
        let payload = &operation.planned_jobs[0].payload;
        assert_eq!(payload["materialization"]["config"]["workers"], "4");
        assert_eq!(
            payload["materialization"]["secret_refs"]["db_password"],
            "configured-api-db-password"
        );
        assert_eq!(payload["auth"]["permissions"], json!(["configured.read"]));
        assert!(
            !serde_json::to_string(payload)
                .unwrap()
                .contains("plaintext-password")
        );

        let mut invalid_fixture = fixture(
            {
                let mut manifest = release_manifest(
                    "configured-invalid",
                    &format!("registry.example/ojos/api@{DIGEST}"),
                );
                manifest.config_schema = json!({
                    "properties": {"db_password": {"type": "secret"}},
                    "required": ["db_password"]
                });
                manifest
            },
            "READY",
        );
        let invalid = route(
            &invalid_fixture.state,
            &mut invalid_fixture.console,
            Some(&invalid_fixture.durable),
            Some(&invalid_fixture.registry),
            Some(&invalid_fixture.artifact_store),
            &ApiRequest {
                method: "POST".to_string(),
                path: "/api/v1/store/releases:install".to_string(),
                headers: BTreeMap::from([(
                    "idempotency-key".to_string(),
                    "configured-invalid-1".to_string(),
                )]),
                body: json!({
                    "service_id": "configured-invalid",
                    "version": "1.2.3",
                    "target_node_id": "node-1",
                    "config": {"db_password": "plaintext-password"},
                    "secret_refs": {"db_password": "vault://db-password"}
                })
                .to_string(),
            },
            "request-configured-invalid",
        )
        .unwrap();
        assert_eq!(invalid.status, 422, "{}", invalid.body);
        assert_eq!(invalid.body["code"], "STORE_SECRET_VALUE_FORBIDDEN");
        assert!(
            invalid_fixture
                .durable
                .operation_store()
                .list()
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn release_delete_removes_only_the_requested_unreferenced_release_metadata() {
        let service_id = "remove-api";
        let mut fixture = fixture(
            release_manifest(service_id, &format!("registry.example/ojos/api@{DIGEST}")),
            "READY",
        );
        let request = ApiRequest {
            method: "POST".to_string(),
            path: "/api/v1/store/releases:delete".to_string(),
            headers: BTreeMap::from([(
                "idempotency-key".to_string(),
                "release-delete-1".to_string(),
            )]),
            body: json!({"service_id": service_id, "version": "1.2.3"}).to_string(),
        };
        let response = route(
            &fixture.state,
            &mut fixture.console,
            Some(&fixture.durable),
            Some(&fixture.registry),
            Some(&fixture.artifact_store),
            &request,
            "request-release-delete",
        )
        .unwrap();
        assert_eq!(response.status, 200, "{}", response.body);
        assert_eq!(response.body["data"]["deleted"], true);
        assert_eq!(response.body["data"]["service_id"], service_id);
        assert_eq!(response.body["data"]["version"], "1.2.3");
        assert_eq!(
            response.body["data"]["action_result"]["action_id"],
            "release.delete"
        );
        assert_eq!(
            response.body["data"]["action_result"]["status"],
            "SUCCEEDED"
        );
        assert!(
            !fixture
                .console
                .service_releases()
                .unwrap()
                .iter()
                .any(|release| release.service_name == service_id && release.version == "1.2.3")
        );
    }

    #[test]
    fn release_delete_fails_closed_when_a_runtime_reference_has_no_version_proof() {
        let service_id = "remove-api";
        let mut fixture = fixture(
            release_manifest(service_id, &format!("registry.example/ojos/api@{DIGEST}")),
            "READY",
        );
        fixture
            .durable
            .put_runtime_instance(&StoredRuntimeInstance {
                node_id: "node-1".to_string(),
                instance: RuntimeInstance {
                    deployment_id: "deployment-remove".to_string(),
                    service_id: service_id.to_string(),
                    release_version: "1.2.3".to_string(),
                    container_id: "0123456789abcdef".to_string(),
                    artifact_digest: format!("registry.example/ojos/api@{DIGEST}"),
                    desired_state: RuntimeDesiredState::Running,
                    observed_state: RuntimeObservedState::Running,
                    health: "HEALTHY".to_string(),
                },
                management_mode: orchestrator_storage::RuntimeManagementMode::Managed,
                endpoint: String::new(),
                updated_at: "t0".to_string(),
            })
            .unwrap();
        let request = ApiRequest {
            method: "POST".to_string(),
            path: "/api/v1/store/releases:delete".to_string(),
            headers: BTreeMap::from([(
                "idempotency-key".to_string(),
                "release-delete-in-use-1".to_string(),
            )]),
            body: json!({"service_id": service_id, "version": "1.2.3"}).to_string(),
        };
        let response = route(
            &fixture.state,
            &mut fixture.console,
            Some(&fixture.durable),
            Some(&fixture.registry),
            Some(&fixture.artifact_store),
            &request,
            "request-release-delete-in-use",
        )
        .unwrap();
        assert_eq!(response.status, 409, "{}", response.body);
        assert_eq!(response.body["code"], "STORE_RELEASE_REFERENCE_UNKNOWN");
        assert!(
            fixture
                .console
                .service_releases()
                .unwrap()
                .iter()
                .any(|release| release.service_name == service_id && release.version == "1.2.3")
        );
    }
}
