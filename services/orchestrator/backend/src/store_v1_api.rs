//! Store HTTP parsing, response envelopes and error mapping. Application work lives in `crate::store`.
use crate::adapters::catalog::CatalogRegistryReader;
use crate::adapters::catalog::InstalledServicesReader;
use crate::adapters::store_validation::StoreValidationReader;
use crate::artifact_store::ArtifactStore;
use crate::catalog_registry::CatalogRegistry;
use crate::catalog_registry::CatalogSourceRegistration;
use crate::catalog_registry::PackageQuery;
use crate::durable::DurableStore;
use crate::http::ApiRequest;
use crate::http::ApiResponse;
use crate::http::query_value;
use crate::market_api;
use crate::registry::RegistryContext;
use crate::routes::status_for_error;
use crate::store::commands::{normalize_store_topology_selection, required_text};
use crate::store::context::MutationContext;
use crate::store::error::catalog_registry_error;
use orchestrator_manager::catalog_query;
use orchestrator_manager::catalog_query::CatalogQueryError;
use orchestrator_manager::catalog_query::CatalogReadPort;
use orchestrator_manager::catalog_v2::ReleaseChannel;
use orchestrator_manager::catalog_v2::TargetPlatform;
use orchestrator_manager::store::validation::InstallBindingSelection;
use orchestrator_manager::store::validation::InstallTopologySelection;
use orchestrator_manager::store::validation::ReleaseValidationError;
use orchestrator_manager::store::validation::ValidateRelease;
use orchestrator_manager::store::validation::validate_release;
use serde::Deserialize;
use serde_json::Value;
use serde_json::json;
use std::collections::BTreeMap;

use crate::store::commands::{default_apply_policy, default_release_channel, default_true};
use crate::store::error::StoreError as StoreApiError;
use crate::store::replacement::ReplacementAction;

pub(crate) fn route(
    _state: &market_api::StoreState,
    registry_context: &mut RegistryContext,
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
        ("GET", "/api/v1/store/packages") => list_catalog_packages(
            registry_context,
            storage,
            catalog_registry,
            request,
            request_id,
        ),
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
            import_release(
                registry_context,
                storage,
                catalog_registry,
                request,
                request_id,
            )
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
            validate_release_catalog(
                registry_context,
                storage,
                catalog_registry,
                request,
                request_id,
            )
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
                registry_context,
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
            delete_release_metadata(registry_context, storage, request, request_id)
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
                registry_context,
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
                registry_context,
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
    let page = CatalogRegistryReader::new(registry, storage)
        .source_page(cursor.as_deref(), limit)
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
    registry_context: &RegistryContext,
    storage: Option<&DurableStore>,
    registry: Option<&CatalogRegistry>,
    request: &ApiRequest,
    request_id: &str,
) -> Result<ApiResponse, StoreApiError> {
    let (storage, registry) = require_catalog_registry(storage, registry)?;
    let query = package_query(request)?;
    let result = catalog_query::list_packages(
        &CatalogRegistryReader::new(registry, storage),
        &InstalledServicesReader::new(registry_context),
        &query,
    )
    .map_err(|error| match error {
        CatalogQueryError::Catalog(error) => catalog_registry_error(error),
        CatalogQueryError::Installations(error) => manager_error(error),
    })?;
    Ok(success(200, json!(result), request_id))
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
struct ValidateReleaseRequest {
    service_id: String,
    target_node_id: String,
    #[serde(default)]
    catalog_source_id: String,
    #[serde(default)]
    version: String,
    #[serde(default = "default_release_channel")]
    channel: String,
    #[serde(default)]
    endpoint: String,
    #[serde(default)]
    bindings: Vec<InstallBindingSelection>,
    #[serde(default)]
    topology_id: String,
    #[serde(default)]
    topology_etag: String,
    /// Compatibility input for 0.2 clients. New clients use the two explicit
    /// fields above so the optimistic-concurrency token cannot be mistaken for
    /// an arbitrary revision selector.
    #[serde(default)]
    topology: Option<InstallTopologySelection>,
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
    /// Optional provider choices and per-node values may be supplied to make
    /// validation return `valid=true`; the full immutable CompositionPlan is
    /// returned even when inputs remain unresolved.
    #[serde(default)]
    inputs: BTreeMap<String, BTreeMap<String, Value>>,
}

fn validate_release_catalog(
    registry_context: &RegistryContext,
    storage: &DurableStore,
    registry: &CatalogRegistry,
    request: &ApiRequest,
    request_id: &str,
) -> Result<ApiResponse, StoreApiError> {
    let input: ValidateReleaseRequest = parse_body(request)?;
    let topology = normalize_store_topology_selection(
        &input.topology_id,
        &input.topology_etag,
        input.topology.as_ref(),
    )?;
    let command = ValidateRelease {
        service_id: required_text(&input.service_id, "service_id")?.to_string(),
        target_node_id: required_text(&input.target_node_id, "target_node_id")?.to_string(),
        catalog_source_id: input.catalog_source_id,
        version: input.version,
        channel: input.channel,
        endpoint: input.endpoint,
        bindings: input.bindings,
        topology,
        start: input.start,
        migration_policy: input.migration_policy,
        gateway_node_id: input.gateway_node_id,
        config: input.config,
        secret_refs: input.secret_refs,
        inputs: input.inputs,
        validation_id: format!("store-validate-{request_id}"),
    };
    let result = validate_release(
        &StoreValidationReader {
            registry_context,
            storage,
            registry,
        },
        &command,
    )
    .map_err(|error| match error {
        ReleaseValidationError::Read(error) => error,
        ReleaseValidationError::Rule(error) => error.into(),
        ReleaseValidationError::MissingRootMetadata => StoreApiError::new(
            500,
            "CATALOG_PLAN_INVALID",
            "resolved validation plan does not contain its requested root metadata",
        ),
    })?;
    Ok(success(200, result, request_id))
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

fn manager_error(error: anyhow::Error) -> StoreApiError {
    StoreApiError::new(
        status_for_error(&error),
        "STORE_REQUEST_REJECTED",
        error.to_string(),
    )
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

fn import_release(
    registry_context: &mut RegistryContext,
    storage: &DurableStore,
    registry: &CatalogRegistry,
    request: &ApiRequest,
    request_id: &str,
) -> Result<ApiResponse, StoreApiError> {
    let input = parse_body(request)?;
    let result =
        crate::store::metadata::import_release(registry_context, storage, registry, input)?;
    Ok(success(201, result, request_id))
}

fn install_release(
    registry_context: &mut RegistryContext,
    storage: &DurableStore,
    registry: &CatalogRegistry,
    artifacts: Option<&ArtifactStore>,
    request: &ApiRequest,
    request_id: &str,
) -> Result<ApiResponse, StoreApiError> {
    let input = parse_body(request)?;
    let context = mutation_context(request);
    let result = crate::store::install::install_release(
        registry_context,
        storage,
        registry,
        artifacts,
        input,
        &context,
    )?;
    Ok(success(202, result, request_id))
}

fn replace_release(
    registry_context: &mut RegistryContext,
    storage: &DurableStore,
    registry: &CatalogRegistry,
    artifacts: Option<&ArtifactStore>,
    request: &ApiRequest,
    request_id: &str,
    action: ReplacementAction,
) -> Result<ApiResponse, StoreApiError> {
    let input = parse_body(request)?;
    let context = mutation_context(request);
    let result = crate::store::replacement::replace_release(
        registry_context,
        storage,
        registry,
        artifacts,
        input,
        &context,
        action,
    )?;
    Ok(success(202, result, request_id))
}

fn delete_release_metadata(
    registry_context: &mut RegistryContext,
    storage: &DurableStore,
    request: &ApiRequest,
    request_id: &str,
) -> Result<ApiResponse, StoreApiError> {
    let input = parse_body(request)?;
    let context = mutation_context(request);
    let result = crate::store::metadata::delete_release_metadata(
        registry_context,
        storage,
        input,
        &context,
    )?;
    Ok(success(200, result, request_id))
}

fn mutation_context(request: &ApiRequest) -> MutationContext {
    MutationContext {
        idempotency_key: request.headers.get("idempotency-key").cloned(),
    }
}
