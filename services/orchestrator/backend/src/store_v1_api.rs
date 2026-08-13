use crate::artifact_store::{ArtifactRetentionPolicy, ArtifactStore, MAX_ARTIFACT_BYTES};
use crate::catalog_registry::{
    CatalogRegistry, CatalogRegistryError, CatalogSourceRegistration, PackageQuery,
    ResolvedCatalogPlan, VerifiedReleaseDocument,
};
use crate::contribution_controller::{
    ContributionReplacementDagV1, SignedContributionSuccessorV1, append_contribution_job_fragment,
    append_contribution_replacement_job_fragment, contribution_job_steps, stage_contribution,
    stage_signed_contribution_successor,
};
use crate::durable::{DurableError, DurableStore};
use crate::http::{ApiRequest, ApiResponse, query_value};
use crate::{market_api, routes::status_for_error};
use orchestrator_agent::NodeRuntimeFactsV1;
use orchestrator_control_plane::{
    DurableOperation, DurableOperationStatus, JobKind, JobStore, OperationCoordinator,
    OperationRepository, PlanOperation, PlannedJob, PlannedJobCondition,
};
use orchestrator_legacy::composition::{
    ApiRequirementV1 as CompositionApiRequirementV1, CompositionModeV1, CompositionNodeSpecV1,
    CompositionPlanBindingV1, CompositionPlanV1, CompositionReleaseV1, ConfigRequirementV1,
    INSTALL_INPUTS_SCHEMA_VERSION, InstallInputsV1, PackageDependencyV1, ProvidedApiV1,
    ProviderCandidateV1, ProviderKindV1, ProviderPolicyV1, ReleaseGraphV1, ResourceLifecycleV1,
    ResourceRequirementV1, SecretRequirementV1, ValidatedInstallInputsV1, build_composition_plan,
    validate_install_inputs,
};
use orchestrator_legacy::{
    ActionRequest, ApiBinding, ApiBindingDesiredState, ApiBindingHealth, ApiBindingObservedState,
    ApiBindingResolutionRequest, ApiBindingState, ApiProviderCandidate, ContributionRevisionV1,
    NodeRecord, OrchestratorActionConsole, ServiceRelease, ServiceReleaseContract,
    ServiceReleaseManifest, TopologyApiBindingSpec, TopologyEndpointSpec, TopologyLinkSpec,
    TopologySpec, api_version_matches, diff_topology_specs, parse_endpoint_id,
    resolve_api_binding_candidate, validate_endpoint_id, validate_service_release,
};
use orchestrator_manager::MigrationPolicyV2;
use orchestrator_manager::catalog_v2::{ReleaseChannel, TargetPlatform};
use orchestrator_runtime::{
    ArtifactReference, AuthPipelineStep, AuthServiceIdentitySpec, BindingContextApplyPayload,
    ContainerSpec, GatewayPipelineStep, GatewayRouteSpec, HealthGatePolicy,
    MANAGED_EVENT_STREAM_V1, ManagedApiBinding, ManagedEventBinding, ManagedEventSubscription,
    ManagedServiceContextProjection, ManagedServiceContextSpec, ManagedWorkloadVerifierSpec,
    OciImageReference, OciMigrationStep, PublishedEndpoint, PublishedPortProtocol,
    RedisNamespaceSpec, ReleasePipelinePayload, ReleaseProviderRevision, ReleaseReplacementPayload,
    ReplacementProviderSaga, ResourceClaimStepV1, RetainedVolumeAttachmentV1, RuntimeContract,
    RuntimeInstallPayload, RuntimeMaterializationStep, RuntimeObservedState, RuntimeProfile,
    SERVICE_CONTRACT_GENERATION_LABEL, StorageResourceSpec, TypedProvisionerStep,
    stable_container_name,
};
use orchestrator_storage::ContributionRepository;
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
const NODE_RUNTIME_FACTS_STALE_MS: i64 = 60_000;

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
            validate_release_catalog(console, storage, catalog_registry, request, request_id)
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
    let platform = target_platform(storage, &node)?;
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
    console: &OrchestratorActionConsole,
    storage: &DurableStore,
    registry: &CatalogRegistry,
    request: &ApiRequest,
    request_id: &str,
) -> Result<ApiResponse, StoreApiError> {
    let mut input: ValidateReleaseRequest = parse_body(request)?;
    input.topology = normalize_store_topology_selection(
        &input.topology_id,
        &input.topology_etag,
        input.topology.as_ref(),
    )?;
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
    ensure_ready_docker_node(storage, &node)?;
    let platform = target_platform(storage, &node)?;
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
    let root_document = documents
        .iter()
        .find(|document| {
            document.selection.module_id == service_id
                && document.selection.release.version == resolved.plan.root.version
        })
        .ok_or_else(|| {
            StoreApiError::new(
                500,
                "CATALOG_PLAN_INVALID",
                "resolved validation plan does not contain its requested root metadata",
            )
        })?;
    let composition_plan = build_store_composition_plan(storage, &documents, service_id, &node)?;
    let contract = release_contract_from_document(root_document)?;
    let composition_validation = validate_store_composition_inputs(
        &composition_plan,
        &composition_plan.plan_digest,
        &composition_plan.release_graph_digest,
        &input.inputs,
        (!input.config.is_null()).then(|| input.config.clone()),
        input.secret_refs.clone(),
    );
    let composition_validation = if contract.platform.is_none() {
        legacy_composition_inputs(&composition_plan, &input.config, &input.secret_refs)
    } else {
        composition_validation
    };
    let composition_error_detail = composition_validation
        .as_ref()
        .err()
        .map(|error| error.detail.clone());
    let runtime_contract = ensure_release_runtime_supported(
        storage,
        &node,
        &contract,
        root_document.selection.release.oci_image.as_str(),
    )?;
    let runtime_facts = node_runtime_facts(storage, &node.node_id)?;
    let planned_deployment_id =
        deployment_id(service_id, &resolved.plan.root.version, &node.node_id);
    let effective_endpoint = effective_managed_endpoint(&input.endpoint, &node, &contract.release)?;
    let (bindings_resolvable, requirements) = preview_install_api_bindings(
        console,
        storage,
        &contract,
        &node.node_id,
        &effective_endpoint,
        &input.bindings,
        input.topology.as_ref(),
    )?;
    let topology_confirmation_required =
        !contract.requirements().is_empty() && input.topology.is_none();
    let bindings_valid = bindings_resolvable && !topology_confirmation_required;
    let binding_plan = if bindings_valid {
        resolve_install_api_bindings(
            console,
            storage,
            &contract,
            &planned_deployment_id,
            &node.node_id,
            &effective_endpoint,
            &input.bindings,
            input.topology.as_ref(),
            false,
        )?
    } else {
        Vec::new()
    };
    if contract.contract_version >= 2
        && (contract.platform.is_none() || composition_validation.is_ok())
    {
        let image = OciImageReference::parse(root_document.selection.release.oci_image.as_str())
            .map_err(|error| {
                StoreApiError::new(
                    422,
                    "STORE_IMMUTABLE_IMAGE_REQUIRED",
                    format!("validation release image is not immutable: {error}"),
                )
            })?;
        let mut preview_spec = container_spec(
            &planned_deployment_id,
            service_id,
            &resolved.plan.root.version,
            &root_document.checksum,
            &node,
            image,
            runtime_contract.clone(),
            &contract.release,
            managed_published_endpoint(&effective_endpoint, service_id, &node, &contract.release)?,
        );
        preview_spec.labels.insert(
            "ojos.service_contract_version".to_string(),
            contract.contract_version.to_string(),
        );
        attach_release_runtime_volume(&mut preview_spec, &contract)?;
        if bindings_valid
            && (!contract.requirements().is_empty()
                || !contract.events.publishes.is_empty()
                || !contract.events.subscribes.is_empty()
                || contract_has_retained_runtime_volume(&contract))
        {
            preview_spec.managed_service_context = managed_service_context_spec(
                storage,
                &contract,
                &node.node_id,
                &binding_plan,
                true,
            )?;
        }
        let preview_health_gate =
            HealthGatePolicy::for_runtime_contract(&preview_spec.runtime_contract);
        let preview_install = RuntimeInstallPayload {
            spec: preview_spec,
            start: input.start,
            health_gate: preview_health_gate,
            offline_oci_artifact: None,
        };
        let (validated_config, validated_secret_refs) = composition_validation
            .as_ref()
            .ok()
            .map(|validated| {
                composition_inputs_for_service(&composition_plan, validated, service_id)
            })
            .unwrap_or_else(|| (input.config.clone(), input.secret_refs.clone()));
        let _validated_pipeline = release_pipeline_payload(
            &contract.release,
            &contract,
            &preview_install,
            &binding_plan,
            &node,
            &format!("store-validate-{request_id}"),
            &input.migration_policy,
            &input.gateway_node_id,
            &validated_config,
            &validated_secret_refs,
        )?;
    }
    let topology_diff = if bindings_valid {
        input
            .topology
            .as_ref()
            .map(|selection| {
                let (current, _) = selected_topology_spec(storage, selection)?;
                let proposed = preview_store_install_topology_spec(
                    current.clone(),
                    &contract,
                    &planned_deployment_id,
                    &node.node_id,
                    &effective_endpoint,
                    &binding_plan,
                )?;
                diff_topology_specs(Some(&current), &proposed).map_err(core_error)
            })
            .transpose()?
    } else {
        None
    };
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
            "valid": bindings_valid && (contract.platform.is_none() || composition_validation.is_ok()),
            "catalog_source_id": resolved.source_id,
            "catalog_id": resolved.catalog_id,
            "verified_key_ids": resolved.verified_key_ids,
            "target_platform": platform,
            "plan": resolved.plan,
            "metadata": metadata,
            "bindings": binding_plan,
            "requirements": requirements,
            "composition_plan": composition_plan,
            "composition_inputs_valid": composition_validation.is_ok(),
            "composition_input_error": composition_error_detail,
            "topology_confirmation_required": topology_confirmation_required,
            "runtime": {
                "node_id": node.node_id,
                "contract": runtime_contract,
                "facts": runtime_facts,
            },
            "topology": input.topology.as_ref().map(|selection| json!({
                "topology_id": selection.topology_id,
                "revision_id": selection.revision_id,
            })),
            "topology_diff": topology_diff,
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
    #[serde(default)]
    plan_digest: String,
    #[serde(default)]
    release_graph_digest: String,
    /// Per Composition node inputs. Legacy root `config` and `secret_refs`
    /// remain aliases for one release cycle.
    #[serde(default)]
    inputs: BTreeMap<String, BTreeMap<String, Value>>,
    #[serde(default)]
    bindings: Vec<InstallBindingSelection>,
    #[serde(default)]
    topology_id: String,
    #[serde(default)]
    topology_etag: String,
    #[serde(default)]
    topology: Option<InstallTopologySelection>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct InstallBindingSelection {
    name: String,
    provider_deployment_id: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct InstallTopologySelection {
    pub(crate) topology_id: String,
    #[serde(default)]
    pub(crate) revision_id: String,
}

fn normalize_store_topology_selection(
    topology_id: &str,
    topology_etag: &str,
    compatibility: Option<&InstallTopologySelection>,
) -> Result<Option<InstallTopologySelection>, StoreApiError> {
    let topology_id = topology_id.trim();
    let topology_etag = topology_etag.trim();
    if topology_id.is_empty() && topology_etag.is_empty() {
        let Some(compatibility) = compatibility else {
            return Ok(None);
        };
        let compatibility_id = required_text(&compatibility.topology_id, "topology.topology_id")?;
        let compatibility_revision =
            required_text(&compatibility.revision_id, "topology.revision_id")?;
        return Ok(Some(InstallTopologySelection {
            topology_id: compatibility_id.to_string(),
            revision_id: compatibility_revision.to_string(),
        }));
    }
    if topology_id.is_empty() || topology_etag.is_empty() {
        return Err(StoreApiError::new(
            422,
            "STORE_TOPOLOGY_CONCURRENCY_REQUIRED",
            "topology_id and topology_etag must be supplied together",
        ));
    }
    let revision_id = strong_topology_etag(topology_etag)?;
    if let Some(compatibility) = compatibility
        && (compatibility.topology_id.trim() != topology_id
            || (!compatibility.revision_id.trim().is_empty()
                && compatibility.revision_id.trim() != revision_id))
    {
        return Err(StoreApiError::new(
            409,
            "STORE_TOPOLOGY_INPUT_CONFLICT",
            "explicit topology_id/topology_etag conflicts with compatibility topology input",
        ));
    }
    Ok(Some(InstallTopologySelection {
        topology_id: topology_id.to_string(),
        revision_id: revision_id.to_string(),
    }))
}

fn strong_topology_etag(value: &str) -> Result<&str, StoreApiError> {
    value
        .trim()
        .strip_prefix('"')
        .and_then(|value| value.strip_suffix('"'))
        .filter(|value| !value.is_empty() && !value.contains('"'))
        .ok_or_else(|| {
            StoreApiError::new(
                422,
                "STORE_TOPOLOGY_ETAG_INVALID",
                "topology_etag must be a strong quoted revision ETag",
            )
        })
}

fn normalize_replacement_topologies(
    single: Option<&InstallTopologySelection>,
    group: &[ReplacementTopologyCas],
) -> Result<Vec<InstallTopologySelection>, StoreApiError> {
    if group.is_empty() {
        return Ok(single.into_iter().cloned().collect());
    }
    if single.is_some() {
        return Err(StoreApiError::new(
            409,
            "STORE_TOPOLOGY_INPUT_CONFLICT",
            "use either topology_id/topology_etag (or compatibility topology) or topologies, not both",
        ));
    }
    let mut selections = Vec::with_capacity(group.len());
    let mut seen = BTreeSet::new();
    for entry in group {
        let topology_id = required_text(&entry.topology_id, "topologies[].topology_id")?;
        let revision_id = strong_topology_etag(&entry.topology_etag)?;
        if !seen.insert(topology_id.to_string()) {
            return Err(StoreApiError::new(
                422,
                "STORE_REPLACEMENT_TOPOLOGY_DUPLICATE",
                format!("topology {topology_id} appears more than once"),
            ));
        }
        selections.push(InstallTopologySelection {
            topology_id: topology_id.to_string(),
            revision_id: revision_id.to_string(),
        });
    }
    selections.sort_by(|left, right| left.topology_id.cmp(&right.topology_id));
    Ok(selections)
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
    let mut input: InstallReleaseRequest = parse_body(request)?;
    input.topology = normalize_store_topology_selection(
        &input.topology_id,
        &input.topology_etag,
        input.topology.as_ref(),
    )?;
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
        ensure_ready_docker_node(storage, node.as_ref().expect("managed node was required"))?;
    }
    let platform = node
        .as_ref()
        .map(|node| target_platform(storage, node))
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
    let composition_node = node.as_ref().ok_or_else(|| {
        StoreApiError::new(
            422,
            "STORE_TARGET_NODE_REQUIRED",
            "CompositionPlanV1 requires a target Node provider snapshot",
        )
    })?;
    let composition_plan =
        build_store_composition_plan(storage, &documents, &service_id, composition_node)?;
    let has_platform_contract = documents
        .iter()
        .map(release_contract_from_document)
        .collect::<Result<Vec<_>, _>>()?
        .iter()
        .any(|contract| contract.platform.is_some());
    let supplied_plan_digest = if input.plan_digest.trim().is_empty() && !has_platform_contract {
        composition_plan.plan_digest.as_str()
    } else {
        required_text(&input.plan_digest, "plan_digest")?
    };
    let supplied_graph_digest =
        if input.release_graph_digest.trim().is_empty() && !has_platform_contract {
            composition_plan.release_graph_digest.as_str()
        } else {
            required_text(&input.release_graph_digest, "release_graph_digest")?
        };
    let validated_composition = if has_platform_contract {
        validate_store_composition_inputs(
            &composition_plan,
            supplied_plan_digest,
            supplied_graph_digest,
            &input.inputs,
            (!input.config.is_null()).then(|| input.config.clone()),
            input.secret_refs.clone(),
        )?
    } else {
        legacy_composition_inputs(&composition_plan, &input.config, &input.secret_refs)?
    };
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
        ensure_ready_docker_node(storage, dependency_node)?;
    }
    if let Some(node) = node.as_ref() {
        for document in &documents {
            let managed_on_target = !external
                || external_missing_dependencies.iter().any(|dependency| {
                    dependency.module_id == document.selection.module_id
                        && dependency.release.version == document.selection.release.version
                });
            if managed_on_target {
                let contract = release_contract_from_document(document)?;
                ensure_release_runtime_supported(
                    storage,
                    node,
                    &contract,
                    document.selection.release.oci_image.as_str(),
                )?;
            }
        }
    }

    // A Managed install must prove its complete outbound contract before even
    // publishing the release metadata. Legacy required_apis are normalized to
    // stable names by ServiceReleaseContract, but they do not receive a
    // PENDING compatibility escape hatch: the exact healthy provider must
    // already be represented by the selected applied Topology.
    let managed_binding_preflight = if external {
        None
    } else {
        let root_document = documents
            .iter()
            .find(|document| {
                document.selection.module_id == service_id
                    && document.selection.release.version == resolved.plan.root.version
            })
            .ok_or_else(|| {
                StoreApiError::new(
                    500,
                    "CATALOG_PLAN_INVALID",
                    "resolved install plan does not contain its requested root metadata",
                )
            })?;
        let contract = release_contract_from_document(root_document)?;
        let node = node
            .as_ref()
            .expect("managed install requires a target Node");
        input.endpoint = effective_managed_endpoint(&input.endpoint, node, &contract.release)?;
        let deployment_id = deployment_id(&service_id, &resolved.plan.root.version, &node.node_id);
        let bindings = resolve_install_api_bindings(
            console,
            storage,
            &contract,
            &deployment_id,
            &node.node_id,
            &input.endpoint,
            &input.bindings,
            input.topology.as_ref(),
            false,
        )?;
        ensure_managed_api_bindings_ready(storage, &contract, &bindings, input.topology.as_ref())?;
        Some((deployment_id, bindings))
    };

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
    let selected = select_catalog_document_release(
        console,
        &documents,
        &service_id,
        &resolved.plan.root.version,
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
            &composition_plan,
            &validated_composition,
        );
    }
    let node = node.expect("managed install requires a target Node");
    let (root_deployment_id, mut binding_plan) = managed_binding_preflight
        .expect("managed binding preflight must run before release publication");
    let operation_id = operation_id("store-install", &root_deployment_id, request)?;
    let staged_contribution = stage_release_contribution(
        storage,
        &operation_id,
        &root_deployment_id,
        &selected.contract,
        root_release.release.oci_image.digest().as_str(),
    )?;
    if staged_contribution.is_some() && !input.start {
        return Err(StoreApiError::new(
            422,
            "STORE_CONTRIBUTION_START_REQUIRED",
            "a release with an active Contribution must start and pass its runtime health gate before routes, permissions, or frontend modules can be activated",
        ));
    }
    let topology_apply = input
        .topology
        .as_ref()
        .map(|selection| {
            propose_store_install_topology(
                storage,
                selection,
                &selected.contract,
                &root_deployment_id,
                &node.node_id,
                &input.endpoint,
                &binding_plan,
                &operation_id,
                None,
            )
        })
        .transpose()?;
    if let Some(topology) = &topology_apply {
        binding_plan = production_binding_plan(topology.staged_bindings.iter().filter(|binding| {
            binding.consumer_deployment_id == root_deployment_id
                && binding.desired_state == "ACTIVE"
        }));
    }
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
    if let Some(topology) = &topology_apply {
        jobs.push(PlannedJob {
            step_id: "topology-binding-prepare".to_string(),
            node_id: CONTROL_PLANE_NODE_ID.to_string(),
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
    let mut planned_deployments = Vec::new();
    let mut root_spec = None;
    for selection in &missing {
        let release = select_catalog_document_release(
            console,
            &documents,
            &selection.module_id,
            &selection.release.version,
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
        let mut spec = container_spec(
            &release_deployment_id,
            &selection.module_id,
            &selection.release.version,
            &release.record.checksum,
            &node,
            release_image,
            release_runtime_contract(&release.contract)?,
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
        spec.labels.insert(
            "ojos.service_contract_version".to_string(),
            release.contract.contract_version.to_string(),
        );
        attach_release_runtime_volume(&mut spec, &release.contract)?;
        let deployment_bindings = if selection.module_id == service_id {
            binding_plan.as_slice()
        } else {
            &[]
        };
        if selection.module_id == service_id
            || !release.contract.events.publishes.is_empty()
            || !release.contract.events.subscribes.is_empty()
            || contract_has_retained_runtime_volume(&release.contract)
        {
            spec.managed_service_context = managed_service_context_spec(
                storage,
                &release.contract,
                &node.node_id,
                deployment_bindings,
                true,
            )?;
        }
        if selection.module_id == service_id {
            root_spec = Some(spec.clone());
        }
        let health_gate = HealthGatePolicy::for_runtime_contract(&spec.runtime_contract);
        let runtime_install = RuntimeInstallPayload {
            spec,
            start: if selection.module_id == service_id {
                input.start
            } else {
                true
            },
            health_gate,
            offline_oci_artifact: offline_artifact_for_release(
                storage,
                artifact_store,
                &documents,
                &selection.module_id,
                &selection.release.version,
            )?,
        };
        let (release_config, release_secret_refs) = composition_inputs_for_service(
            &composition_plan,
            &validated_composition,
            &selection.module_id,
        );
        let pipeline = release_pipeline_payload(
            &release.manifest,
            &release.contract,
            &runtime_install,
            deployment_bindings,
            &node,
            &operation_id,
            &input.migration_policy,
            &input.gateway_node_id,
            &release_config,
            &release_secret_refs,
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
        let mut depends_on: Vec<String> = selection
            .release
            .dependencies
            .iter()
            .filter_map(|dependency| install_steps.get(&dependency.module_id).cloned())
            .collect();
        if selection.module_id == service_id {
            if topology_apply.is_some() {
                depends_on.push("topology-binding-prepare".to_string());
            }
            if let Some(contribution) = &staged_contribution {
                // The contribution fragment is appended after runtime jobs;
                // its deterministic PREPARE id is safe to reference now.
                depends_on.push(contribution_job_steps(contribution).prepare_step_id);
            }
        }
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
    if let Some(topology) = &topology_apply {
        let root_step = install_steps.get(&service_id).cloned().ok_or_else(|| {
            StoreApiError::new(500, "CATALOG_PLAN_INVALID", "root install step is missing")
        })?;
        append_install_topology_jobs(
            &mut jobs,
            topology,
            &root_step,
            &node.node_id,
            &root_deployment_id,
        );
    }
    if let Some(contribution) = &staged_contribution {
        let root_step = install_steps.get(&service_id).cloned().ok_or_else(|| {
            StoreApiError::new(500, "CATALOG_PLAN_INVALID", "root install step is missing")
        })?;
        let prepare_dependencies = topology_apply
            .as_ref()
            .map(|_| vec!["topology-binding-prepare".to_string()])
            .unwrap_or_default();
        let finalize_step = topology_apply
            .as_ref()
            .map(|_| "topology-binding-finalize-success".to_string());
        let commit_dependencies = vec![root_step];
        let contribution_steps = append_contribution_job_fragment(
            &mut jobs,
            contribution,
            prepare_dependencies,
            commit_dependencies,
            finalize_step.clone().into_iter().collect(),
        );
        if let Some(finalize_step) = finalize_step {
            add_job_dependency(
                &mut jobs,
                &finalize_step,
                &contribution_steps.commit_step_id,
            )?;
            // A failed Contribution COMMIT leaves FINALIZE intentionally
            // unmaterialized. Topology ABORT therefore needs the COMMIT as a
            // direct failure witness; depending only on the unbound FINALIZE
            // node would strand the PREPARE projection forever.
            add_job_dependency(
                &mut jobs,
                "topology-binding-finalize-failure",
                &contribution_steps.commit_step_id,
            )?;
            // Runtime cleanup is safe only after both projections have restored
            // their previous active state.  Otherwise topology ABORT and
            // Contribution ABORT can race with container removal.
            add_job_dependency(
                &mut jobs,
                "remove-root-after-topology-abort",
                &contribution_steps.abort_step_id,
            )?;
        }
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
            "composition_plan_digest": composition_plan.plan_digest,
            "composition_release_graph_digest": composition_plan.release_graph_digest,
            "composition_inputs": validated_composition,
            "bindings": binding_plan,
            "topology": input.topology.as_ref().map(|selection| json!({
                "topology_id": selection.topology_id,
                "selected_revision_id": selection.revision_id,
                "proposed_revision_id": topology_apply.as_ref().map(|topology| topology.revision_id.as_str()),
            })),
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
    if let Some(topology) = &topology_apply {
        storage
            .begin_topology_apply(
                &topology.topology_id,
                &topology.revision_id,
                &operation_id,
                &now_marker(),
            )
            .map_err(storage_error)?;
    }
    let operation = match enqueue_plan(storage, plan) {
        Ok(operation) => operation,
        Err(error) => {
            if let Some(topology) = &topology_apply {
                let _ = storage.finish_topology_apply(
                    &topology.topology_id,
                    &topology.revision_id,
                    &operation_id,
                    orchestrator_storage::TopologyApplyOutcome::Failed,
                    &now_marker(),
                );
            }
            return Err(error);
        }
    };
    Ok(success(
        202,
        json!({
            "operation_id": operation_id,
            "operation": operation,
            "deployment_id": root_deployment_id,
            "bindings": binding_plan,
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

fn append_install_topology_jobs(
    jobs: &mut Vec<PlannedJob>,
    topology: &StoreTopologyApplyPlan,
    root_step: &str,
    node_id: &str,
    root_deployment_id: &str,
) {
    jobs.push(PlannedJob {
        step_id: "topology-binding-finalize-success".to_string(),
        node_id: CONTROL_PLANE_NODE_ID.to_string(),
        kind: JobKind::TopologyApply,
        depends_on: vec![root_step.to_string()],
        condition: PlannedJobCondition::OnSuccess,
        payload: json!({
            "topology_id": topology.topology_id,
            "revision_id": topology.revision_id,
            "phase": "FINALIZE",
            "bindings": topology.staged_bindings,
            "previous_bindings": topology.previous_bindings,
        }),
        max_attempts: 1,
    });
    jobs.push(PlannedJob {
        step_id: "topology-binding-finalize-failure".to_string(),
        node_id: CONTROL_PLANE_NODE_ID.to_string(),
        kind: JobKind::TopologyApply,
        // ABORT is needed both when the root install fails and when FINALIZE
        // itself fails. An unbound successful branch is treated as skipped by
        // OnFailure, so this one dependency set covers both paths.
        depends_on: vec![
            root_step.to_string(),
            "topology-binding-finalize-success".to_string(),
        ],
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
    jobs.push(PlannedJob {
        step_id: "remove-root-after-topology-abort".to_string(),
        node_id: node_id.to_string(),
        kind: JobKind::Uninstall,
        depends_on: vec![
            root_step.to_string(),
            "topology-binding-finalize-failure".to_string(),
        ],
        condition: PlannedJobCondition::OnSuccess,
        payload: json!({
            "deployment_id": root_deployment_id,
            "container_id": stable_container_name(root_deployment_id),
            "force": true,
        }),
        max_attempts: 3,
    });
}

fn stage_release_contribution(
    storage: &DurableStore,
    operation_id: &str,
    deployment_id: &str,
    contract: &ServiceReleaseContract,
    runtime_digest: &str,
) -> Result<Option<crate::contribution_controller::StagedContributionV1>, StoreApiError> {
    let Some(platform) = contract.platform.as_ref() else {
        return Ok(None);
    };
    let contribution = &platform.contribution;
    let head = storage
        .contribution_head("default", &contract.release.service_name)
        .map_err(contribution_storage_error)?;
    if head.is_none()
        && contribution.api_surfaces.is_empty()
        && contribution.operation_routes.is_empty()
        && contribution.permission_definitions.is_empty()
        && contribution.user_frontend_modules.is_empty()
        && contribution.admin_frontend_modules.is_empty()
    {
        return Ok(None);
    }
    let generation = crate::contribution_controller::next_contribution_generation(
        storage,
        "default",
        &contract.release.service_name,
        head.as_ref().map_or(0, |head| head.generation()),
    )
    .map_err(contribution_controller_error)?;
    let previous_revision_id = head
        .as_ref()
        .map(|head| head.active_revision_id().to_string());
    let revision = ContributionRevisionV1::stage(
        "default",
        deployment_id,
        contract.release.service_name.clone(),
        runtime_digest,
        platform.contract_digest.clone(),
        generation,
        previous_revision_id,
        contribution.api_surfaces.clone(),
        contribution.operation_routes.clone(),
        contribution.permission_definitions.clone(),
        contribution.user_frontend_modules.clone(),
        contribution.admin_frontend_modules.clone(),
    )
    .map_err(|error| {
        StoreApiError::new(
            422,
            "STORE_CONTRIBUTION_INVALID",
            format!("compile signed contribution revision: {error}"),
        )
    })?;
    stage_contribution(storage, operation_id, &revision)
        .map(Some)
        .map_err(contribution_controller_error)
}

fn stage_replacement_contribution(
    storage: &DurableStore,
    operation_id: &str,
    replaces_deployment_id: &str,
    deployment_id: &str,
    contract: &ServiceReleaseContract,
    runtime_digest: &str,
) -> Result<Option<crate::contribution_controller::StagedContributionV1>, StoreApiError> {
    let head = storage
        .contribution_head("default", &contract.release.service_name)
        .map_err(contribution_storage_error)?;
    let Some(head) = head else {
        return stage_release_contribution(
            storage,
            operation_id,
            deployment_id,
            contract,
            runtime_digest,
        );
    };
    let platform = contract.platform.as_ref().ok_or_else(|| {
        StoreApiError::new(
            422,
            "STORE_CONTRIBUTION_SUCCESSOR_REQUIRED",
            format!(
                "service {} has active Contribution head {}; a replacement release must carry a signed platform Contribution projection, including an explicit empty successor when withdrawing all contributions",
                contract.release.service_name,
                head.etag()
            ),
        )
    })?;
    let contribution = &platform.contribution;
    stage_signed_contribution_successor(
        storage,
        operation_id,
        SignedContributionSuccessorV1 {
            scope_id: "default".to_string(),
            replaces_deployment_id: replaces_deployment_id.to_string(),
            deployment_id: deployment_id.to_string(),
            service_id: contract.release.service_name.clone(),
            release_digest: runtime_digest.to_string(),
            contract_digest: platform.contract_digest.clone(),
            api_surfaces: contribution.api_surfaces.clone(),
            operation_routes: contribution.operation_routes.clone(),
            permission_definitions: contribution.permission_definitions.clone(),
            user_frontend_modules: contribution.user_frontend_modules.clone(),
            admin_frontend_modules: contribution.admin_frontend_modules.clone(),
        },
    )
    .map(Some)
    .map_err(contribution_controller_error)
}

fn add_job_dependency(
    jobs: &mut [PlannedJob],
    step_id: &str,
    dependency: &str,
) -> Result<(), StoreApiError> {
    let job = jobs
        .iter_mut()
        .find(|job| job.step_id == step_id)
        .ok_or_else(|| {
            StoreApiError::new(
                500,
                "STORE_CONTRIBUTION_DAG_INVALID",
                format!("Contribution integration could not find step {step_id}"),
            )
        })?;
    if !job.depends_on.iter().any(|value| value == dependency) {
        job.depends_on.push(dependency.to_string());
    }
    Ok(())
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
    composition_plan: &CompositionPlanV1,
    validated_composition: &ValidatedInstallInputsV1,
) -> Result<ApiResponse, StoreApiError> {
    let endpoint = input.endpoint.trim();
    if release_runtime_contract(&selected.contract)?.id == RuntimeProfile::JudgeSandboxV1 {
        return Err(StoreApiError::new(
            422,
            "STORE_EXTERNAL_RUNTIME_PROFILE_FORBIDDEN",
            "judge-sandbox-v1 requires a Managed Agent assignment so runtime policy, HostConfig attestation, context materialization, and compensation remain provable",
        ));
    }
    if !selected.contract.events.publishes.is_empty()
        || !selected.contract.events.subscribes.is_empty()
    {
        return Err(StoreApiError::new(
            422,
            "STORE_EXTERNAL_EVENT_CONTEXT_REQUIRED",
            "Event Contract v2 requires an Agent-materialized event context; install this release as Managed",
        ));
    }
    let root_external_deployment_id = deployment_id(
        service_id,
        &selected.version,
        &format!("external:{endpoint}"),
    );
    let consumer_node_id = node.map(|node| node.node_id.as_str()).unwrap_or("external");
    let binding_plan = resolve_install_api_bindings(
        console,
        storage,
        &selected.contract,
        &root_external_deployment_id,
        consumer_node_id,
        endpoint,
        &input.bindings,
        input.topology.as_ref(),
        false,
    )?;
    let operation_id = operation_id(
        "store-external-install",
        &root_external_deployment_id,
        request,
    )?;
    if binding_plan.iter().any(|binding| {
        matches!(
            binding.state,
            ApiBindingState::Resolved | ApiBindingState::Active
        )
    }) {
        return Err(StoreApiError::new(
            422,
            "STORE_EXTERNAL_BINDING_CONTEXT_REQUIRED",
            "External consumers cannot receive an Agent-materialized workload context; install this release as Managed or remove its API requirements",
        ));
    }
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
        for selection in missing_dependencies {
            let release = select_catalog_document_release(
                console,
                documents,
                &selection.module_id,
                &selection.release.version,
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
            let mut spec = container_spec(
                &dependency_deployment_id,
                &selection.module_id,
                &selection.release.version,
                &release.record.checksum,
                node,
                release_image,
                release_runtime_contract(&release.contract)?,
                &release.manifest,
                None,
            );
            spec.labels.insert(
                "ojos.service_contract_version".to_string(),
                release.contract.contract_version.to_string(),
            );
            attach_release_runtime_volume(&mut spec, &release.contract)?;
            if !release.contract.events.publishes.is_empty()
                || !release.contract.events.subscribes.is_empty()
                || contract_has_retained_runtime_volume(&release.contract)
            {
                spec.managed_service_context = managed_service_context_spec(
                    storage,
                    &release.contract,
                    &node.node_id,
                    &[],
                    true,
                )?;
            }
            let health_gate = HealthGatePolicy::for_runtime_contract(&spec.runtime_contract);
            let runtime_install = RuntimeInstallPayload {
                spec,
                start: true,
                health_gate,
                offline_oci_artifact: offline_artifact_for_release(
                    storage,
                    artifact_store,
                    documents,
                    &selection.module_id,
                    &selection.release.version,
                )?,
            };
            let (release_config, release_secret_refs) = composition_inputs_for_service(
                composition_plan,
                validated_composition,
                &selection.module_id,
            );
            let pipeline = release_pipeline_payload(
                &release.manifest,
                &release.contract,
                &runtime_install,
                &[],
                node,
                &operation_id,
                &input.migration_policy,
                &input.gateway_node_id,
                &release_config,
                &release_secret_refs,
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
            "bindings": binding_plan,
            "topology": input.topology.as_ref().map(|selection| json!({
                "topology_id": selection.topology_id,
                "revision_id": selection.revision_id,
            })),
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
            "bindings": binding_plan,
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
    #[serde(default)]
    bindings: Vec<InstallBindingSelection>,
    #[serde(default)]
    topology_id: String,
    #[serde(default)]
    topology_etag: String,
    /// Strong-CAS inputs for provider replacements referenced by more than
    /// one applied topology. Entries are normalized and processed in
    /// topology-id order so the resulting Operation plan is deterministic.
    #[serde(default)]
    topologies: Vec<ReplacementTopologyCas>,
    #[serde(default)]
    topology: Option<InstallTopologySelection>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct ReplacementTopologyCas {
    topology_id: String,
    topology_etag: String,
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
    let mut input: ReplaceReleaseRequest = parse_body(request)?;
    input.topology = normalize_store_topology_selection(
        &input.topology_id,
        &input.topology_etag,
        input.topology.as_ref(),
    )?;
    let replacement_topologies =
        normalize_replacement_topologies(input.topology.as_ref(), &input.topologies)?;
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
    ensure_ready_docker_node(storage, &node)?;
    let platform = target_platform(storage, &node)?;
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
    for document in &documents {
        let deployed_by_operation = document.selection.module_id == current.instance.service_id
            || missing_dependencies.iter().any(|dependency| {
                dependency.module_id == document.selection.module_id
                    && dependency.release.version == document.selection.release.version
            });
        if deployed_by_operation {
            ensure_release_runtime_supported(
                storage,
                &node,
                &release_contract_from_document(document)?,
                document.selection.release.oci_image.as_str(),
            )?;
        }
    }
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
    let selected = select_catalog_document_release(
        console,
        &documents,
        &current.instance.service_id,
        &root_release.release.version,
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
    let operation_target = format!("{}->{}", current.instance.deployment_id, new_deployment_id);
    let operation_id = operation_id(action.operation_prefix(), &operation_target, request)?;
    let staged_contribution = stage_replacement_contribution(
        storage,
        &operation_id,
        &current.instance.deployment_id,
        &new_deployment_id,
        &selected.contract,
        root_release.release.oci_image.digest().as_str(),
    )?;
    let requested_replacement_endpoint = if input.endpoint.trim().is_empty() {
        current.endpoint.as_str()
    } else {
        input.endpoint.trim()
    };
    let replacement_endpoint = if !current.endpoint.trim().is_empty()
        && endpoint_socket(&current.endpoint) == endpoint_socket(requested_replacement_endpoint)
    {
        allocate_replacement_endpoint(
            storage,
            &current.endpoint,
            &current.instance.service_id,
            &new_deployment_id,
        )?
    } else {
        requested_replacement_endpoint.to_string()
    };
    let published_endpoint = managed_published_endpoint(
        &replacement_endpoint,
        &current.instance.service_id,
        &node,
        &selected.manifest,
    )?;
    let mut spec = container_spec(
        &new_deployment_id,
        &current.instance.service_id,
        &root_release.release.version,
        &selected.record.checksum,
        &node,
        image,
        release_runtime_contract(&selected.contract)?,
        &selected.manifest,
        published_endpoint,
    );
    spec.labels.insert(
        "ojos.service_contract_version".to_string(),
        selected.contract.contract_version.to_string(),
    );
    attach_release_runtime_volume(&mut spec, &selected.contract)?;
    let current_consumer_bindings = active_consumer_bindings(storage, current_deployment_id)?;
    let current_provider_bindings = active_provider_bindings(storage, current_deployment_id)?;
    let is_topology_consumer =
        !current_consumer_bindings.is_empty() || !selected.contract.requirements().is_empty();
    let is_topology_provider = !current_provider_bindings.is_empty();
    let mut replacement_bindings = Vec::new();
    let mut topology_applies = Vec::new();
    let mut topology_bootstrap_bindings = BTreeMap::<String, Vec<ApiBinding>>::new();
    let mut binding_context_transitions = Vec::new();
    if is_topology_consumer || is_topology_provider {
        let provider_consumers = current_provider_bindings
            .iter()
            .map(|binding| binding.consumer_deployment_id.clone())
            .collect::<BTreeSet<_>>();
        let mut replacement_scope_bindings = current_consumer_bindings.clone();
        replacement_scope_bindings.extend(current_provider_bindings.iter().cloned());
        for consumer in &provider_consumers {
            replacement_scope_bindings.extend(active_consumer_bindings(storage, consumer)?);
        }
        replacement_scope_bindings.sort_by(|left, right| left.binding_id.cmp(&right.binding_id));
        replacement_scope_bindings.dedup_by(|left, right| left.binding_id == right.binding_id);
        let topologies = require_matching_replacement_topologies(
            &replacement_topologies,
            &replacement_scope_bindings,
        )?;
        for topology in &topologies {
            selected_topology_spec(storage, topology)?;
        }

        let mut consumer_bindings_by_topology = BTreeMap::<String, Vec<ApiBinding>>::new();
        if is_topology_consumer {
            // Existing requirement/provider choices are the deterministic
            // defaults; explicit request mappings override them. This avoids
            // silently choosing another healthy provider during replacement.
            let mut effective_selections = current_consumer_bindings
                .iter()
                .map(|binding| {
                    (
                        binding.requirement_name.clone(),
                        binding.provider_deployment_id.clone(),
                    )
                })
                .collect::<BTreeMap<_, _>>();
            for binding in &input.bindings {
                effective_selections
                    .insert(binding.name.clone(), binding.provider_deployment_id.clone());
            }
            let effective_selections = effective_selections
                .into_iter()
                .map(|(name, provider_deployment_id)| InstallBindingSelection {
                    name,
                    provider_deployment_id,
                })
                .collect::<Vec<_>>();
            replacement_bindings = resolve_install_api_bindings(
                console,
                storage,
                &selected.contract,
                &new_deployment_id,
                &node.node_id,
                &replacement_endpoint,
                &effective_selections,
                None,
                true,
            )?;
            for binding in replacement_bindings.iter_mut().filter(|binding| {
                binding.desired_state == "ACTIVE"
                    && matches!(
                        binding.state,
                        ApiBindingState::Resolved | ApiBindingState::Active
                    )
            }) {
                let topology_id = current_consumer_bindings
                    .iter()
                    .find(|current| current.requirement_name == binding.requirement_name)
                    .map(|current| current.topology_id.clone())
                    .or_else(|| {
                        (topologies.len() == 1).then(|| topologies[0].topology_id.clone())
                    })
                    .ok_or_else(|| {
                        StoreApiError::new(
                            422,
                            "STORE_REPLACEMENT_BINDING_TOPOLOGY_AMBIGUOUS",
                            format!(
                                "new requirement {} needs an explicit topology, but replacement spans multiple topologies",
                                binding.requirement_name
                            ),
                        )
                    })?;
                binding.topology_id = topology_id.clone();
                binding.link_source_endpoint = replacement_endpoint.clone();
                consumer_bindings_by_topology
                    .entry(topology_id)
                    .or_default()
                    .push(binding.clone());
            }
        }

        for topology in topologies {
            let consumer_bindings = consumer_bindings_by_topology
                .get(&topology.topology_id)
                .map(Vec::as_slice)
                .unwrap_or(&[]);
            let provider_affected = current_provider_bindings
                .iter()
                .any(|binding| binding.topology_id == topology.topology_id);
            let planned = match (!consumer_bindings.is_empty(), provider_affected) {
                (true, true) => propose_dual_role_replacement_topology(
                    storage,
                    topology,
                    &selected.contract,
                    current_deployment_id,
                    &new_deployment_id,
                    &node.node_id,
                    &replacement_endpoint,
                    consumer_bindings,
                    &operation_id,
                )?,
                (true, false) => propose_store_install_topology(
                    storage,
                    topology,
                    &selected.contract,
                    &new_deployment_id,
                    &node.node_id,
                    &replacement_endpoint,
                    consumer_bindings,
                    &operation_id,
                    Some(current_deployment_id),
                )?,
                (false, true) => propose_provider_replacement_topology(
                    storage,
                    topology,
                    &selected.contract,
                    current_deployment_id,
                    &new_deployment_id,
                    &node.node_id,
                    &replacement_endpoint,
                    &operation_id,
                )?,
                (false, false) => {
                    propose_generation_sibling_topology(storage, topology, &operation_id)?
                }
            };
            if !consumer_bindings.is_empty() && provider_affected {
                let bootstrap = stage_replacement_consumer_bootstrap(
                    storage,
                    &planned,
                    current_deployment_id,
                    &new_deployment_id,
                    &replacement_endpoint,
                    consumer_bindings,
                    &operation_id,
                )?;
                topology_bootstrap_bindings.insert(planned.topology_id.clone(), bootstrap);
            }
            topology_applies.push(planned);
        }
        let mut generation_consumers = provider_consumers.clone();
        if is_topology_consumer {
            generation_consumers.insert(new_deployment_id.clone());
        }
        align_group_binding_generations(storage, &mut topology_applies, &generation_consumers)?;
        if is_topology_consumer {
            replacement_bindings = production_binding_plan(
                topology_applies
                    .iter()
                    .flat_map(|topology| topology.staged_bindings.iter())
                    .filter(|binding| {
                        binding.consumer_deployment_id == new_deployment_id
                            && binding.desired_state == "ACTIVE"
                    }),
            );
            replacement_bindings
                .sort_by(|left, right| left.requirement_name.cmp(&right.requirement_name));
        }
        let existing_context_consumers = provider_consumers
            .into_iter()
            .filter(|consumer| consumer != current_deployment_id)
            .collect::<BTreeSet<_>>();
        if !existing_context_consumers.is_empty() {
            binding_context_transitions = binding_context_transition_plans(
                storage,
                &topology_applies,
                &existing_context_consumers,
            )?;
        }
    } else if !replacement_topologies.is_empty() {
        return Err(StoreApiError::new(
            422,
            "STORE_REPLACEMENT_TOPOLOGY_UNUSED",
            "replacement supplied topology concurrency fields but the deployment has no API Binding role",
        ));
    }
    if is_topology_consumer
        || !selected.contract.events.publishes.is_empty()
        || !selected.contract.events.subscribes.is_empty()
        || contract_has_retained_runtime_volume(&selected.contract)
    {
        spec.managed_service_context = managed_service_context_spec(
            storage,
            &selected.contract,
            &node.node_id,
            &replacement_bindings,
            true,
        )?;
    }
    let replacement_health_gate = HealthGatePolicy::for_runtime_contract(&spec.runtime_contract);
    let replacement_install = RuntimeInstallPayload {
        spec: spec.clone(),
        start: true,
        health_gate: replacement_health_gate.clone(),
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
        &selected.contract,
        &replacement_install,
        &replacement_bindings,
        &node,
        &operation_id,
        &input.migration_policy,
        &input.gateway_node_id,
        &input.config,
        &input.secret_refs,
    )?;
    let previous_provider_revision =
        provider_revision_from_operation(storage, &current_proof.operation_id)?;
    let (
        materialization,
        resource_claims,
        migrations,
        desired_auth,
        desired_provisioners,
        desired_gateway,
    ) = desired_pipeline.map_or_else(
        || (None, Vec::new(), Vec::new(), None, Vec::new(), None),
        |pipeline| {
            (
                pipeline.materialization,
                pipeline.resource_claims,
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
        health_gate: replacement_health_gate,
        offline_oci_artifact: replacement_install.offline_oci_artifact,
        materialization,
        resource_claims,
        migrations,
        provider_saga,
        preserve_old_until_topology_cutover: !topology_applies.is_empty(),
        exclusive_retained_volume_cutover: spec.retained_volume.is_some(),
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
        let release = select_catalog_document_release(
            console,
            &documents,
            &dependency.module_id,
            &dependency.release.version,
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
        let mut dependency_spec = container_spec(
            &dependency_deployment_id,
            &dependency.module_id,
            &dependency.release.version,
            &release.record.checksum,
            &node,
            dependency_image,
            release_runtime_contract(&release.contract)?,
            &release.manifest,
            None,
        );
        dependency_spec.labels.insert(
            "ojos.service_contract_version".to_string(),
            release.contract.contract_version.to_string(),
        );
        attach_release_runtime_volume(&mut dependency_spec, &release.contract)?;
        if !release.contract.events.publishes.is_empty()
            || !release.contract.events.subscribes.is_empty()
            || contract_has_retained_runtime_volume(&release.contract)
        {
            dependency_spec.managed_service_context =
                managed_service_context_spec(storage, &release.contract, &node.node_id, &[], true)?;
        }
        let health_gate = HealthGatePolicy::for_runtime_contract(&dependency_spec.runtime_contract);
        let install = RuntimeInstallPayload {
            spec: dependency_spec,
            start: true,
            health_gate,
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
            &release.contract,
            &install,
            &[],
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
    let runtime_step = format!(
        "runtime-{}",
        match action {
            ReplacementAction::Upgrade => "upgrade",
            ReplacementAction::Rollback => "rollback",
        }
    );
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
    let context_apply_steps = binding_context_transitions
        .iter()
        .enumerate()
        .map(|(index, _)| format!("binding-context-apply-{index}"))
        .collect::<Vec<_>>();
    let context_health_steps = binding_context_transitions
        .iter()
        .enumerate()
        .map(|(index, _)| format!("binding-context-health-{index}"))
        .collect::<Vec<_>>();
    let mut bootstrap_steps = Vec::new();
    if is_topology_consumer {
        for (index, topology) in topology_applies.iter().enumerate() {
            if !topology.staged_bindings.iter().any(|binding| {
                binding.consumer_deployment_id == new_deployment_id
                    && binding.desired_state == "ACTIVE"
            }) {
                continue;
            }
            let step_id = if is_topology_provider {
                format!("topology-binding-bootstrap-{index}")
            } else {
                prepare_steps[index].clone()
            };
            let bindings = topology_bootstrap_bindings
                .get(&topology.topology_id)
                .unwrap_or(&topology.staged_bindings);
            jobs.push(PlannedJob {
                step_id: step_id.clone(),
                node_id: CONTROL_PLANE_NODE_ID.to_string(),
                kind: JobKind::TopologyApply,
                depends_on: dependency_steps.clone(),
                condition: PlannedJobCondition::OnSuccess,
                payload: json!({
                    "topology_id": topology.topology_id.clone(),
                    "revision_id": topology.revision_id,
                    "phase": "PREPARE",
                    "bindings": bindings,
                    "previous_bindings": topology.previous_bindings,
                }),
                max_attempts: 1,
            });
            bootstrap_steps.push(step_id);
        }
    }
    jobs.push(PlannedJob {
        step_id: runtime_step.clone(),
        node_id: node.node_id.clone(),
        kind: action.job_kind(),
        depends_on: if !bootstrap_steps.is_empty() {
            bootstrap_steps.clone()
        } else {
            dependency_steps
        },
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
    if !topology_applies.is_empty() {
        if is_topology_provider {
            for (index, topology) in topology_applies.iter().enumerate() {
                jobs.push(PlannedJob {
                    step_id: prepare_steps[index].clone(),
                    node_id: CONTROL_PLANE_NODE_ID.to_string(),
                    kind: JobKind::TopologyApply,
                    depends_on: vec![runtime_step.clone()],
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
        }
        for (index, transition) in binding_context_transitions.iter().enumerate() {
            jobs.push(PlannedJob {
                step_id: context_apply_steps[index].clone(),
                node_id: transition.node_id.clone(),
                kind: JobKind::BindingContextApply,
                depends_on: prepare_steps.clone(),
                condition: PlannedJobCondition::OnSuccess,
                payload: serde_json::to_value(&transition.forward).map_err(|error| {
                    StoreApiError::new(
                        500,
                        "STORE_BINDING_CONTEXT_INVALID",
                        format!("serialize binding context apply: {error}"),
                    )
                })?,
                max_attempts: 1,
            });
            jobs.push(PlannedJob {
                step_id: context_health_steps[index].clone(),
                node_id: transition.node_id.clone(),
                kind: JobKind::Health,
                depends_on: vec![context_apply_steps[index].clone()],
                condition: PlannedJobCondition::OnSuccess,
                payload: json!({"container_id": transition.container_id}),
                max_attempts: 3,
            });
        }
        let mut finalize_dependencies = vec![runtime_step.clone()];
        finalize_dependencies.extend(prepare_steps.iter().cloned());
        finalize_dependencies.extend(context_health_steps.iter().cloned());
        jobs.push(PlannedJob {
            step_id: finalize_step.clone(),
            node_id: CONTROL_PLANE_NODE_ID.to_string(),
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
        // A consumer-only replacement deliberately reuses its topology
        // PREPARE as the bootstrap gate. Build this compensation fan-in as a
        // set so the shared step cannot be emitted twice and make the durable
        // Operation graph invalid.
        let abort_dependencies = finalize_dependencies
            .into_iter()
            .chain(bootstrap_steps.iter().cloned())
            .chain(std::iter::once(finalize_step.clone()))
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();
        for (index, topology) in topology_applies.iter().enumerate() {
            jobs.push(PlannedJob {
                step_id: abort_steps[index].clone(),
                node_id: CONTROL_PLANE_NODE_ID.to_string(),
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
        for (index, transition) in binding_context_transitions.iter().enumerate() {
            let mut depends_on = abort_steps.clone();
            depends_on.push(context_apply_steps[index].clone());
            jobs.push(PlannedJob {
                step_id: format!("binding-context-rollback-{index}"),
                node_id: transition.node_id.clone(),
                kind: JobKind::BindingContextApply,
                depends_on,
                condition: PlannedJobCondition::OnSuccess,
                payload: serde_json::to_value(&transition.rollback).map_err(|error| {
                    StoreApiError::new(
                        500,
                        "STORE_BINDING_CONTEXT_INVALID",
                        format!("serialize binding context rollback: {error}"),
                    )
                })?,
                max_attempts: 1,
            });
        }
        jobs.push(PlannedJob {
            step_id: "remove-old-after-topology-cutover".to_string(),
            node_id: node.node_id.clone(),
            kind: JobKind::Uninstall,
            depends_on: vec![finalize_step.clone()],
            condition: PlannedJobCondition::OnSuccess,
            payload: json!({
                "deployment_id": current.instance.deployment_id,
                "container_id": current.instance.container_id,
                "force": false,
            }),
            max_attempts: 3,
        });
        jobs.push(PlannedJob {
            step_id: "remove-new-after-topology-abort".to_string(),
            node_id: node.node_id.clone(),
            kind: JobKind::Uninstall,
            depends_on: abort_steps.clone(),
            condition: PlannedJobCondition::OnSuccess,
            payload: json!({
                "deployment_id": new_deployment_id,
                "container_id": stable_container_name(&new_deployment_id),
                "force": true,
            }),
            max_attempts: 3,
        });
    }
    if let Some(contribution) = &staged_contribution {
        let mut commit_dependencies = prepare_steps.clone();
        commit_dependencies.extend(context_health_steps.iter().cloned());
        let has_topology = !topology_applies.is_empty();
        append_contribution_replacement_job_fragment(
            &mut jobs,
            contribution,
            ContributionReplacementDagV1 {
                prepare_depends_on: bootstrap_steps,
                runtime_step_id: runtime_step,
                commit_depends_on: commit_dependencies,
                topology_finalize_step_ids: has_topology
                    .then_some(finalize_step)
                    .into_iter()
                    .collect(),
                topology_abort_step_ids: abort_steps,
                success_cleanup_step_ids: has_topology
                    .then_some("remove-old-after-topology-cutover".to_string())
                    .into_iter()
                    .collect(),
                failure_cleanup_step_ids: has_topology
                    .then_some("remove-new-after-topology-abort".to_string())
                    .into_iter()
                    .collect(),
            },
        )
        .map_err(contribution_controller_error)?;
    }
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
            "bindings": replacement_bindings,
            "topologies": topology_applies.iter().map(|topology| {
                let selected_revision_id = replacement_topologies
                    .iter()
                    .find(|selection| selection.topology_id == topology.topology_id)
                    .map(|selection| selection.revision_id.as_str());
                json!({
                    "topology_id": topology.topology_id,
                    "selected_revision_id": selected_revision_id,
                    "proposed_revision_id": topology.revision_id,
                })
            }).collect::<Vec<_>>(),
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
    let mut begun_topologies: Vec<&StoreTopologyApplyPlan> = Vec::new();
    for topology in &topology_applies {
        if let Err(error) = storage.begin_topology_apply(
            &topology.topology_id,
            &topology.revision_id,
            &operation_id,
            &now_marker(),
        ) {
            for begun in begun_topologies.iter().rev() {
                let _ = storage.finish_topology_apply(
                    &begun.topology_id,
                    &begun.revision_id,
                    &operation_id,
                    orchestrator_storage::TopologyApplyOutcome::Failed,
                    &now_marker(),
                );
            }
            return Err(storage_error(error));
        }
        begun_topologies.push(topology);
    }
    let operation = match enqueue_plan(storage, plan) {
        Ok(operation) => operation,
        Err(error) => {
            for topology in begun_topologies.iter().rev() {
                let _ = storage.finish_topology_apply(
                    &topology.topology_id,
                    &topology.revision_id,
                    &operation_id,
                    orchestrator_storage::TopologyApplyOutcome::Failed,
                    &now_marker(),
                );
            }
            return Err(error);
        }
    };
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
    contract: ServiceReleaseContract,
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
        let contract =
            ServiceReleaseContract::from_json_value(record.manifest.clone()).map_err(|error| {
                StoreApiError::new(
                    422,
                    "STORE_RELEASE_INVALID",
                    format!(
                        "registered release {service_id}@{version} has an invalid manifest: {error}"
                    ),
                )
            })?;
        let manifest = contract.release.clone();
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
            contract,
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

/// Select a release for a trusted Catalog operation from the documents verified
/// for this exact request. New imports persist the full versioned contract, but
/// pre-migration v1 records can still exist and durable state must never replace
/// the current Catalog's signed runtime, event, Auth, or Gateway semantics.
fn select_catalog_document_release(
    console: &OrchestratorActionConsole,
    documents: &[VerifiedReleaseDocument],
    service_id: &str,
    version: &semver::Version,
) -> Result<SelectedRelease, StoreApiError> {
    let document = documents
        .iter()
        .find(|document| {
            document.selection.module_id == service_id
                && document.selection.release.version == *version
        })
        .ok_or_else(|| {
            StoreApiError::new(
                500,
                "CATALOG_PLAN_INVALID",
                format!(
                    "resolved Catalog plan has no verified metadata for {service_id}@{version}"
                ),
            )
        })?;
    let mut selected = select_release(console, service_id, Some(&version.to_string()))?;
    let contract = release_contract_from_document(document)?;
    if contract.release.service_name != service_id
        || contract.release.version != version.to_string()
    {
        return Err(StoreApiError::new(
            500,
            "CATALOG_PLAN_INVALID",
            format!(
                "verified metadata identity {}@{} does not match Catalog selection {service_id}@{version}",
                contract.release.service_name, contract.release.version
            ),
        ));
    }
    selected.manifest = contract.release.clone();
    selected.record.manifest = contract.to_json_value().map_err(core_error)?;
    selected.record.release_url = document.source_url.clone();
    selected.record.checksum = document.checksum.clone();
    selected.contract = contract;
    Ok(selected)
}

fn release_contract_from_document(
    document: &VerifiedReleaseDocument,
) -> Result<ServiceReleaseContract, StoreApiError> {
    let text = std::str::from_utf8(&document.bytes).map_err(|error| {
        StoreApiError::new(
            422,
            "STORE_RELEASE_INVALID",
            format!(
                "release {}@{} metadata is not UTF-8: {error}",
                document.selection.module_id, document.selection.release.version
            ),
        )
    })?;
    let contract = ServiceReleaseContract::from_yaml_str(text).map_err(|error| {
        StoreApiError::new(
            422,
            "STORE_RELEASE_INVALID",
            format!(
                "release {}@{} has an invalid Service Contract: {error}",
                document.selection.module_id, document.selection.release.version
            ),
        )
    })?;
    if contract.release.service_name != document.selection.module_id
        || contract.release.version != document.selection.release.version.to_string()
    {
        return Err(StoreApiError::new(
            409,
            "STORE_RELEASE_IDENTITY_MISMATCH",
            format!(
                "catalog release {}@{} does not match Service Contract {}@{}",
                document.selection.module_id,
                document.selection.release.version,
                contract.release.service_name,
                contract.release.version
            ),
        ));
    }
    Ok(contract)
}

fn build_store_composition_plan(
    storage: &DurableStore,
    documents: &[VerifiedReleaseDocument],
    root_service_id: &str,
    node: &NodeRecord,
) -> Result<CompositionPlanV1, StoreApiError> {
    let mut releases = Vec::with_capacity(documents.len());
    for document in documents {
        let contract = release_contract_from_document(document)?;
        let platform = contract.platform.as_ref();
        let mut package_dependencies = document
            .selection
            .release
            .dependencies
            .iter()
            .map(|dependency| PackageDependencyV1 {
                service_id: dependency.module_id.clone(),
                version_requirement: dependency.requirement.to_string(),
                development: false,
            })
            .collect::<Vec<_>>();
        if let Some(platform) = platform {
            package_dependencies.extend(platform.package_requirements.iter().map(|requirement| {
                PackageDependencyV1 {
                    service_id: requirement.service_id.clone(),
                    version_requirement: requirement.version_requirement.clone(),
                    development: requirement.development,
                }
            }));
        }
        package_dependencies.sort_by(|left, right| left.service_id.cmp(&right.service_id));
        package_dependencies.dedup_by(|left, right| left.service_id == right.service_id);

        let release_digest = contract
            .platform
            .as_ref()
            .map(|platform| platform.release_lock_digest.clone())
            .unwrap_or_else(|| document.checksum.clone());
        let owner_instance_id = stable_service_instance_id(&contract.release.service_name);
        let provided_apis = contract
            .provides
            .apis
            .iter()
            .map(|provided| {
                let version = contract
                    .release
                    .apis
                    .iter()
                    .find(|api| api.api_id == provided.api_id())
                    .and_then(|api| normalize_composition_version(&api.version))
                    .unwrap_or_else(|| document.selection.release.version.clone());
                ProvidedApiV1 {
                    api_id: provided.api_id().to_string(),
                    version,
                }
            })
            .collect();
        let required_apis = contract
            .requirements()
            .iter()
            .map(|requirement| CompositionApiRequirementV1 {
                name: requirement.binding_name().to_string(),
                api_id: requirement.api_id().to_string(),
                version_requirement: normalize_composition_requirement(
                    requirement.version_requirement(),
                ),
                optional: requirement.optional(),
                provider_policy: if requirement.selection() == "explicit" {
                    ProviderPolicyV1::Explicit
                } else {
                    ProviderPolicyV1::UniqueHealthy
                },
            })
            .collect();
        let resource_claims = platform
            .into_iter()
            .flat_map(|platform| platform.resource_claims.iter())
            .map(|resource| ResourceRequirementV1 {
                name: resource.name.clone(),
                resource_type: normalize_resource_capability(&resource.resource_type),
                version_requirement: "^1.0.0".to_string(),
                optional: false,
                provider_policy: ProviderPolicyV1::UniqueHealthy,
                lifecycle: ResourceLifecycleV1::Retain,
            })
            .collect();
        let config = platform
            .and_then(|platform| platform.config_schema.as_ref())
            .map(|config| ConfigRequirementV1 {
                schema: config.schema.clone(),
                required: true,
            });
        let mut secrets = contract
            .release
            .secrets
            .iter()
            .map(|name| SecretRequirementV1 {
                name: name.clone(),
                required: true,
            })
            .collect::<Vec<_>>();
        if let Some(config) = platform.and_then(|platform| platform.config_schema.as_ref()) {
            let mut schema_secrets = BTreeSet::new();
            collect_config_secret_paths(&config.schema, "", &mut schema_secrets)?;
            secrets.extend(schema_secrets.into_iter().map(|name| SecretRequirementV1 {
                // JSON Schema conditionals decide whether this secret is
                // required for the submitted config. Marking it optional here
                // lets validate return the whole input surface without forcing
                // mutually-exclusive conditional secrets.
                name,
                required: false,
            }));
        }
        secrets.sort_by(|left, right| left.name.cmp(&right.name));
        secrets.dedup_by(|left, right| left.name == right.name);
        releases.push(CompositionReleaseV1 {
            service_id: contract.release.service_name.clone(),
            owner_instance_id,
            version: document.selection.release.version.clone(),
            release_digest,
            package_dependencies,
            provided_apis,
            required_apis,
            resource_claims,
            config,
            secrets,
        });
    }
    releases.sort_by(|left, right| left.service_id.cmp(&right.service_id));
    let providers = store_composition_providers(storage, documents, node)?;
    build_composition_plan(
        ReleaseGraphV1 {
            schema_version: orchestrator_legacy::composition::RELEASE_GRAPH_SCHEMA_VERSION
                .to_string(),
            root_service_id: root_service_id.to_string(),
            releases,
        },
        &providers,
        CompositionModeV1::Production,
    )
    .map_err(composition_error)
}

fn store_composition_providers(
    storage: &DurableStore,
    documents: &[VerifiedReleaseDocument],
    node: &NodeRecord,
) -> Result<Vec<ProviderCandidateV1>, StoreApiError> {
    let mut providers = Vec::new();
    for document in documents {
        let contract = release_contract_from_document(document)?;
        for api in &contract.release.apis {
            let Some(version) = normalize_composition_version(&api.version) else {
                continue;
            };
            providers.push(ProviderCandidateV1 {
                provider_id: format!("package:{}:{}", contract.release.service_name, api.api_id),
                capability: api.api_id.clone(),
                version,
                kind: ProviderKindV1::Package,
                service_id: Some(contract.release.service_name.clone()),
            });
        }
    }
    let evidence_at_ms = now_ms();
    for stored in storage.runtime_instances(None).map_err(storage_error)? {
        let stored = storage
            .runtime_with_current_evidence(stored, evidence_at_ms)
            .map_err(storage_error)?;
        let managed_evidence_ready =
            if stored.management_mode == orchestrator_storage::RuntimeManagementMode::Managed {
                stored.instance.runtime_attested
                    && stored.drift_reason.is_empty()
                    && storage
                        .managed_runtime_report_unavailable_reason(&stored, evidence_at_ms)
                        .map_err(storage_error)?
                        .is_none()
            } else {
                stored.drift_reason.is_empty()
            };
        if stored.instance.observed_state != RuntimeObservedState::Running
            || !stored.instance.health.eq_ignore_ascii_case("HEALTHY")
            || !managed_evidence_ready
            || stored.endpoint.trim().is_empty()
        {
            continue;
        }
        // Runtime API providers are independent of package dependencies in
        // the release graph. Resolve their exact, already-registered Service
        // Contract from durable identity rather than requiring the provider's
        // release metadata to be repeated in this install's Catalog plan.
        let Some(contract) = storage
            .service_release_contract(
                &stored.instance.service_id,
                &stored.instance.release_version,
            )
            .map_err(storage_error)?
        else {
            continue;
        };
        if contract.release.service_name != stored.instance.service_id
            || contract.release.version != stored.instance.release_version
        {
            return Err(StoreApiError::new(
                409,
                "STORE_COMPOSITION_PROVIDER_RELEASE_MISMATCH",
                format!(
                    "provider deployment {} runtime identity {}@{} does not match its registered contract {}@{}",
                    stored.instance.deployment_id,
                    stored.instance.service_id,
                    stored.instance.release_version,
                    contract.release.service_name,
                    contract.release.version,
                ),
            ));
        }
        for api in &contract.release.apis {
            if let Some(version) = normalize_composition_version(&api.version) {
                providers.push(ProviderCandidateV1 {
                    provider_id: stored.instance.deployment_id.clone(),
                    capability: api.api_id.clone(),
                    version,
                    kind: match stored.management_mode {
                        orchestrator_storage::RuntimeManagementMode::Managed => {
                            ProviderKindV1::Managed
                        }
                        orchestrator_storage::RuntimeManagementMode::External => {
                            ProviderKindV1::External
                        }
                    },
                    service_id: Some(stored.instance.service_id.clone()),
                });
            }
        }
    }
    if let Some(Value::Object(postgresql)) = node_provider_label(node, "postgresql") {
        let enabled = postgresql
            .get("enabled")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let provider_id = postgresql
            .get("provider_id")
            .or_else(|| postgresql.get("connection_id"))
            .and_then(Value::as_str)
            .map(str::trim)
            .unwrap_or_default();
        if enabled && !provider_id.is_empty() {
            providers.push(ProviderCandidateV1 {
                provider_id: provider_id.to_string(),
                capability: "postgresql.database".to_string(),
                version: semver::Version::parse("1.0.0").expect("static semver"),
                kind: ProviderKindV1::Managed,
                service_id: None,
            });
        }
    }
    providers.sort_by(|left, right| {
        left.capability
            .cmp(&right.capability)
            .then(left.provider_id.cmp(&right.provider_id))
    });
    providers.dedup_by(|left, right| {
        left.capability == right.capability && left.provider_id == right.provider_id
    });
    Ok(providers)
}

fn validate_store_composition_inputs(
    plan: &CompositionPlanV1,
    plan_digest: &str,
    release_graph_digest: &str,
    inputs: &BTreeMap<String, BTreeMap<String, Value>>,
    config: Option<Value>,
    secret_refs: BTreeMap<String, String>,
) -> Result<ValidatedInstallInputsV1, StoreApiError> {
    validate_install_inputs(
        plan,
        &InstallInputsV1 {
            schema_version: INSTALL_INPUTS_SCHEMA_VERSION.to_string(),
            plan_digest: plan_digest.to_string(),
            release_graph_digest: release_graph_digest.to_string(),
            inputs: inputs.clone(),
            config,
            secret_refs,
        },
        &CompositionPlanBindingV1::from(plan),
    )
    .map_err(composition_error)
}

fn legacy_composition_inputs(
    plan: &CompositionPlanV1,
    config: &Value,
    secret_refs: &BTreeMap<String, String>,
) -> Result<ValidatedInstallInputsV1, StoreApiError> {
    let mut inputs = BTreeMap::new();
    if !config.is_null() || !secret_refs.is_empty() {
        // v1/v2 releases predate Composition nodes. Preserve their root
        // aliases in a synthetic private node so downstream Store code can
        // continue forwarding the exact signed release inputs while the
        // public plan remains truthful about having no v3 config contract.
        let mut values = BTreeMap::new();
        if !config.is_null() {
            values.insert("config".to_string(), config.clone());
        }
        for (name, reference) in secret_refs {
            values.insert(
                format!("secretRef.{name}"),
                Value::String(reference.clone()),
            );
        }
        inputs.insert("legacy-root-inputs".to_string(), values);
    }
    Ok(ValidatedInstallInputsV1 {
        schema_version: INSTALL_INPUTS_SCHEMA_VERSION.to_string(),
        plan_digest: plan.plan_digest.clone(),
        release_graph_digest: plan.release_graph_digest.clone(),
        inputs,
        normalized_legacy_aliases: !config.is_null() || !secret_refs.is_empty(),
    })
}

fn composition_inputs_for_service(
    plan: &CompositionPlanV1,
    validated: &ValidatedInstallInputsV1,
    service_id: &str,
) -> (Value, BTreeMap<String, String>) {
    let mut config = Value::Null;
    let mut secrets = BTreeMap::new();
    if service_id == plan.root_service_id
        && let Some(values) = validated.inputs.get("legacy-root-inputs")
    {
        if let Some(value) = values.get("config") {
            config = value.clone();
        }
        for (key, value) in values {
            if let Some(name) = key.strip_prefix("secretRef.")
                && let Some(reference) = value.as_str()
            {
                secrets.insert(name.to_string(), reference.to_string());
            }
        }
    }
    for node in plan
        .nodes
        .iter()
        .filter(|node| node.service_id == service_id)
    {
        let Some(values) = validated.inputs.get(&node.node_id) else {
            continue;
        };
        match &node.spec {
            CompositionNodeSpecV1::Config { .. } => {
                if let Some(value) = values.get("config") {
                    config = value.clone();
                }
            }
            CompositionNodeSpecV1::Secret { name, .. } => {
                if let Some(reference) = values.get("secretRef").and_then(Value::as_str) {
                    secrets.insert(name.clone(), reference.to_string());
                }
            }
            _ => {}
        }
    }
    (config, secrets)
}

fn normalize_composition_requirement(value: &str) -> String {
    let value = value.trim();
    if semver::VersionReq::parse(value).is_ok() {
        value.to_string()
    } else if let Some(version) = normalize_composition_version(value) {
        format!("={version}")
    } else {
        "*".to_string()
    }
}

fn normalize_composition_version(value: &str) -> Option<semver::Version> {
    let value = value.trim().strip_prefix('v').unwrap_or(value.trim());
    semver::Version::parse(value).ok().or_else(|| {
        value
            .parse::<u64>()
            .ok()
            .map(|major| semver::Version::new(major, 0, 0))
    })
}

fn normalize_resource_capability(value: &str) -> String {
    value.strip_suffix("/v1").unwrap_or(value).to_string()
}

fn composition_error(error: impl std::fmt::Display) -> StoreApiError {
    StoreApiError::new(422, "STORE_COMPOSITION_INVALID", error.to_string())
}

#[derive(Debug, Clone)]
struct TopologyBindingContext {
    topology_id: String,
    revision_id: String,
    source_endpoint: String,
    target_endpoint: String,
    api_id: String,
    version: String,
    optional: bool,
    provider_deployment_id: String,
    selection: String,
}

fn topology_contains_provider_candidate(
    spec: &TopologySpec,
    candidate: &ApiProviderCandidate,
) -> bool {
    spec.endpoints.iter().any(|endpoint| {
        endpoint.endpoint == candidate.endpoint
            && endpoint.service_id == candidate.service_id
            && endpoint
                .config
                .as_object()
                .and_then(|config| config.get("deployment_id"))
                .and_then(Value::as_str)
                .filter(|deployment_id| !deployment_id.trim().is_empty())
                .is_none_or(|deployment_id| deployment_id == candidate.deployment_id)
    })
}

fn preview_install_api_bindings(
    console: &OrchestratorActionConsole,
    storage: &DurableStore,
    contract: &ServiceReleaseContract,
    consumer_node_id: &str,
    requested_consumer_endpoint: &str,
    selections: &[InstallBindingSelection],
    topology: Option<&InstallTopologySelection>,
) -> Result<(bool, Vec<Value>), StoreApiError> {
    let mut selections_by_name = BTreeMap::new();
    for selection in selections {
        let name = required_text(&selection.name, "bindings[].name")?.to_string();
        let provider = required_text(
            &selection.provider_deployment_id,
            "bindings[].provider_deployment_id",
        )?
        .to_string();
        if selections_by_name.insert(name.clone(), provider).is_some() {
            return Err(StoreApiError::new(
                422,
                "STORE_BINDING_DUPLICATE",
                format!("binding selection {name} is declared more than once"),
            ));
        }
    }

    let (topology_spec, topology_revision_id) = topology
        .map(|selection| selected_topology_spec(storage, selection))
        .transpose()?
        .map_or((None, String::new()), |(spec, revision_id)| {
            (Some(spec), revision_id)
        });
    let consumer_endpoint = topology_spec
        .as_ref()
        .map(|spec| {
            topology_consumer_endpoint(
                spec,
                &contract.release.service_name,
                requested_consumer_endpoint,
            )
        })
        .transpose()?
        .unwrap_or_else(|| requested_consumer_endpoint.trim().to_string());
    let topology_bindings = topology_spec
        .as_ref()
        .map(|spec| topology_binding_contexts(spec, &topology_revision_id, &consumer_endpoint))
        .transpose()?
        .unwrap_or_default();
    let all_candidates = provider_candidates(console, storage)?;
    let mut requirements = Vec::with_capacity(contract.requirements().len());
    let mut valid = true;
    let mut used_selections = BTreeSet::new();
    let mut used_topology_bindings = BTreeSet::new();

    for requirement in contract.requirements() {
        let name = requirement.binding_name();
        let explicit_provider = selections_by_name.get(name).cloned().unwrap_or_default();
        if !explicit_provider.is_empty() {
            used_selections.insert(name.to_string());
        }
        let topology_binding = topology_bindings.get(name);
        if let Some(binding) = topology_binding {
            used_topology_bindings.insert(name.to_string());
            if binding.api_id != requirement.api_id() {
                return Err(StoreApiError::new(
                    422,
                    "STORE_BINDING_API_MISMATCH",
                    format!(
                        "topology binding {name} declares {}, but release requires {}",
                        binding.api_id,
                        requirement.api_id()
                    ),
                ));
            }
        }
        let provider_deployment_id = match topology_binding {
            Some(binding)
                if !binding.provider_deployment_id.is_empty()
                    && !explicit_provider.is_empty()
                    && binding.provider_deployment_id != explicit_provider =>
            {
                return Err(StoreApiError::new(
                    409,
                    "STORE_BINDING_PROVIDER_CONFLICT",
                    format!(
                        "binding {name} selects conflicting providers {} and {}",
                        binding.provider_deployment_id, explicit_provider
                    ),
                ));
            }
            Some(binding) if !binding.provider_deployment_id.is_empty() => {
                binding.provider_deployment_id.clone()
            }
            _ => explicit_provider,
        };
        let optional = topology_binding
            .map(|binding| binding.optional)
            .unwrap_or_else(|| requirement.optional());
        let selection = topology_binding
            .map(|binding| binding.selection.as_str())
            .unwrap_or_else(|| requirement.selection())
            .to_string();
        let version_requirement = topology_binding
            .map(|binding| binding.version.as_str())
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| requirement.version_requirement())
            .to_string();
        let auth_incompatible = all_candidates.iter().any(|candidate| {
            candidate.healthy
                && candidate.api_id == requirement.api_id()
                && api_version_matches(&version_requirement, &candidate.api_version)
                && !provider_auth_supported(contract.contract_version, &candidate.auth_mode)
                && topology_spec
                    .as_ref()
                    .is_none_or(|spec| topology_contains_provider_candidate(spec, candidate))
        });
        let mut candidates = all_candidates
            .iter()
            .filter(|candidate| {
                candidate.healthy
                    && candidate.api_id == requirement.api_id()
                    && api_version_matches(&version_requirement, &candidate.api_version)
                    && provider_auth_supported(contract.contract_version, &candidate.auth_mode)
                    && (selection != "same-node" || candidate.node_id == consumer_node_id)
                    && topology_spec
                        .as_ref()
                        .is_none_or(|spec| topology_contains_provider_candidate(spec, candidate))
                    && topology_binding.is_none_or(|binding| {
                        candidate.endpoint == binding.target_endpoint
                            && (binding.provider_deployment_id.is_empty()
                                || candidate.deployment_id == binding.provider_deployment_id)
                    })
            })
            .cloned()
            .collect::<Vec<_>>();
        candidates.sort_by(|left, right| {
            left.deployment_id
                .cmp(&right.deployment_id)
                .then_with(|| left.endpoint.cmp(&right.endpoint))
        });
        let selection_missing = !provider_deployment_id.is_empty()
            && !candidates
                .iter()
                .any(|candidate| candidate.deployment_id == provider_deployment_id);
        let ambiguous = provider_deployment_id.is_empty() && candidates.len() > 1;
        let missing = candidates.is_empty() || selection_missing;
        if ambiguous || selection_missing || (!optional && missing) {
            valid = false;
        }
        let recommended_provider_deployment_id =
            if !provider_deployment_id.is_empty() && !selection_missing {
                Some(provider_deployment_id.clone())
            } else if candidates.len() == 1 {
                Some(candidates[0].deployment_id.clone())
            } else {
                None
            };
        requirements.push(json!({
            "requirement_name": name,
            "api_id": requirement.api_id(),
            "version": version_requirement,
            "optional": optional,
            "selection": selection,
            "candidates": candidates,
            "recommended_provider_deployment_id": recommended_provider_deployment_id,
            "ambiguous": ambiguous,
            "missing": missing,
            "reason": if missing && auth_incompatible {
                "matching providers use a non-workload upstream auth mode; production ApiBindings support only workload or public upstream auth"
            } else {
                ""
            },
        }));
    }
    if let Some(unused) = selections_by_name
        .keys()
        .find(|name| !used_selections.contains(*name))
    {
        return Err(StoreApiError::new(
            422,
            "STORE_BINDING_UNKNOWN",
            format!("bindings[] references undeclared requirement {unused}"),
        ));
    }
    if let Some(unused) = topology_bindings
        .keys()
        .find(|name| !used_topology_bindings.contains(*name))
    {
        return Err(StoreApiError::new(
            422,
            "STORE_BINDING_UNKNOWN",
            format!("topology api_bindings references undeclared requirement {unused}"),
        ));
    }
    Ok((valid, requirements))
}

#[allow(clippy::too_many_arguments)]
fn resolve_install_api_bindings(
    console: &OrchestratorActionConsole,
    storage: &DurableStore,
    contract: &ServiceReleaseContract,
    consumer_deployment_id: &str,
    consumer_node_id: &str,
    requested_consumer_endpoint: &str,
    selections: &[InstallBindingSelection],
    topology: Option<&InstallTopologySelection>,
    allow_removed_topology_requirements: bool,
) -> Result<Vec<ApiBinding>, StoreApiError> {
    let mut selections_by_name = BTreeMap::new();
    for selection in selections {
        let name = required_text(&selection.name, "bindings[].name")?.to_string();
        let provider = required_text(
            &selection.provider_deployment_id,
            "bindings[].provider_deployment_id",
        )?
        .to_string();
        if selections_by_name.insert(name.clone(), provider).is_some() {
            return Err(StoreApiError::new(
                422,
                "STORE_BINDING_DUPLICATE",
                format!("binding selection {name} is declared more than once"),
            ));
        }
    }

    let (topology_spec, topology_revision_id) = topology
        .map(|selection| selected_topology_spec(storage, selection))
        .transpose()?
        .map_or((None, String::new()), |(spec, revision_id)| {
            (Some(spec), revision_id)
        });
    let consumer_endpoint = topology_spec
        .as_ref()
        .map(|spec| {
            topology_consumer_endpoint(
                spec,
                &contract.release.service_name,
                requested_consumer_endpoint,
            )
        })
        .transpose()?
        .unwrap_or_else(|| requested_consumer_endpoint.trim().to_string());
    let topology_bindings = topology_spec
        .as_ref()
        .map(|spec| topology_binding_contexts(spec, &topology_revision_id, &consumer_endpoint))
        .transpose()?
        .unwrap_or_default();
    let candidates = provider_candidates(console, storage)?;
    let mut resolved = Vec::with_capacity(contract.requirements().len());
    let mut used_selections = BTreeSet::new();
    let mut used_topology_bindings = BTreeSet::new();

    for requirement in contract.requirements() {
        let name = requirement.binding_name();
        let explicit_provider = selections_by_name.get(name).cloned().unwrap_or_default();
        if !explicit_provider.is_empty() {
            used_selections.insert(name.to_string());
        }
        let topology_binding = topology_bindings.get(name);
        if let Some(binding) = topology_binding {
            used_topology_bindings.insert(name.to_string());
            if binding.api_id != requirement.api_id() {
                return Err(StoreApiError::new(
                    422,
                    "STORE_BINDING_API_MISMATCH",
                    format!(
                        "topology binding {name} declares {}, but release requires {}",
                        binding.api_id,
                        requirement.api_id()
                    ),
                ));
            }
        }
        let provider_deployment_id = match topology_binding {
            Some(binding)
                if !binding.provider_deployment_id.is_empty()
                    && !explicit_provider.is_empty()
                    && binding.provider_deployment_id != explicit_provider =>
            {
                return Err(StoreApiError::new(
                    409,
                    "STORE_BINDING_PROVIDER_CONFLICT",
                    format!(
                        "binding {name} selects conflicting providers {} and {}",
                        binding.provider_deployment_id, explicit_provider
                    ),
                ));
            }
            Some(binding) if !binding.provider_deployment_id.is_empty() => {
                binding.provider_deployment_id.clone()
            }
            _ => explicit_provider,
        };
        let optional = topology_binding
            .map(|binding| binding.optional)
            .unwrap_or_else(|| requirement.optional());
        let selection = topology_binding
            .map(|binding| binding.selection.as_str())
            .unwrap_or_else(|| requirement.selection())
            .to_string();
        let version_requirement = topology_binding
            .map(|binding| binding.version.as_str())
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| requirement.version_requirement())
            .to_string();

        let rejected_auth = candidates.iter().any(|candidate| {
            candidate.api_id == requirement.api_id()
                && api_version_matches(&version_requirement, &candidate.api_version)
                && !provider_auth_supported(contract.contract_version, &candidate.auth_mode)
                && topology_spec
                    .as_ref()
                    .is_none_or(|spec| topology_contains_provider_candidate(spec, candidate))
                && topology_binding.is_none_or(|binding| {
                    candidate.endpoint == binding.target_endpoint
                        && (binding.provider_deployment_id.is_empty()
                            || candidate.deployment_id == binding.provider_deployment_id)
                })
        });
        let candidate_pool = candidates
            .iter()
            .filter(|candidate| {
                provider_auth_supported(contract.contract_version, &candidate.auth_mode)
                    && topology_spec
                        .as_ref()
                        .is_none_or(|spec| topology_contains_provider_candidate(spec, candidate))
                    && topology_binding.is_none_or(|binding| {
                        candidate.endpoint == binding.target_endpoint
                            && (binding.provider_deployment_id.is_empty()
                                || candidate.deployment_id == binding.provider_deployment_id)
                    })
            })
            .cloned()
            .collect::<Vec<_>>();
        if rejected_auth && candidate_pool.is_empty() {
            return Err(StoreApiError::new(
                422,
                "STORE_BINDING_PROVIDER_AUTH_UNSUPPORTED",
                format!(
                    "requirement {name} has only non-workload providers; production Deployment SDK calls support workload or public upstream auth"
                ),
            ));
        }
        let request = ApiBindingResolutionRequest {
            requirement_name: name.to_string(),
            api_id: requirement.api_id().to_string(),
            version_requirement: version_requirement.clone(),
            consumer_node_id: consumer_node_id.to_string(),
            provider_deployment_id,
            optional,
            selection,
        };
        let candidate =
            resolve_api_binding_candidate(&request, &candidate_pool).map_err(|error| {
                StoreApiError::new(422, "STORE_BINDING_UNRESOLVED", error.to_string())
            })?;
        let now = now_marker();
        let binding = match candidate {
            Some(candidate) => ApiBinding {
                binding_id: binding_id(consumer_deployment_id, name),
                requirement_name: name.to_string(),
                api_id: requirement.api_id().to_string(),
                api_version: candidate.api_version,
                consumer_deployment_id: consumer_deployment_id.to_string(),
                consumer_service_id: contract.release.service_name.clone(),
                consumer_node_id: consumer_node_id.to_string(),
                consumer_endpoint: consumer_endpoint.clone(),
                provider_deployment_id: candidate.deployment_id,
                provider_service_id: candidate.service_id,
                provider_node_id: candidate.node_id,
                provider_endpoint: candidate.endpoint,
                provider_path: candidate.path,
                virtual_endpoint: format!("/internal/apis/{}", requirement.api_id()),
                protocol: candidate.protocol,
                methods: candidate.methods,
                // `/internal/apis/*` is always a workload-authenticated
                // consumer surface. `provider_auth_mode` separately records
                // how Gateway must authenticate (or not authenticate) to the
                // selected upstream provider. A public upstream therefore
                // still needs a Deployment credential for scoped routing.
                auth_mode: "workload".to_string(),
                provider_auth_mode: candidate.auth_mode,
                permission: candidate.permission,
                timeout_ms: requirement.timeout_ms(),
                topology_id: topology_binding
                    .map(|binding| binding.topology_id.clone())
                    .unwrap_or_default(),
                topology_revision_id: topology_binding
                    .map(|binding| binding.revision_id.clone())
                    .unwrap_or_default(),
                link_source_endpoint: topology_binding
                    .map(|binding| binding.source_endpoint.clone())
                    .unwrap_or_default(),
                link_target_endpoint: topology_binding
                    .map(|binding| binding.target_endpoint.clone())
                    .unwrap_or_default(),
                credential_ref: String::new(),
                credential_generation: 1,
                context_generation: 1,
                desired_state: ApiBindingDesiredState::Active,
                observed_state: ApiBindingObservedState::Resolved,
                health: ApiBindingHealth::Unknown,
                drift: Vec::new(),
                last_operation_id: String::new(),
                state: ApiBindingState::Resolved,
                optional,
                reason: String::new(),
                created_at: now.clone(),
                updated_at: now,
            },
            None => unbound_binding(
                consumer_deployment_id,
                &contract.release.service_name,
                consumer_node_id,
                &consumer_endpoint,
                name,
                requirement.api_id(),
                &version_requirement,
                "no healthy provider currently satisfies this optional requirement",
            ),
        };
        binding.validate().map_err(|error| {
            StoreApiError::new(
                500,
                "STORE_BINDING_INVALID",
                format!("resolved binding {name} is invalid: {error}"),
            )
        })?;
        resolved.push(binding);
    }

    if let Some(unused) = selections_by_name
        .keys()
        .find(|name| !used_selections.contains(*name))
    {
        return Err(StoreApiError::new(
            422,
            "STORE_BINDING_UNKNOWN",
            format!("bindings[] references undeclared requirement {unused}"),
        ));
    }
    if !allow_removed_topology_requirements
        && let Some(unused) = topology_bindings
            .keys()
            .find(|name| !used_topology_bindings.contains(*name))
    {
        return Err(StoreApiError::new(
            422,
            "STORE_BINDING_UNKNOWN",
            format!("topology api_bindings references undeclared requirement {unused}"),
        ));
    }
    resolved.sort_by(|left, right| left.binding_id.cmp(&right.binding_id));
    Ok(resolved)
}

fn production_binding_plan<'a>(
    bindings: impl IntoIterator<Item = &'a ApiBinding>,
) -> Vec<ApiBinding> {
    let mut planned = bindings
        .into_iter()
        .map(|binding| {
            let mut planned = binding.clone();
            // PENDING belongs only to the internal Topology PREPARE payload.
            // Public Store plans and Agent contexts carry a proven provider
            // resolution; activation remains represented by the Operation.
            if planned.state == ApiBindingState::Pending {
                planned.state = ApiBindingState::Resolved;
                planned.observed_state = ApiBindingObservedState::Resolved;
            }
            planned
        })
        .collect::<Vec<_>>();
    planned.sort_by(|left, right| left.binding_id.cmp(&right.binding_id));
    planned
}

fn ensure_managed_api_bindings_ready(
    storage: &DurableStore,
    contract: &ServiceReleaseContract,
    bindings: &[ApiBinding],
    topology: Option<&InstallTopologySelection>,
) -> Result<(), StoreApiError> {
    if contract.requirements().is_empty() {
        return Ok(());
    }
    let topology = topology.ok_or_else(|| {
        StoreApiError::new(
            422,
            "STORE_BINDING_TOPOLOGY_REQUIRED",
            "required APIs must be confirmed through an immutable applied Topology revision",
        )
    })?;
    let (spec, _) = selected_topology_spec(storage, topology)?;

    if let Some(binding) = bindings.iter().find(|binding| {
        matches!(
            binding.state,
            ApiBindingState::Pending | ApiBindingState::Error
        )
    }) {
        return Err(StoreApiError::new(
            422,
            "STORE_BINDING_NOT_READY",
            format!(
                "requirement {} cannot enter a production plan in {:?} state",
                binding.requirement_name, binding.state
            ),
        ));
    }

    for requirement in contract.requirements() {
        let matches = bindings
            .iter()
            .filter(|binding| binding.requirement_name == requirement.binding_name())
            .collect::<Vec<_>>();
        let [binding] = matches.as_slice() else {
            return Err(StoreApiError::new(
                422,
                "STORE_BINDING_UNRESOLVED",
                format!(
                    "requirement {} must resolve to exactly one binding",
                    requirement.binding_name()
                ),
            ));
        };
        if binding.state == ApiBindingState::Unbound && requirement.optional() {
            continue;
        }
        if !matches!(
            binding.state,
            ApiBindingState::Resolved | ApiBindingState::Active
        ) || binding.provider_deployment_id.trim().is_empty()
            || binding.provider_endpoint.trim().is_empty()
        {
            return Err(StoreApiError::new(
                422,
                "STORE_BINDING_UNRESOLVED",
                format!(
                    "required API binding {} has no healthy resolved provider",
                    requirement.binding_name()
                ),
            ));
        }
        let provider_endpoint = spec.endpoints.iter().find(|endpoint| {
            endpoint.endpoint == binding.provider_endpoint
                && endpoint.service_id == binding.provider_service_id
        });
        let Some(provider_endpoint) = provider_endpoint else {
            return Err(StoreApiError::new(
                422,
                "STORE_BINDING_PROVIDER_NOT_APPLIED",
                format!(
                    "provider deployment {} endpoint {} is not present in applied topology {}",
                    binding.provider_deployment_id, binding.provider_endpoint, spec.topology_id
                ),
            ));
        };
        if let Some(configured_deployment_id) = provider_endpoint
            .config
            .as_object()
            .and_then(|config| config.get("deployment_id"))
            .and_then(Value::as_str)
            .filter(|deployment_id| !deployment_id.trim().is_empty())
            && configured_deployment_id != binding.provider_deployment_id
        {
            return Err(StoreApiError::new(
                409,
                "STORE_BINDING_PROVIDER_NOT_APPLIED",
                format!(
                    "applied topology endpoint {} selects deployment {}, not requested provider {}",
                    provider_endpoint.endpoint,
                    configured_deployment_id,
                    binding.provider_deployment_id
                ),
            ));
        }
    }
    Ok(())
}

fn provider_auth_supported(_contract_version: u32, auth_mode: &str) -> bool {
    // Compatibility manifests may still describe legacy service/internal
    // provider authentication, but a production ApiBinding always uses the
    // workload identity path (or an explicitly public upstream). Legacy auth
    // remains importable and usable by development Compose only.
    matches!(auth_mode, "workload" | "public")
}

pub(crate) fn selected_topology_spec(
    storage: &DurableStore,
    selection: &InstallTopologySelection,
) -> Result<(TopologySpec, String), StoreApiError> {
    let topology_id = required_text(&selection.topology_id, "topology.topology_id")?;
    let heads = storage
        .topology_heads(topology_id)
        .map_err(storage_error)?
        .ok_or_else(|| {
            StoreApiError::new(
                404,
                "STORE_BINDING_TOPOLOGY_NOT_FOUND",
                format!("topology {topology_id} was not found"),
            )
        })?;
    if heads.applying_revision_id.is_some() {
        return Err(StoreApiError::new(
            409,
            "STORE_BINDING_TOPOLOGY_APPLYING",
            format!("topology {topology_id} already has an apply in progress"),
        ));
    }
    let revision_id = heads.applied_revision_id.ok_or_else(|| {
        StoreApiError::new(
            409,
            "STORE_BINDING_TOPOLOGY_NOT_APPLIED",
            format!(
                "topology {topology_id} has no applied head; apply its initial revision before installing a bound service"
            ),
        )
    })?;
    if selection.revision_id.trim() != revision_id {
        return Err(StoreApiError::new(
            409,
            "STORE_BINDING_TOPOLOGY_ETAG_CONFLICT",
            format!(
                "topology {topology_id} applied head is {revision_id}, but install confirmed {}",
                selection.revision_id.trim()
            ),
        ));
    }
    if heads.draft_revision_id != revision_id {
        return Err(StoreApiError::new(
            409,
            "STORE_BINDING_TOPOLOGY_DRAFT_DIVERGED",
            format!(
                "topology {topology_id} has unapplied draft {}; apply or discard it before Store creates a deployment Binding revision",
                heads.draft_revision_id
            ),
        ));
    }
    let revision = storage
        .topology_revision(topology_id, &revision_id)
        .map_err(storage_error)?
        .ok_or_else(|| {
            StoreApiError::new(
                404,
                "STORE_BINDING_TOPOLOGY_REVISION_NOT_FOUND",
                format!("topology {topology_id} revision {revision_id} was not found"),
            )
        })?;
    Ok((revision.spec().clone(), revision_id))
}

fn topology_consumer_endpoint(
    spec: &TopologySpec,
    service_id: &str,
    requested: &str,
) -> Result<String, StoreApiError> {
    if !requested.trim().is_empty() {
        validate_endpoint_id(requested.trim()).map_err(|error| {
            StoreApiError::new(
                422,
                "STORE_BINDING_CONSUMER_ENDPOINT_INVALID",
                format!("consumer endpoint is invalid: {error}"),
            )
        })?;
        let identity = parse_endpoint_id(requested.trim()).map_err(|error| {
            StoreApiError::new(
                422,
                "STORE_BINDING_CONSUMER_ENDPOINT_INVALID",
                error.to_string(),
            )
        })?;
        if identity.service_name != service_id {
            return Err(StoreApiError::new(
                422,
                "STORE_BINDING_CONSUMER_ENDPOINT_MISMATCH",
                format!(
                    "consumer endpoint service {} must match {service_id}",
                    identity.service_name
                ),
            ));
        }
        if spec.endpoints.iter().any(|endpoint| {
            endpoint.endpoint == requested.trim() && endpoint.service_id == service_id
        }) {
            return Ok(requested.trim().to_string());
        }
        // Store will add this exact endpoint to the proposed immutable
        // revision. Resolution still uses only explicitly selected providers.
        return Ok(requested.trim().to_string());
    }
    let endpoints = spec
        .endpoints
        .iter()
        .filter(|endpoint| endpoint.service_id == service_id)
        .map(|endpoint| endpoint.endpoint.clone())
        .collect::<Vec<_>>();
    match endpoints.as_slice() {
        [endpoint] => Ok(endpoint.clone()),
        [] => Err(StoreApiError::new(
            422,
            "STORE_BINDING_CONSUMER_ENDPOINT_REQUIRED",
            format!(
                "topology {} has no endpoint for consumer service {service_id}",
                spec.topology_id
            ),
        )),
        _ => Err(StoreApiError::new(
            409,
            "STORE_BINDING_CONSUMER_ENDPOINT_AMBIGUOUS",
            format!(
                "topology {} has multiple endpoints for {service_id}; install endpoint is required",
                spec.topology_id
            ),
        )),
    }
}

fn topology_binding_contexts(
    spec: &TopologySpec,
    revision_id: &str,
    consumer_endpoint: &str,
) -> Result<BTreeMap<String, TopologyBindingContext>, StoreApiError> {
    let mut bindings = BTreeMap::new();
    for link in spec
        .links
        .iter()
        .filter(|link| link.enabled && link.source_endpoint == consumer_endpoint)
    {
        for binding in &link.api_bindings {
            let context = TopologyBindingContext {
                topology_id: spec.topology_id.clone(),
                revision_id: revision_id.to_string(),
                source_endpoint: link.source_endpoint.clone(),
                target_endpoint: link.target_endpoint.clone(),
                api_id: binding.api_id.clone(),
                version: binding.version.clone(),
                optional: binding.optional,
                provider_deployment_id: binding.provider_deployment_id.clone(),
                selection: binding.selection.clone(),
            };
            if bindings
                .insert(binding.requirement_name.clone(), context)
                .is_some()
            {
                return Err(StoreApiError::new(
                    409,
                    "STORE_BINDING_TOPOLOGY_AMBIGUOUS",
                    format!(
                        "topology {} binds requirement {} through more than one Link",
                        spec.topology_id, binding.requirement_name
                    ),
                ));
            }
        }
    }
    Ok(bindings)
}

#[derive(Debug, Clone)]
pub(crate) struct StoreTopologyApplyPlan {
    pub(crate) topology_id: String,
    pub(crate) revision_id: String,
    pub(crate) staged_bindings: Vec<ApiBinding>,
    pub(crate) previous_bindings: Vec<ApiBinding>,
}

#[derive(Debug, Clone)]
pub(crate) struct BindingContextTransitionPlan {
    pub(crate) deployment_id: String,
    pub(crate) node_id: String,
    pub(crate) container_id: String,
    pub(crate) forward: BindingContextApplyPayload,
    pub(crate) rollback: BindingContextApplyPayload,
}

/// A validated, materializable view of bindings that are still staged by a
/// Topology apply. The durable rows remain `PENDING`; only these private clones
/// are represented as `RESOLVED` while constructing the desired Agent context.
/// Keeping this conversion behind a distinct type prevents ordinary context
/// callers from treating an arbitrary uncommitted binding as available.
#[derive(Debug, Clone)]
struct StagedApplyDesiredContextBindings(Vec<ApiBinding>);

impl StagedApplyDesiredContextBindings {
    fn from_plans(
        plans: &[StoreTopologyApplyPlan],
        deployment_id: &str,
    ) -> Result<Self, StoreApiError> {
        let mut materializable = Vec::new();
        let mut requirements = BTreeSet::new();
        let mut operation_id = None::<String>;
        for plan in plans {
            for binding in plan.staged_bindings.iter().filter(|binding| {
                binding.consumer_deployment_id == deployment_id && binding.desired_state == "ACTIVE"
            }) {
                if binding.state != ApiBindingState::Pending
                    || binding.observed_state != "PENDING"
                    || binding.topology_id != plan.topology_id
                    || binding.topology_revision_id != plan.revision_id
                    || binding.last_operation_id.trim().is_empty()
                {
                    return Err(StoreApiError::new(
                        409,
                        "STORE_STAGED_BINDING_CONTEXT_INVALID",
                        format!(
                            "binding {} is not a valid staged activation for topology {} revision {}",
                            binding.binding_id, plan.topology_id, plan.revision_id
                        ),
                    ));
                }
                match operation_id.as_deref() {
                    Some(expected) if expected != binding.last_operation_id => {
                        return Err(StoreApiError::new(
                            409,
                            "STORE_STAGED_BINDING_CONTEXT_INVALID",
                            format!(
                                "consumer {deployment_id} staged bindings span more than one Operation"
                            ),
                        ));
                    }
                    None => operation_id = Some(binding.last_operation_id.clone()),
                    Some(_) => {}
                }
                if !requirements.insert(binding.requirement_name.clone()) {
                    return Err(StoreApiError::new(
                        409,
                        "STORE_BINDING_REQUIREMENT_CONFLICT",
                        format!(
                            "consumer {deployment_id} requirement {} is staged more than once",
                            binding.requirement_name
                        ),
                    ));
                }

                let mut resolved_view = binding.clone();
                resolved_view.state = ApiBindingState::Resolved;
                resolved_view.observed_state = ApiBindingObservedState::Resolved;
                resolved_view.validate().map_err(|error| {
                    StoreApiError::new(
                        409,
                        "STORE_STAGED_BINDING_CONTEXT_INVALID",
                        format!(
                            "binding {} cannot materialize a desired Service Context: {error}",
                            binding.binding_id
                        ),
                    )
                })?;
                materializable.push(resolved_view);
            }
        }
        materializable.sort_by(|left, right| {
            left.requirement_name
                .cmp(&right.requirement_name)
                .then_with(|| left.binding_id.cmp(&right.binding_id))
        });
        Ok(Self(materializable))
    }

    fn as_slice(&self) -> &[ApiBinding] {
        &self.0
    }
}

pub(crate) fn propose_generation_sibling_topology(
    storage: &DurableStore,
    selection: &InstallTopologySelection,
    operation_id: &str,
) -> Result<StoreTopologyApplyPlan, StoreApiError> {
    let (spec, expected_draft) = selected_topology_spec(storage, selection)?;
    let topology_id = spec.topology_id.clone();
    let revision = storage
        .create_next_topology_revision(
            &topology_id,
            &expected_draft,
            spec,
            now_marker(),
            "store-replacement".to_string(),
            "reproject deployment-wide binding credential generation".to_string(),
        )
        .map_err(storage_error)?;
    let previous_bindings = storage
        .api_bindings_for_topology(&topology_id)
        .map_err(storage_error)?;
    let desired = previous_bindings
        .iter()
        .filter(|binding| {
            binding.desired_state == "ACTIVE" && binding.state == ApiBindingState::Active
        })
        .cloned()
        .collect::<Vec<_>>();
    let staged_bindings = storage
        .stage_precomputed_topology_api_bindings(
            &topology_id,
            revision.revision_id(),
            operation_id,
            desired,
        )
        .map_err(|error| {
            StoreApiError::new(422, "STORE_BINDING_TOPOLOGY_INVALID", error.to_string())
        })?;
    Ok(StoreTopologyApplyPlan {
        topology_id,
        revision_id: revision.revision_id().to_string(),
        staged_bindings,
        previous_bindings,
    })
}

pub(crate) fn align_group_binding_generations(
    storage: &DurableStore,
    plans: &mut [StoreTopologyApplyPlan],
    affected_consumers: &BTreeSet<String>,
) -> Result<(), StoreApiError> {
    let planned_topologies = plans
        .iter()
        .map(|plan| plan.topology_id.clone())
        .collect::<BTreeSet<_>>();
    let mut seen_requirements = BTreeSet::new();
    for consumer in affected_consumers {
        let current = active_consumer_bindings(storage, consumer)?;
        let current_topologies = current
            .iter()
            .map(|binding| binding.topology_id.clone())
            .filter(|topology_id| !topology_id.is_empty())
            .collect::<BTreeSet<_>>();
        if !current_topologies.is_subset(&planned_topologies) {
            return Err(StoreApiError::new(
                409,
                "STORE_REPLACEMENT_SIBLING_TOPOLOGY_REQUIRED",
                format!(
                    "consumer {consumer} has deployment-wide sibling bindings in {:?}; every sibling topology requires one strong CAS entry",
                    current_topologies
                        .difference(&planned_topologies)
                        .collect::<Vec<_>>()
                ),
            ));
        }
        let binding_generation = current
            .iter()
            .map(|binding| {
                binding
                    .credential_generation
                    .max(binding.context_generation)
            })
            .max()
            .unwrap_or(0);
        let projected_generation = storage
            .get_state::<ManagedServiceContextProjection>("managed-service-context-v1", consumer)
            .map_err(storage_error)?
            .map(|projection| {
                projection
                    .current
                    .as_ref()
                    .unwrap_or(&projection.last_nonempty)
                    .generation
                    .max(projection.last_nonempty.generation)
            })
            .unwrap_or(0);
        let next_generation = binding_generation
            .max(projected_generation)
            .saturating_add(1)
            .max(1);
        for plan in plans.iter_mut() {
            for binding in plan.staged_bindings.iter_mut().filter(|binding| {
                binding.consumer_deployment_id == *consumer && binding.desired_state == "ACTIVE"
            }) {
                if !seen_requirements.insert((consumer.clone(), binding.requirement_name.clone())) {
                    return Err(StoreApiError::new(
                        409,
                        "STORE_BINDING_REQUIREMENT_CONFLICT",
                        format!(
                            "consumer {consumer} requirement {} is active in more than one topology",
                            binding.requirement_name
                        ),
                    ));
                }
                binding.credential_generation = next_generation;
                binding.context_generation = next_generation;
            }
        }
    }
    Ok(())
}

pub(crate) fn binding_context_transition_plans(
    storage: &DurableStore,
    plans: &[StoreTopologyApplyPlan],
    affected_consumers: &BTreeSet<String>,
) -> Result<Vec<BindingContextTransitionPlan>, StoreApiError> {
    let mut transitions = Vec::new();
    for deployment_id in affected_consumers {
        let runtime = storage
            .runtime_instance(deployment_id)
            .map_err(storage_error)?
            .ok_or_else(|| {
                StoreApiError::new(
                    409,
                    "STORE_BINDING_CONSUMER_RUNTIME_MISSING",
                    format!("consumer deployment {deployment_id} has no runtime projection"),
                )
            })?;
        if runtime.management_mode != orchestrator_storage::RuntimeManagementMode::Managed {
            return Err(StoreApiError::new(
                422,
                "STORE_EXTERNAL_BINDING_CONTEXT_REQUIRED",
                format!(
                    "consumer deployment {deployment_id} is External and cannot receive a managed binding context"
                ),
            ));
        }
        let contract = storage
            .service_release_contract(
                &runtime.instance.service_id,
                &runtime.instance.release_version,
            )
            .map_err(storage_error)?
            .ok_or_else(|| {
                StoreApiError::new(
                    409,
                    "STORE_BINDING_CONSUMER_RELEASE_MISSING",
                    format!(
                        "consumer deployment {deployment_id} has no exact release contract {}@{}",
                        runtime.instance.service_id, runtime.instance.release_version
                    ),
                )
            })?;
        let previous_bindings = active_consumer_bindings(storage, deployment_id)?;
        let desired_bindings = StagedApplyDesiredContextBindings::from_plans(plans, deployment_id)?;
        let derived_previous = managed_service_context_spec(
            storage,
            &contract,
            &runtime.node_id,
            &previous_bindings,
            false,
        )?;
        let persisted = storage
            .get_state::<ManagedServiceContextProjection>(
                "managed-service-context-v1",
                deployment_id,
            )
            .map_err(storage_error)?;
        if let (Some(projected), Some(derived)) = (
            persisted
                .as_ref()
                .and_then(|projection| projection.current.as_ref()),
            derived_previous.as_ref(),
        ) && projected != derived
        {
            return Err(StoreApiError::new(
                409,
                "STORE_BINDING_CONTEXT_PROJECTION_DRIFT",
                format!(
                    "consumer deployment {deployment_id} persisted Agent context does not match active ApiBindings"
                ),
            ));
        }
        let previous = persisted
            .map(|projection| projection.last_nonempty)
            .or(derived_previous)
            .ok_or_else(|| {
                StoreApiError::new(
                    409,
                    "STORE_BINDING_PREVIOUS_CONTEXT_REQUIRED",
                    format!("consumer deployment {deployment_id} has no previous managed context"),
                )
            })?;
        // Removing the last required API is an explicit credential/context
        // revocation before uninstall. A partial required set remains invalid,
        // while optional-only and event-only contracts keep their ordinary
        // materialization semantics.
        let desired = if desired_bindings.as_slice().is_empty()
            && contract
                .requirements()
                .iter()
                .any(|requirement| !requirement.optional())
        {
            None
        } else {
            managed_service_context_spec(
                storage,
                &contract,
                &runtime.node_id,
                desired_bindings.as_slice(),
                false,
            )?
        };
        let forward = BindingContextApplyPayload {
            deployment_id: deployment_id.clone(),
            service_id: runtime.instance.service_id.clone(),
            context: desired.clone(),
            previous_context: Some(previous.clone()),
        };
        let rollback = BindingContextApplyPayload {
            deployment_id: deployment_id.clone(),
            service_id: runtime.instance.service_id.clone(),
            context: Some(previous.clone()),
            previous_context: desired.or(Some(previous)),
        };
        forward.validate().map_err(|error| {
            StoreApiError::new(500, "STORE_BINDING_CONTEXT_INVALID", error.to_string())
        })?;
        rollback.validate().map_err(|error| {
            StoreApiError::new(500, "STORE_BINDING_CONTEXT_INVALID", error.to_string())
        })?;
        transitions.push(BindingContextTransitionPlan {
            deployment_id: deployment_id.clone(),
            node_id: runtime.node_id,
            container_id: runtime.instance.container_id,
            forward,
            rollback,
        });
    }
    transitions.sort_by(|left, right| left.deployment_id.cmp(&right.deployment_id));
    Ok(transitions)
}

fn active_consumer_bindings(
    storage: &DurableStore,
    deployment_id: &str,
) -> Result<Vec<ApiBinding>, StoreApiError> {
    let mut bindings = storage
        .api_bindings_for_deployment(deployment_id)
        .map_err(storage_error)?
        .into_iter()
        .filter(|binding| {
            binding.desired_state == "ACTIVE" && binding.state == ApiBindingState::Active
        })
        .collect::<Vec<_>>();
    bindings.sort_by(|left, right| left.requirement_name.cmp(&right.requirement_name));
    Ok(bindings)
}

fn active_provider_bindings(
    storage: &DurableStore,
    deployment_id: &str,
) -> Result<Vec<ApiBinding>, StoreApiError> {
    let mut bindings = Vec::new();
    for heads in storage.list_topology_heads().map_err(storage_error)? {
        bindings.extend(
            storage
                .api_bindings_for_topology(&heads.topology_id)
                .map_err(storage_error)?
                .into_iter()
                .filter(|binding| {
                    binding.provider_deployment_id == deployment_id
                        && binding.desired_state == "ACTIVE"
                        && binding.state == ApiBindingState::Active
                }),
        );
    }
    bindings.sort_by(|left, right| {
        (
            &left.topology_id,
            &left.consumer_deployment_id,
            &left.requirement_name,
        )
            .cmp(&(
                &right.topology_id,
                &right.consumer_deployment_id,
                &right.requirement_name,
            ))
    });
    Ok(bindings)
}

fn require_matching_replacement_topologies<'a>(
    selected: &'a [InstallTopologySelection],
    existing: &[ApiBinding],
) -> Result<Vec<&'a InstallTopologySelection>, StoreApiError> {
    if selected.is_empty() {
        return Err(StoreApiError::new(
            422,
            "STORE_REPLACEMENT_TOPOLOGY_REQUIRED",
            "a topology-bound replacement requires strong ETag confirmation for every affected topology",
        ));
    }
    let topology_ids = existing
        .iter()
        .map(|binding| binding.topology_id.as_str())
        .filter(|topology_id| !topology_id.is_empty())
        .collect::<BTreeSet<_>>();
    if topology_ids.is_empty() {
        if selected.len() != 1 {
            return Err(StoreApiError::new(
                422,
                "STORE_REPLACEMENT_TOPOLOGY_AMBIGUOUS",
                "a consumer without an existing binding authority requires exactly one topology CAS input",
            ));
        }
        return Ok(vec![&selected[0]]);
    }
    let selected_ids = selected
        .iter()
        .map(|selection| selection.topology_id.as_str())
        .collect::<BTreeSet<_>>();
    if selected_ids != topology_ids {
        return Err(StoreApiError::new(
            409,
            "STORE_REPLACEMENT_TOPOLOGY_CONFLICT",
            format!(
                "replacement topology CAS set {:?} must exactly match affected applied topologies {:?}",
                selected_ids, topology_ids
            ),
        ));
    }
    Ok(selected
        .iter()
        .filter(|selection| topology_ids.contains(selection.topology_id.as_str()))
        .collect())
}

#[allow(clippy::too_many_arguments)]
fn propose_provider_replacement_topology(
    storage: &DurableStore,
    selection: &InstallTopologySelection,
    contract: &ServiceReleaseContract,
    old_deployment_id: &str,
    new_deployment_id: &str,
    new_node_id: &str,
    new_endpoint: &str,
    operation_id: &str,
) -> Result<StoreTopologyApplyPlan, StoreApiError> {
    let (mut spec, expected_draft) = selected_topology_spec(storage, selection)?;
    let new_endpoint = required_text(new_endpoint, "endpoint")?;
    validate_endpoint_id(new_endpoint).map_err(|error| {
        StoreApiError::new(
            422,
            "STORE_REPLACEMENT_PROVIDER_ENDPOINT_INVALID",
            error.to_string(),
        )
    })?;
    let identity = parse_endpoint_id(new_endpoint).map_err(|error| {
        StoreApiError::new(
            422,
            "STORE_REPLACEMENT_PROVIDER_ENDPOINT_INVALID",
            error.to_string(),
        )
    })?;
    if identity.service_name != contract.release.service_name {
        return Err(StoreApiError::new(
            422,
            "STORE_REPLACEMENT_PROVIDER_ENDPOINT_MISMATCH",
            format!(
                "replacement endpoint service {} must match {}",
                identity.service_name, contract.release.service_name
            ),
        ));
    }
    let previous_bindings = storage
        .api_bindings_for_topology(&spec.topology_id)
        .map_err(storage_error)?;
    let affected = previous_bindings
        .iter()
        .filter(|binding| {
            binding.provider_deployment_id == old_deployment_id
                && binding.desired_state == "ACTIVE"
                && binding.state == ApiBindingState::Active
        })
        .map(|binding| binding.binding_id.as_str())
        .collect::<BTreeSet<_>>();
    if affected.is_empty() {
        return Err(StoreApiError::new(
            409,
            "STORE_REPLACEMENT_PROVIDER_BINDINGS_MISSING",
            format!(
                "deployment {old_deployment_id} has no active provider binding in topology {}",
                spec.topology_id
            ),
        ));
    }

    let old_targets = previous_bindings
        .iter()
        .filter(|binding| affected.contains(binding.binding_id.as_str()))
        .map(|binding| binding.link_target_endpoint.as_str())
        .collect::<BTreeSet<_>>();
    if old_targets.len() != 1 {
        return Err(StoreApiError::new(
            409,
            "STORE_REPLACEMENT_PROVIDER_ENDPOINT_AMBIGUOUS",
            "active provider bindings do not share one topology endpoint",
        ));
    }
    let old_target = *old_targets.first().expect("one target was checked");
    match spec
        .endpoints
        .iter_mut()
        .find(|endpoint| endpoint.endpoint == new_endpoint)
    {
        Some(endpoint) if endpoint.service_id != contract.release.service_name => {
            return Err(StoreApiError::new(
                409,
                "STORE_REPLACEMENT_PROVIDER_ENDPOINT_CONFLICT",
                format!(
                    "endpoint {new_endpoint} already belongs to {}",
                    endpoint.service_id
                ),
            ));
        }
        Some(endpoint) => {
            endpoint.protocol = contract.release.backend.protocol.clone();
            endpoint.health_path = contract.release.backend.health_path.clone();
            endpoint.config = json!({
                "deployment_id": new_deployment_id,
                "node_id": new_node_id,
            });
        }
        None => spec.endpoints.push(TopologyEndpointSpec {
            endpoint: new_endpoint.to_string(),
            service_id: contract.release.service_name.clone(),
            protocol: contract.release.backend.protocol.clone(),
            health_path: contract.release.backend.health_path.clone(),
            display_name: contract.release.service_name.clone(),
            note: "Store-managed replacement provider endpoint".to_string(),
            config: json!({
                "deployment_id": new_deployment_id,
                "node_id": new_node_id,
            }),
        }),
    }
    let mut rewritten_links = Vec::with_capacity(spec.links.len() + 1);
    for mut link in std::mem::take(&mut spec.links) {
        if link.target_endpoint != old_target {
            rewritten_links.push(link);
            continue;
        }
        let replacement_template = link.clone();
        let (mut affected_bindings, retained_bindings): (Vec<_>, Vec<_>) = link
            .api_bindings
            .into_iter()
            .partition(|binding| binding.provider_deployment_id == old_deployment_id);
        if affected_bindings.is_empty() {
            link.api_bindings = retained_bindings;
            rewritten_links.push(link);
            continue;
        }
        for binding in &mut affected_bindings {
            binding.provider_deployment_id = new_deployment_id.to_string();
        }
        if !retained_bindings.is_empty() {
            // A Link may aggregate multiple named requirements that happen to
            // share an endpoint. Move only the affected requirements to a new
            // target Link and leave unrelated provider bindings untouched.
            let mut replacement_link = replacement_template;
            replacement_link.target_endpoint = new_endpoint.to_string();
            replacement_link.api_bindings = affected_bindings;
            link.api_bindings = retained_bindings;
            rewritten_links.push(link);
            rewritten_links.push(replacement_link);
        } else {
            link.target_endpoint = new_endpoint.to_string();
            link.api_bindings = affected_bindings;
            rewritten_links.push(link);
        }
    }
    spec.links = rewritten_links;
    if spec.root_endpoint == old_target {
        spec.root_endpoint = new_endpoint.to_string();
    }
    if old_target != spec.root_endpoint
        && !spec
            .links
            .iter()
            .any(|link| link.source_endpoint == old_target || link.target_endpoint == old_target)
    {
        spec.endpoints
            .retain(|endpoint| endpoint.endpoint != old_target);
    }
    // Compatibility is a plan-time precondition. Validate it before creating
    // the immutable draft so a rejected upgrade cannot leave an unusable
    // revision behind.
    for binding in previous_bindings
        .iter()
        .filter(|binding| affected.contains(binding.binding_id.as_str()))
    {
        let (link, selection) = spec
            .links
            .iter()
            .flat_map(|link| {
                link.api_bindings
                    .iter()
                    .map(move |selection| (link, selection))
            })
            .find(|(link, selection)| {
                link.source_endpoint == binding.link_source_endpoint
                    && selection.requirement_name == binding.requirement_name
                    && selection.provider_deployment_id == new_deployment_id
            })
            .ok_or_else(|| {
                StoreApiError::new(
                    500,
                    "STORE_REPLACEMENT_BINDING_REVISION_INVALID",
                    format!(
                        "replacement Link for binding {} is missing",
                        binding.binding_id
                    ),
                )
            })?;
        let version_requirement = if selection.version.trim().is_empty() {
            binding.api_version.as_str()
        } else {
            selection.version.as_str()
        };
        if !contract.release.apis.iter().any(|api| {
            api.api_id == binding.api_id
                && api.protocol == link.protocol
                && api_version_matches(version_requirement, &api.version)
        }) {
            return Err(StoreApiError::new(
                422,
                "STORE_REPLACEMENT_PROVIDER_API_INCOMPATIBLE",
                format!(
                    "replacement release does not provide {} compatible with {}",
                    binding.api_id, version_requirement
                ),
            ));
        }
    }
    spec = spec.canonicalized().map_err(core_error)?;
    let topology_id = spec.topology_id.clone();
    let revision = storage
        .create_next_topology_revision(
            &topology_id,
            &expected_draft,
            spec,
            now_marker(),
            "store-replacement".to_string(),
            format!("switch provider {old_deployment_id} to {new_deployment_id}"),
        )
        .map_err(storage_error)?;

    let mut desired = previous_bindings
        .iter()
        .filter(|binding| {
            binding.desired_state == "ACTIVE" && binding.state == ApiBindingState::Active
        })
        .cloned()
        .collect::<Vec<_>>();
    for binding in &mut desired {
        binding.topology_revision_id = revision.revision_id().to_string();
        binding.last_operation_id = operation_id.to_string();
        if !affected.contains(binding.binding_id.as_str()) {
            continue;
        }
        let topology_binding = revision
            .spec()
            .links
            .iter()
            .flat_map(|link| {
                link.api_bindings
                    .iter()
                    .map(move |selection| (link, selection))
            })
            .find(|(link, selection)| {
                link.source_endpoint == binding.link_source_endpoint
                    && selection.requirement_name == binding.requirement_name
                    && selection.provider_deployment_id == new_deployment_id
            })
            .ok_or_else(|| {
                StoreApiError::new(
                    500,
                    "STORE_REPLACEMENT_BINDING_REVISION_INVALID",
                    format!(
                        "replacement Link for binding {} is missing",
                        binding.binding_id
                    ),
                )
            })?;
        let version_requirement = if topology_binding.1.version.trim().is_empty() {
            binding.api_version.as_str()
        } else {
            topology_binding.1.version.as_str()
        };
        let provider_api = contract
            .release
            .apis
            .iter()
            .find(|api| {
                api.api_id == binding.api_id
                    && api.protocol == topology_binding.0.protocol
                    && api_version_matches(version_requirement, &api.version)
            })
            .ok_or_else(|| {
                StoreApiError::new(
                    422,
                    "STORE_REPLACEMENT_PROVIDER_API_INCOMPATIBLE",
                    format!(
                        "replacement release does not provide {} compatible with {}",
                        binding.api_id, version_requirement
                    ),
                )
            })?;
        binding.api_version = provider_api.version.clone();
        binding.provider_deployment_id = new_deployment_id.to_string();
        binding.provider_service_id = contract.release.service_name.clone();
        binding.provider_node_id = new_node_id.to_string();
        binding.provider_endpoint = new_endpoint.to_string();
        binding.provider_path = provider_api.path_prefix.clone();
        binding.protocol = provider_api.protocol.clone();
        binding.methods = provider_api.methods.clone();
        binding.provider_auth_mode = provider_api.auth_mode.clone();
        binding.permission = provider_api.permission.clone();
        binding.link_target_endpoint = new_endpoint.to_string();
    }
    let staged_bindings = storage
        .stage_precomputed_topology_api_bindings(
            revision.spec().topology_id.as_str(),
            revision.revision_id(),
            operation_id,
            desired,
        )
        .map_err(|error| {
            StoreApiError::new(422, "STORE_BINDING_TOPOLOGY_INVALID", error.to_string())
        })?;
    Ok(StoreTopologyApplyPlan {
        topology_id: revision.spec().topology_id.clone(),
        revision_id: revision.revision_id().to_string(),
        staged_bindings,
        previous_bindings,
    })
}

#[allow(clippy::too_many_arguments)]
fn propose_dual_role_replacement_topology(
    storage: &DurableStore,
    selection: &InstallTopologySelection,
    contract: &ServiceReleaseContract,
    old_deployment_id: &str,
    new_deployment_id: &str,
    new_node_id: &str,
    new_endpoint: &str,
    consumer_bindings: &[ApiBinding],
    operation_id: &str,
) -> Result<StoreTopologyApplyPlan, StoreApiError> {
    let (mut spec, expected_draft) = selected_topology_spec(storage, selection)?;
    validate_endpoint_id(new_endpoint).map_err(|error| {
        StoreApiError::new(422, "STORE_REPLACEMENT_ENDPOINT_INVALID", error.to_string())
    })?;
    let previous_bindings = storage
        .api_bindings_for_topology(&spec.topology_id)
        .map_err(storage_error)?;
    let provider_affected = previous_bindings
        .iter()
        .filter(|binding| {
            binding.provider_deployment_id == old_deployment_id
                && binding.desired_state == "ACTIVE"
                && binding.state == ApiBindingState::Active
        })
        .map(|binding| binding.binding_id.clone())
        .collect::<BTreeSet<_>>();
    if provider_affected.is_empty() {
        return Err(StoreApiError::new(
            409,
            "STORE_REPLACEMENT_PROVIDER_BINDINGS_MISSING",
            format!(
                "deployment {old_deployment_id} has no provider binding in topology {}",
                spec.topology_id
            ),
        ));
    }
    let old_targets = previous_bindings
        .iter()
        .filter(|binding| provider_affected.contains(&binding.binding_id))
        .map(|binding| binding.link_target_endpoint.clone())
        .collect::<BTreeSet<_>>();
    if old_targets.len() != 1 {
        return Err(StoreApiError::new(
            409,
            "STORE_REPLACEMENT_PROVIDER_ENDPOINT_AMBIGUOUS",
            "dual-role provider bindings must share one topology endpoint",
        ));
    }
    let old_target = old_targets
        .first()
        .expect("one provider endpoint was checked")
        .clone();
    let old_sources = previous_bindings
        .iter()
        .filter(|binding| binding.consumer_deployment_id == old_deployment_id)
        .map(|binding| binding.link_source_endpoint.clone())
        .collect::<BTreeSet<_>>();
    let old_consumer_requirements = previous_bindings
        .iter()
        .filter(|binding| binding.consumer_deployment_id == old_deployment_id)
        .map(|binding| binding.requirement_name.clone())
        .collect::<BTreeSet<_>>();

    let endpoint_config = json!({
        "deployment_id": new_deployment_id,
        "node_id": new_node_id,
        "outbound_only": false,
    });
    match spec
        .endpoints
        .iter_mut()
        .find(|endpoint| endpoint.endpoint == new_endpoint)
    {
        Some(endpoint) if endpoint.service_id != contract.release.service_name => {
            return Err(StoreApiError::new(
                409,
                "STORE_REPLACEMENT_ENDPOINT_CONFLICT",
                format!("endpoint {new_endpoint} belongs to {}", endpoint.service_id),
            ));
        }
        Some(endpoint) => {
            endpoint.protocol = contract.release.backend.protocol.clone();
            endpoint.health_path = contract.release.backend.health_path.clone();
            endpoint.config = endpoint_config;
        }
        None => spec.endpoints.push(TopologyEndpointSpec {
            endpoint: new_endpoint.to_string(),
            service_id: contract.release.service_name.clone(),
            protocol: contract.release.backend.protocol.clone(),
            health_path: contract.release.backend.health_path.clone(),
            display_name: contract.release.service_name.clone(),
            note: "Store-managed dual-role replacement endpoint".to_string(),
            config: endpoint_config,
        }),
    }

    // Remove only the old consumer's named requirements. Links may aggregate
    // unrelated requirements and must remain intact.
    for link in &mut spec.links {
        if old_sources.contains(&link.source_endpoint) {
            link.api_bindings
                .retain(|binding| !old_consumer_requirements.contains(&binding.requirement_name));
        }
    }
    for binding in consumer_bindings.iter().filter(|binding| {
        binding.desired_state == "ACTIVE"
            && matches!(
                binding.state,
                ApiBindingState::Resolved | ApiBindingState::Active
            )
    }) {
        if !spec
            .endpoints
            .iter()
            .any(|endpoint| endpoint.endpoint == binding.provider_endpoint)
            && binding.provider_deployment_id != old_deployment_id
        {
            return Err(StoreApiError::new(
                422,
                "STORE_BINDING_PROVIDER_ENDPOINT_MISSING",
                format!(
                    "provider endpoint {} for {} is absent from topology {}",
                    binding.provider_endpoint, binding.requirement_name, spec.topology_id
                ),
            ));
        }
        let target_endpoint = if binding.provider_deployment_id == old_deployment_id {
            old_target.as_str()
        } else {
            binding.provider_endpoint.as_str()
        };
        let link = if let Some(index) = spec.links.iter().position(|link| {
            link.source_endpoint == new_endpoint && link.target_endpoint == target_endpoint
        }) {
            &mut spec.links[index]
        } else {
            spec.links.push(TopologyLinkSpec {
                source_endpoint: new_endpoint.to_string(),
                target_endpoint: target_endpoint.to_string(),
                protocol: binding.protocol.clone(),
                auth_mode: "workload".to_string(),
                scope: "api-binding".to_string(),
                enabled: true,
                config_ref: String::new(),
                secret_ref: String::new(),
                policy: json!({}),
                api_bindings: Vec::new(),
            });
            spec.links.last_mut().expect("dual-role link was inserted")
        };
        link.api_bindings.push(TopologyApiBindingSpec {
            requirement_name: binding.requirement_name.clone(),
            api_id: binding.api_id.clone(),
            version: contract
                .requirements()
                .iter()
                .find(|requirement| requirement.binding_name() == binding.requirement_name)
                .map(|requirement| requirement.version_requirement().to_string())
                .unwrap_or_else(|| binding.api_version.clone()),
            optional: binding.optional,
            provider_deployment_id: binding.provider_deployment_id.clone(),
            selection: "explicit".to_string(),
        });
    }

    // Split mixed target Links and move only bindings supplied by the old
    // provider to the transient replacement endpoint.
    let mut rewritten_links = Vec::with_capacity(spec.links.len() + 1);
    for mut link in std::mem::take(&mut spec.links) {
        if link.target_endpoint != old_target {
            rewritten_links.push(link);
            continue;
        }
        let template = link.clone();
        let (mut affected, retained): (Vec<_>, Vec<_>) = link
            .api_bindings
            .into_iter()
            .partition(|binding| binding.provider_deployment_id == old_deployment_id);
        for binding in &mut affected {
            binding.provider_deployment_id = new_deployment_id.to_string();
        }
        match (affected.is_empty(), retained.is_empty()) {
            (true, _) => {
                link.api_bindings = retained;
                rewritten_links.push(link);
            }
            (false, true) => {
                link.target_endpoint = new_endpoint.to_string();
                link.api_bindings = affected;
                rewritten_links.push(link);
            }
            (false, false) => {
                link.api_bindings = retained;
                let mut replacement = template;
                replacement.target_endpoint = new_endpoint.to_string();
                replacement.api_bindings = affected;
                rewritten_links.push(link);
                rewritten_links.push(replacement);
            }
        }
    }
    spec.links = rewritten_links;
    if spec.root_endpoint == old_target || old_sources.contains(&spec.root_endpoint) {
        spec.root_endpoint = new_endpoint.to_string();
    }
    spec.links
        .retain(|link| !link.api_bindings.is_empty() || link.scope != "api-binding");
    if !spec
        .links
        .iter()
        .any(|link| link.source_endpoint == old_target || link.target_endpoint == old_target)
    {
        spec.endpoints
            .retain(|endpoint| endpoint.endpoint != old_target);
    }
    for old_source in &old_sources {
        if old_source != &spec.root_endpoint
            && !spec.links.iter().any(|link| {
                &link.source_endpoint == old_source || &link.target_endpoint == old_source
            })
        {
            spec.endpoints
                .retain(|endpoint| &endpoint.endpoint != old_source);
        }
    }
    spec = spec.canonicalized().map_err(core_error)?;
    let topology_id = spec.topology_id.clone();
    let revision = storage
        .create_next_topology_revision(
            &topology_id,
            &expected_draft,
            spec,
            now_marker(),
            "store-replacement".to_string(),
            format!("replace dual-role deployment {old_deployment_id} with {new_deployment_id}"),
        )
        .map_err(storage_error)?;

    let mut desired = previous_bindings
        .iter()
        .filter(|binding| {
            binding.desired_state == "ACTIVE"
                && binding.state == ApiBindingState::Active
                && binding.consumer_deployment_id != old_deployment_id
        })
        .cloned()
        .collect::<Vec<_>>();
    desired.extend(consumer_bindings.iter().cloned());
    for binding in &mut desired {
        binding.topology_id = topology_id.clone();
        binding.topology_revision_id = revision.revision_id().to_string();
        binding.last_operation_id = operation_id.to_string();
        if binding.consumer_deployment_id == new_deployment_id {
            binding.consumer_endpoint = new_endpoint.to_string();
            binding.link_source_endpoint = new_endpoint.to_string();
        }
        if binding.provider_deployment_id != old_deployment_id {
            continue;
        }
        let provider_api = contract
            .release
            .apis
            .iter()
            .find(|api| {
                api.api_id == binding.api_id
                    && api_version_matches(&binding.api_version, &api.version)
            })
            .ok_or_else(|| {
                StoreApiError::new(
                    422,
                    "STORE_REPLACEMENT_PROVIDER_API_INCOMPATIBLE",
                    format!("replacement release cannot provide {}", binding.api_id),
                )
            })?;
        if !provider_auth_supported(contract.contract_version, &provider_api.auth_mode) {
            return Err(StoreApiError::new(
                422,
                "STORE_BINDING_PROVIDER_AUTH_UNSUPPORTED",
                format!(
                    "replacement provider API {} uses unsupported {} auth",
                    binding.api_id, provider_api.auth_mode
                ),
            ));
        }
        binding.api_version = provider_api.version.clone();
        binding.provider_deployment_id = new_deployment_id.to_string();
        binding.provider_service_id = contract.release.service_name.clone();
        binding.provider_node_id = new_node_id.to_string();
        binding.provider_endpoint = new_endpoint.to_string();
        binding.provider_path = provider_api.path_prefix.clone();
        binding.protocol = provider_api.protocol.clone();
        binding.methods = provider_api.methods.clone();
        binding.provider_auth_mode = provider_api.auth_mode.clone();
        binding.permission = provider_api.permission.clone();
        binding.link_target_endpoint = new_endpoint.to_string();
    }
    let staged_bindings = storage
        .stage_precomputed_topology_api_bindings(
            &topology_id,
            revision.revision_id(),
            operation_id,
            desired,
        )
        .map_err(|error| {
            StoreApiError::new(422, "STORE_BINDING_TOPOLOGY_INVALID", error.to_string())
        })?;
    Ok(StoreTopologyApplyPlan {
        topology_id,
        revision_id: revision.revision_id().to_string(),
        staged_bindings,
        previous_bindings,
    })
}

#[allow(clippy::too_many_arguments)]
fn stage_replacement_consumer_bootstrap(
    storage: &DurableStore,
    plan: &StoreTopologyApplyPlan,
    old_consumer_deployment_id: &str,
    new_consumer_deployment_id: &str,
    new_consumer_endpoint: &str,
    consumer_bindings: &[ApiBinding],
    operation_id: &str,
) -> Result<Vec<ApiBinding>, StoreApiError> {
    let mut desired = plan
        .previous_bindings
        .iter()
        .filter(|binding| {
            binding.desired_state == "ACTIVE"
                && binding.state == ApiBindingState::Active
                && binding.consumer_deployment_id != old_consumer_deployment_id
        })
        .cloned()
        .collect::<Vec<_>>();
    desired.extend(consumer_bindings.iter().cloned());
    for binding in &mut desired {
        binding.topology_id = plan.topology_id.clone();
        binding.topology_revision_id = plan.revision_id.clone();
        binding.last_operation_id = operation_id.to_string();
        if binding.consumer_deployment_id == new_consumer_deployment_id {
            binding.consumer_endpoint = new_consumer_endpoint.to_string();
            binding.link_source_endpoint = new_consumer_endpoint.to_string();
        }
    }
    storage
        .stage_precomputed_topology_api_bindings(
            &plan.topology_id,
            &plan.revision_id,
            operation_id,
            desired,
        )
        .map_err(|error| {
            StoreApiError::new(422, "STORE_BINDING_BOOTSTRAP_INVALID", error.to_string())
        })
}

struct StoreConsumerBindingMergeContext<'a> {
    consumer_deployment_id: &'a str,
    replaced_consumer_deployment_id: Option<&'a str>,
    consumer_endpoint: &'a str,
    topology_id: &'a str,
    revision_id: &'a str,
    operation_id: &'a str,
}

/// Builds the exact immutable Topology shape that a Store install would
/// propose, without creating a revision or touching apply ownership.  The
/// validate endpoint uses this to return a truthful prospective diff.
fn preview_store_install_topology_spec(
    mut spec: TopologySpec,
    contract: &ServiceReleaseContract,
    consumer_deployment_id: &str,
    consumer_node_id: &str,
    consumer_endpoint: &str,
    bindings: &[ApiBinding],
) -> Result<TopologySpec, StoreApiError> {
    let consumer_endpoint = required_text(consumer_endpoint, "endpoint")?;
    validate_endpoint_id(consumer_endpoint).map_err(|error| {
        StoreApiError::new(
            422,
            "STORE_BINDING_CONSUMER_ENDPOINT_INVALID",
            error.to_string(),
        )
    })?;
    let endpoint_config = json!({
        "deployment_id": consumer_deployment_id,
        "node_id": consumer_node_id,
        "outbound_only": true,
    });
    match spec
        .endpoints
        .iter_mut()
        .find(|endpoint| endpoint.endpoint == consumer_endpoint)
    {
        Some(endpoint) if endpoint.service_id != contract.release.service_name => {
            return Err(StoreApiError::new(
                409,
                "STORE_BINDING_CONSUMER_ENDPOINT_CONFLICT",
                format!(
                    "endpoint {consumer_endpoint} already belongs to {}",
                    endpoint.service_id
                ),
            ));
        }
        Some(endpoint) => {
            endpoint.config = endpoint_config;
            endpoint.protocol = contract.release.backend.protocol.clone();
            endpoint.health_path = contract.release.backend.health_path.clone();
        }
        None => spec.endpoints.push(TopologyEndpointSpec {
            endpoint: consumer_endpoint.to_string(),
            service_id: contract.release.service_name.clone(),
            protocol: contract.release.backend.protocol.clone(),
            health_path: contract.release.backend.health_path.clone(),
            display_name: contract.release.service_name.clone(),
            note: "Store-managed outbound workload endpoint".to_string(),
            config: endpoint_config,
        }),
    }
    for link in &mut spec.links {
        if link.source_endpoint == consumer_endpoint {
            link.api_bindings.clear();
        }
    }
    for binding in bindings.iter().filter(|binding| {
        matches!(
            binding.state,
            ApiBindingState::Resolved | ApiBindingState::Active
        ) && binding.desired_state == "ACTIVE"
    }) {
        let target = spec
            .endpoints
            .iter()
            .find(|endpoint| endpoint.endpoint == binding.provider_endpoint)
            .ok_or_else(|| {
                StoreApiError::new(
                    422,
                    "STORE_BINDING_PROVIDER_ENDPOINT_MISSING",
                    format!(
                        "provider deployment {} endpoint {} is not present in topology {}",
                        binding.provider_deployment_id, binding.provider_endpoint, spec.topology_id
                    ),
                )
            })?;
        if target.service_id != binding.provider_service_id {
            return Err(StoreApiError::new(
                422,
                "STORE_BINDING_PROVIDER_ENDPOINT_MISMATCH",
                format!(
                    "provider endpoint {} belongs to {}, expected {}",
                    target.endpoint, target.service_id, binding.provider_service_id
                ),
            ));
        }
        let link = if let Some(index) = spec.links.iter().position(|link| {
            link.source_endpoint == consumer_endpoint
                && link.target_endpoint == binding.provider_endpoint
        }) {
            &mut spec.links[index]
        } else {
            spec.links.push(TopologyLinkSpec {
                source_endpoint: consumer_endpoint.to_string(),
                target_endpoint: binding.provider_endpoint.clone(),
                protocol: binding.protocol.clone(),
                auth_mode: "workload".to_string(),
                scope: "api-binding".to_string(),
                enabled: true,
                config_ref: String::new(),
                secret_ref: String::new(),
                policy: json!({}),
                api_bindings: Vec::new(),
            });
            spec.links.last_mut().expect("link was just inserted")
        };
        if link.protocol != binding.protocol {
            return Err(StoreApiError::new(
                422,
                "STORE_BINDING_LINK_PROTOCOL_CONFLICT",
                format!(
                    "Link {} -> {} uses {}, but requirement {} needs {}",
                    consumer_endpoint,
                    binding.provider_endpoint,
                    link.protocol,
                    binding.requirement_name,
                    binding.protocol
                ),
            ));
        }
        link.enabled = true;
        link.auth_mode = "workload".to_string();
        let version_requirement = contract
            .requirements()
            .iter()
            .find(|requirement| requirement.binding_name() == binding.requirement_name)
            .map(|requirement| requirement.version_requirement())
            .filter(|version| !version.trim().is_empty())
            .unwrap_or(binding.api_version.as_str());
        link.api_bindings.push(TopologyApiBindingSpec {
            requirement_name: binding.requirement_name.clone(),
            api_id: binding.api_id.clone(),
            version: version_requirement.to_string(),
            optional: binding.optional,
            provider_deployment_id: binding.provider_deployment_id.clone(),
            selection: "explicit".to_string(),
        });
    }
    spec.links
        .retain(|link| !link.api_bindings.is_empty() || link.scope != "api-binding");
    spec.canonicalized().map_err(core_error)
}

fn merge_store_consumer_bindings(
    previous_bindings: &[ApiBinding],
    consumer_bindings: &[ApiBinding],
    context: StoreConsumerBindingMergeContext<'_>,
) -> Vec<ApiBinding> {
    let mut merged = previous_bindings
        .iter()
        .filter(|binding| {
            binding.desired_state == "ACTIVE"
                && binding.state == ApiBindingState::Active
                && binding.consumer_deployment_id != context.consumer_deployment_id
                && context
                    .replaced_consumer_deployment_id
                    .is_none_or(|old| binding.consumer_deployment_id != old)
                && binding.link_source_endpoint != context.consumer_endpoint
        })
        .cloned()
        .collect::<Vec<_>>();
    merged.extend(consumer_bindings.iter().cloned());
    for binding in &mut merged {
        binding.topology_id = context.topology_id.to_string();
        binding.topology_revision_id = context.revision_id.to_string();
        binding.last_operation_id = context.operation_id.to_string();
        if binding.consumer_deployment_id == context.consumer_deployment_id {
            binding.link_source_endpoint = context.consumer_endpoint.to_string();
            binding.link_target_endpoint = binding.provider_endpoint.clone();
        }
    }
    merged
}

#[allow(clippy::too_many_arguments)]
fn propose_store_install_topology(
    storage: &DurableStore,
    selection: &InstallTopologySelection,
    contract: &ServiceReleaseContract,
    consumer_deployment_id: &str,
    consumer_node_id: &str,
    consumer_endpoint: &str,
    bindings: &[ApiBinding],
    operation_id: &str,
    replaced_consumer_deployment_id: Option<&str>,
) -> Result<StoreTopologyApplyPlan, StoreApiError> {
    let (spec, expected_draft) = selected_topology_spec(storage, selection)?;
    let spec = preview_store_install_topology_spec(
        spec,
        contract,
        consumer_deployment_id,
        consumer_node_id,
        consumer_endpoint,
        bindings,
    )?;
    let consumer_endpoint = required_text(consumer_endpoint, "endpoint")?;
    let topology_id = spec.topology_id.clone();
    let revision = storage
        .create_next_topology_revision(
            &topology_id,
            &expected_draft,
            spec,
            now_marker(),
            "store-install".to_string(),
            format!(
                "bind {} deployment {}",
                contract.release.service_name, consumer_deployment_id
            ),
        )
        .map_err(storage_error)?;
    let previous_bindings = storage
        .api_bindings_for_topology(&revision.spec().topology_id)
        .map_err(storage_error)?;
    // A Store edit owns only the selected consumer. Preserve every other
    // active consumer in the topology so staging a new deployment cannot
    // accidentally revoke unrelated routes.
    let annotated = merge_store_consumer_bindings(
        &previous_bindings,
        bindings,
        StoreConsumerBindingMergeContext {
            consumer_deployment_id,
            replaced_consumer_deployment_id,
            consumer_endpoint,
            topology_id: &revision.spec().topology_id,
            revision_id: revision.revision_id(),
            operation_id,
        },
    );
    let staged_bindings = storage
        .stage_precomputed_topology_api_bindings(
            &revision.spec().topology_id,
            revision.revision_id(),
            operation_id,
            annotated,
        )
        .map_err(|error| {
            StoreApiError::new(422, "STORE_BINDING_TOPOLOGY_INVALID", error.to_string())
        })?;
    Ok(StoreTopologyApplyPlan {
        topology_id: revision.spec().topology_id.clone(),
        revision_id: revision.revision_id().to_string(),
        staged_bindings,
        previous_bindings,
    })
}

fn provider_candidates(
    console: &OrchestratorActionConsole,
    storage: &DurableStore,
) -> Result<Vec<ApiProviderCandidate>, StoreApiError> {
    let mut contracts = BTreeMap::new();
    for record in console.service_releases().map_err(core_error)? {
        let Ok(contract) = ServiceReleaseContract::from_json_value(record.manifest.clone()) else {
            continue;
        };
        contracts.insert((record.service_name, record.version), contract);
    }
    let mut candidates = Vec::new();
    let evidence_at_ms = now_ms();
    for stored in storage.runtime_instances(None).map_err(storage_error)? {
        let stored = storage
            .runtime_with_current_evidence(stored, evidence_at_ms)
            .map_err(storage_error)?;
        let healthy = stored.instance.observed_state == RuntimeObservedState::Running
            && stored.instance.health.eq_ignore_ascii_case("HEALTHY");
        let managed_observation_ready =
            if stored.management_mode == orchestrator_storage::RuntimeManagementMode::Managed {
                stored.instance.runtime_attested
                    && stored.drift_reason.is_empty()
                    && storage
                        .managed_runtime_report_unavailable_reason(&stored, evidence_at_ms)
                        .map_err(storage_error)?
                        .is_none()
            } else {
                stored.drift_reason.is_empty()
            };
        if !healthy || !managed_observation_ready || stored.endpoint.trim().is_empty() {
            continue;
        }
        let Some(contract) = contracts.get(&(
            stored.instance.service_id.clone(),
            stored.instance.release_version.clone(),
        )) else {
            continue;
        };
        for api in &contract.release.apis {
            candidates.push(ApiProviderCandidate {
                deployment_id: stored.instance.deployment_id.clone(),
                service_id: stored.instance.service_id.clone(),
                node_id: stored.node_id.clone(),
                endpoint: stored.endpoint.clone(),
                path: api.path_prefix.clone(),
                api_id: api.api_id.clone(),
                api_version: api.version.clone(),
                protocol: api.protocol.clone(),
                methods: api.methods.clone(),
                auth_mode: api.auth_mode.clone(),
                permission: api.permission.clone(),
                healthy,
            });
        }
    }
    Ok(candidates)
}

#[allow(clippy::too_many_arguments)]
fn unbound_binding(
    deployment_id: &str,
    service_id: &str,
    node_id: &str,
    endpoint: &str,
    name: &str,
    api_id: &str,
    version: &str,
    reason: &str,
) -> ApiBinding {
    unresolved_binding(
        deployment_id,
        service_id,
        node_id,
        endpoint,
        name,
        api_id,
        version,
        true,
        ApiBindingState::Unbound,
        reason,
    )
}

#[allow(clippy::too_many_arguments)]
fn unresolved_binding(
    deployment_id: &str,
    service_id: &str,
    node_id: &str,
    endpoint: &str,
    name: &str,
    api_id: &str,
    version: &str,
    optional: bool,
    state: ApiBindingState,
    reason: &str,
) -> ApiBinding {
    let now = now_marker();
    ApiBinding {
        binding_id: binding_id(deployment_id, name),
        requirement_name: name.to_string(),
        api_id: api_id.to_string(),
        api_version: version.to_string(),
        consumer_deployment_id: deployment_id.to_string(),
        consumer_service_id: service_id.to_string(),
        consumer_node_id: node_id.to_string(),
        consumer_endpoint: endpoint.to_string(),
        provider_deployment_id: String::new(),
        provider_service_id: String::new(),
        provider_node_id: String::new(),
        provider_endpoint: String::new(),
        provider_path: String::new(),
        virtual_endpoint: format!("/internal/apis/{api_id}"),
        protocol: String::new(),
        methods: Vec::new(),
        auth_mode: String::new(),
        provider_auth_mode: String::new(),
        permission: String::new(),
        timeout_ms: None,
        topology_id: String::new(),
        topology_revision_id: String::new(),
        link_source_endpoint: String::new(),
        link_target_endpoint: String::new(),
        credential_ref: String::new(),
        credential_generation: 1,
        context_generation: 1,
        desired_state: ApiBindingDesiredState::Active,
        observed_state: match state {
            ApiBindingState::Unbound => ApiBindingObservedState::Revoked,
            ApiBindingState::Error => ApiBindingObservedState::Error,
            _ => ApiBindingObservedState::Pending,
        },
        health: ApiBindingHealth::Unknown,
        drift: Vec::new(),
        last_operation_id: String::new(),
        state,
        optional,
        reason: reason.to_string(),
        created_at: now.clone(),
        updated_at: now,
    }
}

fn binding_id(deployment_id: &str, requirement_name: &str) -> String {
    let digest = Sha256::digest(format!("{deployment_id}\0{requirement_name}").as_bytes());
    format!("binding-{digest:x}")
}

fn now_marker() -> String {
    format!("unix-ms:{}", now_ms())
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
    contract: &ServiceReleaseContract,
    install: &RuntimeInstallPayload,
    api_bindings: &[ApiBinding],
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
    let resource_claims = build_resource_claim_steps(contract, install, node)?;
    let materialization =
        build_runtime_materialization(release, node, requested_config, requested_secret_refs)?;

    let mut provisioners = Vec::new();
    let mut redis_resources = Vec::new();
    if !release.redis.is_empty() {
        let connection_id = provider_identifier(node, "redis", "connection_id")?;
        redis_resources.extend(release.redis.iter().map(|resource| RedisNamespaceSpec {
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
        }));
    }
    if contract.contract_version >= 2 && !contract.events.subscribes.is_empty() {
        let connection_id = provider_identifier(node, "redis", "connection_id")?;
        let groups = contract
            .events
            .subscribes
            .iter()
            .map(|event| event.consumer_group().to_string())
            .collect::<BTreeSet<_>>();
        redis_resources.extend(groups.into_iter().map(|consumer_group| RedisNamespaceSpec {
            name: format!("event-group-{}", provider_token(&consumer_group)),
            kind: "consumer-group".to_string(),
            connection_id: connection_id.clone(),
            namespace: MANAGED_EVENT_STREAM_V1.to_string(),
            consumer_group,
        }));
    }
    redis_resources.sort_by(|left, right| left.name.cmp(&right.name));
    if !redis_resources.is_empty() {
        provisioners.push(TypedProvisionerStep::Redis {
            service_name: release.service_name.clone(),
            resources: redis_resources,
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
    // API surfaces are part of the signed Release record and ApiBinding
    // projection in orchestrator-storage. They are never provisioned through
    // an external registry service, including for normalized v1 manifests.
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

    let auth = if contract.contract_version < 2
        && (!release.permissions.is_empty()
            || !release.service_identity.allowed_apis.is_empty()
            || !release.service_identity.service_name.is_empty())
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
            resource_claims: migration_resource_claims(migration, &resource_claims)?,
            timeout_ms: oci.timeout_ms,
            dry_run: migration_policy == MigrationPolicyV2::DryRun,
        });
    }

    let routed_bindings = api_bindings
        .iter()
        .filter(|binding| {
            matches!(
                binding.state,
                ApiBindingState::Resolved | ApiBindingState::Active
            ) && binding.desired_state == "ACTIVE"
                && binding.topology_id.is_empty()
        })
        .collect::<Vec<_>>();
    let gateway = if contract.contract_version >= 2 {
        None
    } else if release.routes.is_empty() && routed_bindings.is_empty() {
        if !gateway_node_id.trim().is_empty() {
            return Err(StoreApiError::new(
                422,
                "STORE_GATEWAY_NODE_UNUSED",
                "gateway_node_id cannot be set when the release declares no routes or API bindings",
            ));
        }
        None
    } else {
        require_node_provider(node, "gateway")?;
        let gateway_node_id = required_text(gateway_node_id, "gateway_node_id")?;
        if !release.routes.is_empty() && node.host_ip.trim().is_empty() {
            return Err(StoreApiError::new(
                422,
                "STORE_NODE_HOST_REQUIRED",
                "target Node must advertise host_ip before Gateway routes can be published",
            ));
        }
        if !release.routes.is_empty()
            && !matches!(release.backend.protocol.as_str(), "http" | "https")
        {
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
        let mut routes = Vec::with_capacity(release.routes.len() + routed_bindings.len());
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
                api_id: String::new(),
                binding_id: String::new(),
                consumer_deployment_id: String::new(),
                credential_generation: 1,
                timeout_ms: 30_000,
                provider_node_id: node.node_id.clone(),
                provider_endpoint: String::new(),
                strip_prefix: false,
                rewrite_prefix: String::new(),
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
        for binding in routed_bindings {
            if !matches!(binding.protocol.as_str(), "http" | "https") {
                return Err(StoreApiError::new(
                    422,
                    "STORE_BINDING_GATEWAY_PROTOCOL_UNSUPPORTED",
                    format!(
                        "API binding {} uses unsupported provider protocol {}",
                        binding.requirement_name, binding.protocol
                    ),
                ));
            }
            validate_endpoint_id(&binding.provider_endpoint).map_err(|error| {
                StoreApiError::new(
                    422,
                    "STORE_BINDING_PROVIDER_ENDPOINT_INVALID",
                    format!(
                        "API binding {} provider endpoint is invalid: {error}",
                        binding.requirement_name
                    ),
                )
            })?;
            let identity = parse_endpoint_id(&binding.provider_endpoint).map_err(|error| {
                StoreApiError::new(
                    422,
                    "STORE_BINDING_PROVIDER_ENDPOINT_INVALID",
                    format!(
                        "API binding {} provider endpoint is invalid: {error}",
                        binding.requirement_name
                    ),
                )
            })?;
            let provider_host = identity.host.parse::<IpAddr>().map_err(|_| {
                StoreApiError::new(
                    422,
                    "STORE_BINDING_PROVIDER_ENDPOINT_INVALID",
                    format!(
                        "API binding {} provider endpoint host must be an IP address",
                        binding.requirement_name
                    ),
                )
            })?;
            let provider_host = match provider_host {
                IpAddr::V4(address) => address.to_string(),
                IpAddr::V6(address) => format!("[{address}]"),
            };
            routes.push(GatewayRouteSpec {
                route_id: binding.binding_id.clone(),
                path_prefix: binding.virtual_endpoint.clone(),
                upstream_base: format!(
                    "{}://{}:{}",
                    binding.protocol, provider_host, identity.port
                ),
                api_id: binding.api_id.clone(),
                binding_id: binding.binding_id.clone(),
                consumer_deployment_id: binding.consumer_deployment_id.clone(),
                credential_generation: binding.credential_generation,
                timeout_ms: binding.timeout_ms.unwrap_or(30_000),
                provider_node_id: binding.provider_node_id.clone(),
                provider_endpoint: binding.provider_endpoint.clone(),
                strip_prefix: true,
                rewrite_prefix: binding.provider_path.clone(),
                methods: binding.methods.clone(),
                auth_mode: "workload".to_string(),
                required_permission: binding.permission.clone(),
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
        && resource_claims.is_empty()
        && provisioners.is_empty()
        && migrations.is_empty()
        && gateway.is_none()
    {
        Ok(None)
    } else {
        Ok(Some(ReleasePipelinePayload {
            install: install.clone(),
            resource_claims,
            materialization,
            auth,
            provisioners,
            migrations,
            gateway,
        }))
    }
}

fn build_resource_claim_steps(
    contract: &ServiceReleaseContract,
    install: &RuntimeInstallPayload,
    node: &NodeRecord,
) -> Result<Vec<ResourceClaimStepV1>, StoreApiError> {
    let Some(platform) = contract.platform.as_ref() else {
        return Ok(Vec::new());
    };
    if platform.resource_claims.is_empty() {
        return Ok(Vec::new());
    }
    let provider_id = provider_identifier(node, "postgresql", "provider_id")
        .or_else(|_| provider_identifier(node, "postgresql", "connection_id"))?;
    let mut claims = platform
        .resource_claims
        .iter()
        .map(|resource| {
            if resource.resource_type != "postgresql.database/v1" {
                return Err(StoreApiError::new(
                    422,
                    "STORE_RESOURCE_TYPE_UNSUPPORTED",
                    format!(
                        "resource {} declares unsupported type {}; v1 implements postgresql.database/v1",
                        resource.name, resource.resource_type
                    ),
                ));
            }
            if !resource.lifecycle.eq_ignore_ascii_case("retain") {
                return Err(StoreApiError::new(
                    422,
                    "STORE_RESOURCE_LIFECYCLE_INVALID",
                    format!(
                        "resource {} must use RETAIN; deletion requires a separate audited purge",
                        resource.name
                    ),
                ));
            }
            let step = ResourceClaimStepV1 {
                claim_id: stable_resource_claim_id(&install.spec.service_id, &resource.name),
                owner_instance_id: stable_service_instance_id(&install.spec.service_id),
                deployment_id: install.spec.deployment_id.clone(),
                service_id: install.spec.service_id.clone(),
                resource_name: resource.name.clone(),
                resource_type: resource.resource_type.clone(),
                // Resource generation describes the durable resource spec, not
                // the replaceable runtime container. postgresql.database/v1
                // has no mutable resource shape in this release, so upgrades,
                // rollbacks, and rescheduling must keep generation 1 and reuse
                // the exact same claim.
                generation: 1,
                provider_id: provider_id.clone(),
                output_path_environment: resource_output_environment(&resource.name),
            };
            step.validate().map_err(|error| {
                StoreApiError::new(
                    422,
                    "STORE_RESOURCE_CLAIM_INVALID",
                    format!("resource {} could not be materialized: {error}", resource.name),
                )
            })?;
            Ok(step)
        })
        .collect::<Result<Vec<_>, StoreApiError>>()?;
    claims.sort_by(|left, right| left.resource_name.cmp(&right.resource_name));
    Ok(claims)
}

fn migration_resource_claims(
    migration: &orchestrator_legacy::ReleaseMigrationDecl,
    claims: &[ResourceClaimStepV1],
) -> Result<Vec<String>, StoreApiError> {
    let requested = migration
        .oci
        .as_ref()
        .and_then(|oci| oci.env.get("OJOS_RESOURCE_CLAIM"))
        .map(|value| value.trim())
        .filter(|value| !value.is_empty());
    match requested {
        Some(name) => claims
            .iter()
            .find(|claim| claim.resource_name == name)
            .map(|claim| vec![claim.resource_name.clone()])
            .ok_or_else(|| {
                StoreApiError::new(
                    422,
                    "STORE_MIGRATION_RESOURCE_UNKNOWN",
                    format!(
                        "migration {} references undeclared resource {name}",
                        migration.version
                    ),
                )
            }),
        None if claims.len() == 1 => Ok(vec![claims[0].resource_name.clone()]),
        None => Ok(Vec::new()),
    }
}

fn stable_resource_claim_id(service_id: &str, resource_name: &str) -> String {
    let owner_instance_id = stable_service_instance_id(service_id);
    let digest = Sha256::digest(format!("{owner_instance_id}\0{resource_name}").as_bytes());
    format!("claim-{digest:x}")
}

/// Store v1 has one installation slot for each service in the default scope.
/// This identity deliberately excludes release, deployment, and Node so a
/// retained resource survives upgrades, rollbacks, and rescheduling. Explicit
/// multi-instance support must add a persisted slot id instead of changing this
/// derivation implicitly.
fn stable_service_instance_id(service_id: &str) -> String {
    let digest = Sha256::digest(format!("default\0{service_id}").as_bytes());
    format!("service-instance-{digest:x}")
}

fn resource_output_environment(resource_name: &str) -> String {
    let token = resource_name
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() {
                character.to_ascii_uppercase()
            } else {
                '_'
            }
        })
        .collect::<String>();
    format!("OJOS_RESOURCE_{token}_OUTPUT_FILE")
}

fn build_runtime_materialization(
    release: &ServiceReleaseManifest,
    node: &NodeRecord,
    requested_config: &Value,
    requested_secret_refs: &BTreeMap<String, String>,
) -> Result<Option<RuntimeMaterializationStep>, StoreApiError> {
    let (config, schema_secrets, schema_controls_requiredness) = validate_release_config(
        &release.config_schema,
        requested_config,
        requested_secret_refs,
    )?;
    let mut allowed_secrets = release.secrets.iter().cloned().collect::<BTreeSet<_>>();
    allowed_secrets.extend(schema_secrets.iter().cloned());
    let mut required_secrets = if schema_controls_requiredness {
        BTreeSet::new()
    } else {
        allowed_secrets.clone()
    };
    if !schema_controls_requiredness {
        required_secrets.extend(schema_secrets);
    }
    let supplied = requested_secret_refs
        .keys()
        .cloned()
        .collect::<BTreeSet<_>>();
    let missing = required_secrets
        .difference(&supplied)
        .cloned()
        .collect::<Vec<_>>();
    let unknown = supplied
        .difference(&allowed_secrets)
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
        let environment_key = format!("OJOS_CONFIG_{}", environment_token(key));
        environment_templates
            .entry(environment_key)
            .or_insert_with(|| format!("${{config.{key}}}"));
    }
    for key in &supplied {
        let environment_key = format!("OJOS_SECRET_{}", environment_token(key));
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

fn environment_token(value: &str) -> String {
    value
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() {
                character.to_ascii_uppercase()
            } else {
                '_'
            }
        })
        .collect()
}

type ValidatedReleaseConfig = (BTreeMap<String, String>, BTreeSet<String>, bool);

fn validate_release_config(
    schema: &Value,
    requested: &Value,
    requested_secret_refs: &BTreeMap<String, String>,
) -> Result<ValidatedReleaseConfig, StoreApiError> {
    if schema.get("$schema").is_some() {
        let (config, secrets) =
            validate_json_schema_config(schema, requested, requested_secret_refs)?;
        return Ok((config, secrets, true));
    }
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
                return Ok((BTreeMap::new(), BTreeSet::new(), false));
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
    Ok((output, secrets, false))
}

fn validate_json_schema_config(
    schema: &Value,
    requested: &Value,
    requested_secret_refs: &BTreeMap<String, String>,
) -> Result<(BTreeMap<String, String>, BTreeSet<String>), StoreApiError> {
    let requested = match requested {
        Value::Null => serde_json::Map::new(),
        Value::Object(requested) => requested.clone(),
        _ => {
            return Err(StoreApiError::new(
                422,
                "STORE_CONFIG_INVALID",
                "config must be a JSON object",
            ));
        }
    };
    reject_unsupported_config_schema_keywords(schema)?;
    let mut secret_paths = BTreeSet::new();
    collect_config_secret_paths(schema, "", &mut secret_paths)?;

    for path in &secret_paths {
        if json_path(&requested, path).is_some() {
            return Err(StoreApiError::new(
                422,
                "STORE_SECRET_VALUE_FORBIDDEN",
                format!("config field {path} is secret; submit only secret_refs.{path}"),
            ));
        }
    }

    // Secret references participate in conditional validation as opaque
    // placeholders. The reference itself is never placed in the config map or
    // exposed to schema expressions.
    let unknown_secret_refs = requested_secret_refs
        .keys()
        .filter(|path| !secret_paths.contains(*path))
        .cloned()
        .collect::<Vec<_>>();
    if !unknown_secret_refs.is_empty() {
        return Err(StoreApiError::new(
            422,
            "STORE_SECRET_REFS_INVALID",
            format!(
                "secret_refs contains undeclared JSON Schema field(s): {}",
                unknown_secret_refs.join(", ")
            ),
        ));
    }
    let mut instance = Value::Object(requested.clone());
    for path in requested_secret_refs.keys() {
        insert_json_path(&mut instance, path, Value::String("opaque".to_string()))?;
    }
    let mut validation_schema = schema.clone();
    relax_config_secret_value_constraints(&mut validation_schema);
    let validator = jsonschema::options()
        .with_draft(jsonschema::Draft::Draft202012)
        .should_validate_formats(true)
        .build(&validation_schema)
        .map_err(|error| {
            StoreApiError::new(
                422,
                "STORE_CONFIG_SCHEMA_INVALID",
                format!("compile signed JSON Schema 2020-12: {error}"),
            )
        })?;
    let errors = validator
        .iter_errors(&instance)
        .take(8)
        .map(|error| error.to_string())
        .collect::<Vec<_>>();
    if !errors.is_empty() {
        return Err(StoreApiError::new(
            422,
            "STORE_CONFIG_INVALID",
            format!(
                "config does not satisfy signed JSON Schema: {}",
                errors.join("; ")
            ),
        ));
    }

    let mut output = BTreeMap::new();
    flatten_config_scalars("", &Value::Object(requested), &mut output)?;
    Ok((output, secret_paths))
}

fn relax_config_secret_value_constraints(schema: &mut Value) {
    match schema {
        Value::Object(object) => {
            let secret = object.get("writeOnly").and_then(Value::as_bool) == Some(true)
                && object.get("x-ojos-secret").and_then(Value::as_bool) == Some(true);
            if secret {
                object.retain(|key, _| {
                    matches!(
                        key.as_str(),
                        "type" | "writeOnly" | "x-ojos-secret" | "title" | "description"
                    )
                });
                object.insert("type".to_string(), Value::String("string".to_string()));
                return;
            }
            for child in object.values_mut() {
                relax_config_secret_value_constraints(child);
            }
        }
        Value::Array(values) => {
            for value in values {
                relax_config_secret_value_constraints(value);
            }
        }
        _ => {}
    }
}

fn reject_unsupported_config_schema_keywords(schema: &Value) -> Result<(), StoreApiError> {
    fn visit(value: &Value) -> Result<(), StoreApiError> {
        match value {
            Value::Object(object) => {
                for (key, child) in object {
                    if matches!(
                        key.as_str(),
                        "unevaluatedProperties"
                            | "patternProperties"
                            | "propertyNames"
                            | "contains"
                            | "prefixItems"
                    ) {
                        return Err(StoreApiError::new(
                            422,
                            "STORE_CONFIG_SCHEMA_INVALID",
                            format!(
                                "JSON Schema keyword {key} is outside the supported configuration subset"
                            ),
                        ));
                    }
                    if key == "$ref"
                        && !child.as_str().is_some_and(|reference| {
                            reference.starts_with("#/") || reference.starts_with("sha256:")
                        })
                    {
                        return Err(StoreApiError::new(
                            422,
                            "STORE_CONFIG_SCHEMA_INVALID",
                            "JSON Schema $ref must be local or digest-pinned",
                        ));
                    }
                    visit(child)?;
                }
            }
            Value::Array(values) => {
                for value in values {
                    visit(value)?;
                }
            }
            _ => {}
        }
        Ok(())
    }
    visit(schema)
}

fn collect_config_secret_paths(
    schema: &Value,
    prefix: &str,
    output: &mut BTreeSet<String>,
) -> Result<(), StoreApiError> {
    if let Some(properties) = schema.get("properties").and_then(Value::as_object) {
        for (name, declaration) in properties {
            let path = if prefix.is_empty() {
                name.clone()
            } else {
                format!("{prefix}.{name}")
            };
            let secret = declaration.get("writeOnly").and_then(Value::as_bool) == Some(true)
                && declaration.get("x-ojos-secret").and_then(Value::as_bool) == Some(true);
            if secret {
                output.insert(path);
            } else {
                collect_config_secret_paths(declaration, &path, output)?;
            }
        }
    }
    for keyword in ["allOf", "anyOf", "oneOf"] {
        if let Some(branches) = schema.get(keyword).and_then(Value::as_array) {
            for branch in branches {
                collect_config_secret_paths(branch, prefix, output)?;
            }
        }
    }
    for keyword in ["if", "then", "else", "not"] {
        if let Some(branch) = schema.get(keyword) {
            collect_config_secret_paths(branch, prefix, output)?;
        }
    }
    Ok(())
}

fn json_path<'a>(root: &'a serde_json::Map<String, Value>, path: &str) -> Option<&'a Value> {
    let mut value = root.get(path.split('.').next()?)?;
    for segment in path.split('.').skip(1) {
        value = value.as_object()?.get(segment)?;
    }
    Some(value)
}

fn insert_json_path(root: &mut Value, path: &str, value: Value) -> Result<(), StoreApiError> {
    let mut segments = path.split('.').peekable();
    let mut current = root;
    while let Some(segment) = segments.next() {
        if segments.peek().is_none() {
            current
                .as_object_mut()
                .ok_or_else(|| {
                    StoreApiError::new(
                        422,
                        "STORE_CONFIG_INVALID",
                        format!("config parent for secret {path} must be an object"),
                    )
                })?
                .entry(segment.to_string())
                .or_insert(value.clone());
            return Ok(());
        }
        let Some(object) = current.as_object_mut() else {
            return Err(StoreApiError::new(
                422,
                "STORE_CONFIG_INVALID",
                format!("config parent for secret {path} must be an object"),
            ));
        };
        current = object
            .entry(segment.to_string())
            .or_insert_with(|| Value::Object(serde_json::Map::new()));
    }
    Ok(())
}

fn flatten_config_scalars(
    prefix: &str,
    value: &Value,
    output: &mut BTreeMap<String, String>,
) -> Result<(), StoreApiError> {
    match value {
        Value::Object(object) => {
            for (name, value) in object {
                let path = if prefix.is_empty() {
                    name.clone()
                } else {
                    format!("{prefix}.{name}")
                };
                flatten_config_scalars(&path, value, output)?;
            }
            Ok(())
        }
        Value::String(_) | Value::Bool(_) | Value::Number(_) => {
            output.insert(prefix.to_string(), scalar_config_value(prefix, value)?);
            Ok(())
        }
        Value::Null => Ok(()),
        _ => Err(StoreApiError::new(
            422,
            "STORE_CONFIG_TYPE_INVALID",
            format!("config field {prefix} must be a scalar or nested object"),
        )),
    }
}

#[cfg(test)]
fn conditional_config_schema_fixture() -> Value {
    json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "registration": {
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "mode": {"enum": ["open", "invite-only"]},
                    "inviteSigningKey": {
                        "type": "string",
                        "minLength": 32,
                        "writeOnly": true,
                        "x-ojos-secret": true
                    }
                },
                "required": ["mode"],
                "if": {
                    "properties": {"mode": {"const": "invite-only"}},
                    "required": ["mode"]
                },
                "then": {"required": ["inviteSigningKey"]},
                "else": {"not": {"required": ["inviteSigningKey"]}}
            }
        },
        "required": ["registration"]
    })
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

fn ensure_ready_docker_node(
    storage: &DurableStore,
    node: &NodeRecord,
) -> Result<(), StoreApiError> {
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
    let facts = node_runtime_facts(storage, &node.node_id)?;
    if facts.docker.engine != "docker" {
        return Err(StoreApiError::new(
            422,
            "STORE_DOCKER_CAPABILITY_REQUIRED",
            format!(
                "target Node {} latest authenticated runtime facts do not report Docker Engine",
                node.node_id
            ),
        ));
    }
    Ok(())
}

fn target_platform(
    storage: &DurableStore,
    node: &NodeRecord,
) -> Result<TargetPlatform, StoreApiError> {
    let facts = node_runtime_facts(storage, &node.node_id)?;
    let os = facts.docker.os_type.trim();
    let arch = facts.docker.architecture.trim();
    if !valid_platform_token(os) || !valid_platform_token(arch) {
        return Err(StoreApiError::new(
            422,
            "STORE_TARGET_PLATFORM_INVALID",
            format!(
                "target Node {} authenticated runtime facts contain an invalid platform",
                node.node_id
            ),
        ));
    }
    Ok(TargetPlatform::new(normalize_os(os), normalize_arch(arch)))
}

fn node_runtime_facts(
    storage: &DurableStore,
    node_id: &str,
) -> Result<NodeRuntimeFactsV1, StoreApiError> {
    let stored = storage
        .node_runtime_facts(node_id)
        .map_err(storage_error)?
        .ok_or_else(|| {
            StoreApiError::new(
                422,
                "STORE_NODE_RUNTIME_FACTS_REQUIRED",
                format!("target Node {node_id} has not submitted authenticated runtime facts"),
            )
        })?;
    let now = now_ms();
    if stored.is_stale_at(now, NODE_RUNTIME_FACTS_STALE_MS) {
        return Err(StoreApiError::new(
            409,
            "STORE_NODE_RUNTIME_FACTS_STALE",
            format!(
                "target Node {node_id} runtime facts are older than {} seconds",
                NODE_RUNTIME_FACTS_STALE_MS / 1_000
            ),
        ));
    }
    serde_json::from_value(stored.facts).map_err(|error| {
        StoreApiError::new(
            500,
            "STORE_NODE_RUNTIME_FACTS_INVALID",
            format!("target Node {node_id} runtime facts cannot be decoded: {error}"),
        )
    })
}

fn release_runtime_contract(
    contract: &ServiceReleaseContract,
) -> Result<RuntimeContract, StoreApiError> {
    if contract.contract_version == 1 {
        return Ok(RuntimeContract::standard_v1());
    }
    let id = match contract.runtime_contract.id.as_str() {
        "standard-container-v1" => RuntimeProfile::StandardV1,
        "judge-sandbox-v1" => RuntimeProfile::JudgeSandboxV1,
        other => {
            return Err(StoreApiError::new(
                422,
                "STORE_RUNTIME_CONTRACT_UNSUPPORTED",
                format!("release selects unknown runtime contract {other}"),
            ));
        }
    };
    let selected = RuntimeContract {
        id,
        profile_sha256: contract.runtime_contract.sha256.clone(),
    };
    selected.validate().map_err(|error| {
        StoreApiError::new(
            422,
            "STORE_RUNTIME_CONTRACT_DIGEST_MISMATCH",
            error.to_string(),
        )
    })?;
    Ok(selected)
}

fn ensure_release_runtime_supported(
    storage: &DurableStore,
    node: &NodeRecord,
    contract: &ServiceReleaseContract,
    image: &str,
) -> Result<RuntimeContract, StoreApiError> {
    let requested = release_runtime_contract(contract)?;
    let facts = node_runtime_facts(storage, &node.node_id)?;
    if !facts
        .allowed_contracts
        .iter()
        .any(|allowed| allowed == &requested)
    {
        return Err(StoreApiError::new(
            422,
            "STORE_RUNTIME_CONTRACT_NOT_ALLOWED",
            format!(
                "target Node {} authenticated runtime facts do not allow {} with digest {}",
                node.node_id, requested.id, requested.profile_sha256
            ),
        ));
    }
    if requested.id == RuntimeProfile::JudgeSandboxV1
        && !facts
            .judge_sandbox_allowed_images
            .iter()
            .any(|allowed| allowed == image)
    {
        return Err(StoreApiError::new(
            422,
            "STORE_RUNTIME_ARTIFACT_NOT_ALLOWED",
            format!(
                "target Node {} local judge-sandbox-v1 policy does not authorize exact artifact {image}",
                node.node_id
            ),
        ));
    }
    Ok(requested)
}

fn host_platform() -> TargetPlatform {
    TargetPlatform::new(
        normalize_os(std::env::consts::OS),
        normalize_arch(std::env::consts::ARCH),
    )
}

fn managed_service_context_spec(
    storage: &DurableStore,
    contract: &ServiceReleaseContract,
    node_id: &str,
    bindings: &[ApiBinding],
    mount_unbound_optional_context: bool,
) -> Result<Option<ManagedServiceContextSpec>, StoreApiError> {
    let has_events =
        !contract.events.publishes.is_empty() || !contract.events.subscribes.is_empty();
    let has_retained_volume = contract_has_retained_runtime_volume(contract);
    let provides_workload_api = contract.platform.is_some()
        && contract
            .release
            .apis
            .iter()
            .any(|api| api.auth_mode == "workload");
    if contract.requirements().is_empty()
        && !has_events
        && !has_retained_volume
        && !provides_workload_api
    {
        return Ok(None);
    }
    let included = bindings
        .iter()
        .filter(|binding| {
            matches!(
                binding.state,
                ApiBindingState::Resolved | ApiBindingState::Active
            ) && binding.desired_state == "ACTIVE"
        })
        .collect::<Vec<_>>();
    let included_requirements = included
        .iter()
        .map(|binding| binding.requirement_name.as_str())
        .collect::<BTreeSet<_>>();
    let missing_required = contract
        .requirements()
        .iter()
        .filter(|requirement| {
            !requirement.optional() && !included_requirements.contains(requirement.binding_name())
        })
        .map(|requirement| requirement.binding_name().to_string())
        .collect::<BTreeSet<_>>();
    if !missing_required.is_empty() {
        return Err(StoreApiError::new(
            409,
            "STORE_REQUIRED_BINDING_CONTEXT_MISSING",
            format!(
                "required APIs cannot be materialized in the Service Context: {}",
                missing_required.into_iter().collect::<Vec<_>>().join(", ")
            ),
        ));
    }
    if included.is_empty()
        && !mount_unbound_optional_context
        && !has_events
        && !has_retained_volume
        && !provides_workload_api
    {
        return Ok(None);
    }
    let generations = included
        .iter()
        .map(|binding| binding.context_generation)
        .collect::<BTreeSet<_>>();
    if !included.is_empty() && (generations.len() != 1 || generations.contains(&0)) {
        return Err(StoreApiError::new(
            409,
            "STORE_BINDING_GENERATION_SPLIT",
            "all active bindings for one Deployment must share one positive context generation",
        ));
    }
    let generation = generations.first().copied().unwrap_or(1);
    let bindings = included
        .into_iter()
        .map(|binding| {
            (
                binding.requirement_name.clone(),
                ManagedApiBinding {
                    binding_id: binding.binding_id.clone(),
                    api_id: binding.api_id.clone(),
                    timeout_ms: binding.timeout_ms.unwrap_or(30_000),
                    context_generation: binding.context_generation,
                },
            )
        })
        .collect::<BTreeMap<_, _>>();
    let events = managed_event_binding(storage, contract, node_id, generation)?;
    let allow_development_admin_fallback = matches!(storage, DurableStore::Sqlite(_))
        && std::env::var("ORCHESTRATOR_ALLOW_ADMIN_ORIGIN_FOR_WORKLOAD")
            .ok()
            .is_some_and(|value| value == "1" || value.eq_ignore_ascii_case("true"));
    let gateway_origin = std::env::var("ORCHESTRATOR_GATEWAY_WORKLOAD_ORIGIN")
        .ok()
        .or_else(|| {
            allow_development_admin_fallback
                .then(|| std::env::var("ORCHESTRATOR_GATEWAY_ADMIN_ORIGIN").ok())
                .flatten()
        })
        .map(|value| value.trim().trim_end_matches('/').to_string())
        .filter(|value| !value.is_empty())
        .or_else(|| contract.requirements().is_empty().then(|| "http://127.0.0.1".to_string()))
        .ok_or_else(|| {
            StoreApiError::new(
                503,
                "STORE_GATEWAY_WORKLOAD_ORIGIN_REQUIRED",
                "managed API bindings require ORCHESTRATOR_GATEWAY_WORKLOAD_ORIGIN; production never falls back to the Gateway admin origin",
            )
        })?;
    let gateway_ca_pem = std::env::var("ORCHESTRATOR_GATEWAY_WORKLOAD_CA_FILE")
        .ok()
        .map(|path| {
            let path = path.trim();
            if path.is_empty() {
                return Err(StoreApiError::new(
                    503,
                    "STORE_GATEWAY_WORKLOAD_CA_INVALID",
                    "ORCHESTRATOR_GATEWAY_WORKLOAD_CA_FILE must not be empty",
                ));
            }
            fs::read_to_string(path).map_err(|error| {
                StoreApiError::new(
                    503,
                    "STORE_GATEWAY_WORKLOAD_CA_UNREADABLE",
                    format!("read Gateway workload CA file {path}: {error}"),
                )
            })
        })
        .transpose()?;
    let workload_verifier = provides_workload_api
        .then(load_managed_workload_verifier)
        .transpose()?;
    let context = ManagedServiceContextSpec {
        generation,
        node_id: node_id.to_string(),
        gateway_origin,
        gateway_ca_pem,
        bindings,
        events,
        workload_verifier,
    };
    context.validate().map_err(|error| {
        StoreApiError::new(
            503,
            "STORE_GATEWAY_WORKLOAD_CONTEXT_INVALID",
            error.to_string(),
        )
    })?;
    Ok(Some(context))
}

const MAX_WORKLOAD_PUBLIC_KEY_FILE_BYTES: u64 = 16 * 1024;

fn load_managed_workload_verifier() -> Result<ManagedWorkloadVerifierSpec, StoreApiError> {
    let path = required_workload_verifier_env("ORCHESTRATOR_WORKLOAD_PUBLIC_KEY_FILE")?;
    let metadata = fs::metadata(&path).map_err(|error| {
        StoreApiError::new(
            503,
            "STORE_WORKLOAD_VERIFIER_UNREADABLE",
            format!("read workload verifier public key metadata: {error}"),
        )
    })?;
    if !metadata.is_file()
        || metadata.len() == 0
        || metadata.len() > MAX_WORKLOAD_PUBLIC_KEY_FILE_BYTES
    {
        return Err(StoreApiError::new(
            503,
            "STORE_WORKLOAD_VERIFIER_INVALID",
            "ORCHESTRATOR_WORKLOAD_PUBLIC_KEY_FILE must name one non-empty regular file no larger than 16 KiB",
        ));
    }
    let public_key_pem = fs::read_to_string(&path).map_err(|error| {
        StoreApiError::new(
            503,
            "STORE_WORKLOAD_VERIFIER_UNREADABLE",
            format!("read workload verifier public key: {error}"),
        )
    })?;
    let verifier = ManagedWorkloadVerifierSpec {
        public_key_pem,
        key_id: required_workload_verifier_env("ORCHESTRATOR_WORKLOAD_KEY_ID")?,
        issuer: required_workload_verifier_env("ORCHESTRATOR_WORKLOAD_ISSUER")?,
        audience: required_workload_verifier_env("ORCHESTRATOR_WORKLOAD_AUDIENCE")?,
    };
    verifier.validate().map_err(|error| {
        StoreApiError::new(503, "STORE_WORKLOAD_VERIFIER_INVALID", error.to_string())
    })?;
    Ok(verifier)
}

fn required_workload_verifier_env(name: &str) -> Result<String, StoreApiError> {
    std::env::var(name)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            StoreApiError::new(
                503,
                "STORE_WORKLOAD_VERIFIER_REQUIRED",
                format!("signed v3 workload API providers require {name}"),
            )
        })
}

fn contract_has_retained_runtime_volume(contract: &ServiceReleaseContract) -> bool {
    contract
        .platform
        .as_ref()
        .is_some_and(|platform| !platform.runtime_volumes.is_empty())
}

fn attach_release_runtime_volume(
    spec: &mut ContainerSpec,
    contract: &ServiceReleaseContract,
) -> Result<(), StoreApiError> {
    if contract.platform.is_some() {
        spec.labels.insert(
            SERVICE_CONTRACT_GENERATION_LABEL.to_string(),
            "3".to_string(),
        );
    }
    let volumes = contract
        .platform
        .as_ref()
        .map(|platform| platform.runtime_volumes.as_slice())
        .unwrap_or_default();
    let Some(volume) = volumes.first() else {
        spec.retained_volume = None;
        return Ok(());
    };
    if volumes.len() != 1 {
        return Err(StoreApiError::new(
            422,
            "STORE_RUNTIME_VOLUME_INVALID",
            "signed runtime volume contract must contain exactly one v1 RETAIN attachment",
        ));
    }
    let attachment = RetainedVolumeAttachmentV1 {
        owner_instance_id: stable_service_instance_id(&spec.service_id),
        logical_name: volume.name.clone(),
        target: volume.target.clone(),
        access: volume.access.clone(),
        lifecycle: volume.lifecycle.clone(),
    };
    attachment
        .validate_for_service(&spec.service_id)
        .map_err(|error| {
            StoreApiError::new(
                422,
                "STORE_RUNTIME_VOLUME_INVALID",
                format!("signed runtime volume contract is invalid: {error}"),
            )
        })?;
    spec.retained_volume = Some(attachment);
    Ok(())
}

fn managed_event_binding(
    storage: &DurableStore,
    contract: &ServiceReleaseContract,
    node_id: &str,
    generation: u64,
) -> Result<Option<ManagedEventBinding>, StoreApiError> {
    if contract.events.publishes.is_empty() && contract.events.subscribes.is_empty() {
        return Ok(None);
    }
    let node = storage
        .list_nodes()
        .map_err(storage_error)?
        .into_iter()
        .find(|node| node.node_id == node_id)
        .ok_or_else(|| {
            StoreApiError::new(
                404,
                "STORE_NODE_NOT_FOUND",
                format!("target Node {node_id} does not exist"),
            )
        })?;
    let connection_id = provider_identifier(&node, "redis", "connection_id")?;
    let facts = node_runtime_facts(storage, node_id)?;
    if !facts
        .redis_connection_ids
        .iter()
        .any(|configured| configured == &connection_id)
    {
        return Err(StoreApiError::new(
            422,
            "STORE_EVENT_PROVIDER_NOT_ATTESTED",
            format!(
                "target Node {node_id} has not attested Agent-local Redis connection {connection_id}"
            ),
        ));
    }
    let publish_types = contract
        .events
        .publishes
        .iter()
        .map(|event| event.event_id().to_string())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let subscriptions = contract
        .events
        .subscribes
        .iter()
        .map(|event| ManagedEventSubscription {
            event_type: event.event_id().to_string(),
            consumer_group: event.consumer_group().to_string(),
        })
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    Ok(Some(ManagedEventBinding {
        connection_id,
        stream: MANAGED_EVENT_STREAM_V1.to_string(),
        publish_types,
        subscriptions,
        generation,
    }))
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
    runtime_contract: RuntimeContract,
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
        (
            "ojos.catalog_signature_verified".to_string(),
            "true".to_string(),
        ),
        ("ojos.target_node_id".to_string(), node.node_id.clone()),
    ]);
    ContainerSpec {
        deployment_id: deployment_id.to_string(),
        service_id: service_id.to_string(),
        generation: 1,
        image,
        runtime_contract,
        runtime_context: None,
        managed_service_context: None,
        resource_secret_file_mounts: Vec::new(),
        retained_volume: None,
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
    // A backend worker still needs a stable Topology endpoint identity so its
    // outbound ApiBindings can be versioned and audited.  It is not an
    // inbound service, however, and publishing its health port would violate
    // the fixed judge-sandbox-v1 runtime contract.  Keep the endpoint in the
    // immutable Topology while deliberately omitting Docker port bindings.
    if release.service_type.eq_ignore_ascii_case("backend-worker") {
        return Ok(None);
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

fn effective_managed_endpoint(
    requested: &str,
    node: &NodeRecord,
    release: &ServiceReleaseManifest,
) -> Result<String, StoreApiError> {
    if !requested.trim().is_empty() {
        return Ok(requested.trim().to_string());
    }
    if !release.service_type.eq_ignore_ascii_case("backend-worker") {
        return Ok(String::new());
    }
    node.host_ip.parse::<IpAddr>().map_err(|_| {
        StoreApiError::new(
            422,
            "STORE_TARGET_NODE_ENDPOINT_UNAVAILABLE",
            format!(
                "target Node {} does not advertise a valid host_ip for its logical worker endpoint",
                node.node_id
            ),
        )
    })?;
    if release.backend.port == 0 {
        return Err(StoreApiError::new(
            422,
            "STORE_WORKER_ENDPOINT_PORT_INVALID",
            "backend-worker release must declare a non-zero backend port for its logical Topology identity",
        ));
    }
    Ok(format!(
        "{}:{}:{}",
        node.host_ip, release.backend.port, release.service_name
    ))
}

fn endpoint_socket(endpoint: &str) -> Option<(IpAddr, u16)> {
    let identity = parse_endpoint_id(endpoint).ok()?;
    Some((identity.host.parse().ok()?, identity.port.parse().ok()?))
}

fn allocate_replacement_endpoint(
    storage: &DurableStore,
    current_endpoint: &str,
    service_id: &str,
    deployment_id: &str,
) -> Result<String, StoreApiError> {
    let identity = parse_endpoint_id(current_endpoint).map_err(|error| {
        StoreApiError::new(422, "STORE_REPLACEMENT_ENDPOINT_INVALID", error.to_string())
    })?;
    let used = storage
        .runtime_instances(None)
        .map_err(storage_error)?
        .into_iter()
        .filter_map(|runtime| endpoint_socket(&runtime.endpoint))
        .collect::<BTreeSet<_>>();
    let seed = Sha256::digest(deployment_id.as_bytes());
    let first = 20_000_u16 + u16::from_be_bytes([seed[0], seed[1]]) % 40_000;
    for offset in 0..40_000_u32 {
        let port = 20_000_u16 + ((u32::from(first - 20_000) + offset) % 40_000) as u16;
        let socket = identity
            .host
            .parse::<IpAddr>()
            .ok()
            .map(|host| (host, port));
        if socket.is_some_and(|socket| !used.contains(&socket)) {
            return Ok(format!("{}:{port}:{service_id}", identity.host));
        }
    }
    Err(StoreApiError::new(
        503,
        "STORE_REPLACEMENT_ENDPOINT_EXHAUSTED",
        format!(
            "no temporary replacement endpoint is available on {}",
            identity.host
        ),
    ))
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
pub(crate) struct StoreApiError {
    pub(crate) status: u16,
    pub(crate) code: &'static str,
    pub(crate) detail: String,
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

fn contribution_storage_error(
    error: orchestrator_storage::ContributionRepositoryError,
) -> StoreApiError {
    let status = match &error {
        orchestrator_storage::ContributionRepositoryError::Conflict(_) => 409,
        orchestrator_storage::ContributionRepositoryError::Invalid(_) => 422,
        orchestrator_storage::ContributionRepositoryError::NotFound(_) => 404,
        orchestrator_storage::ContributionRepositoryError::Persistence(_) => 500,
    };
    StoreApiError::new(
        status,
        "STORE_CONTRIBUTION_STORAGE_ERROR",
        error.to_string(),
    )
}

fn contribution_controller_error(
    error: crate::contribution_controller::ContributionControllerError,
) -> StoreApiError {
    let status = match &error {
        crate::contribution_controller::ContributionControllerError::Conflict(_) => 409,
        crate::contribution_controller::ContributionControllerError::NotFound(_) => 404,
        crate::contribution_controller::ContributionControllerError::NeedsAttention(_) => 409,
        crate::contribution_controller::ContributionControllerError::Retryable(_)
        | crate::contribution_controller::ContributionControllerError::RetryableCompensation(_) => {
            409
        }
        crate::contribution_controller::ContributionControllerError::Invalid(_) => 422,
        crate::contribution_controller::ContributionControllerError::Persistence(_) => 500,
    };
    StoreApiError::new(status, error.code(), error.to_string())
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
    use crate::test_env::TestEnv;
    use base64::Engine as _;
    use base64::engine::general_purpose::STANDARD;
    use ed25519_dalek::{Signer, SigningKey};
    use orchestrator_control_plane::{
        ClaimRequest, CompleteRequest, CompletionStatus, DurableOperationStatus, JobStatus,
        JobStore, MemoryJobStore, MemoryOperationStore, OperationRepository,
    };
    use orchestrator_legacy::{ContributionRevisionV1, OrchestratorStore, ServiceRelease};
    use orchestrator_manager::catalog_v2::{
        CatalogModuleV2, CatalogReleaseV2, CatalogTrustStore, CatalogV2, Ed25519Signature,
        MetadataPackageV2, ReleaseDependencyV2, RuntimeCapabilityV2,
    };
    use orchestrator_runtime::{DockerRuntimeFacts, RuntimeDesiredState, RuntimeInstance};
    use orchestrator_storage::{
        ContributionRepository, SqliteOrchestratorStore, StoredNodeRuntimeFacts,
        StoredRuntimeInstance, TopologyApplyOutcome,
    };
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

    #[test]
    fn json_schema_conditionals_require_only_the_active_secret_branch() {
        let schema = conditional_config_schema_fixture();
        let open = json!({"registration": {"mode": "open"}});
        let invite = json!({"registration": {"mode": "invite-only"}});
        let empty = BTreeMap::new();

        let (config, declared, controlled) =
            validate_release_config(&schema, &open, &empty).expect("open branch");
        assert!(controlled);
        assert_eq!(config["registration.mode"], "open");
        assert!(declared.contains("registration.inviteSigningKey"));

        let missing = validate_release_config(&schema, &invite, &empty).unwrap_err();
        assert_eq!(missing.code, "STORE_CONFIG_INVALID");

        let supplied = BTreeMap::from([(
            "registration.inviteSigningKey".to_string(),
            "file://opaque-ref".to_string(),
        )]);
        validate_release_config(&schema, &invite, &supplied)
            .expect("invite branch with opaque secret reference");
        let inactive = validate_release_config(&schema, &open, &supplied).unwrap_err();
        assert_eq!(inactive.code, "STORE_CONFIG_INVALID");
        assert!(!inactive.detail.contains("file://opaque-ref"));
    }
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

    fn legacy_consumer_manifest(service_id: &str, api_id: &str) -> ServiceReleaseManifest {
        let mut release = release_manifest(
            service_id,
            &format!("registry.example/ojos/{service_id}@{DIGEST}"),
        );
        release.required_apis = vec![api_id.to_string()];
        release
    }

    fn workload_provider_manifest(service_id: &str, api_id: &str) -> ServiceReleaseManifest {
        let mut release = release_manifest(
            service_id,
            &format!("registry.example/ojos/{service_id}@{DIGEST}"),
        );
        let permission = format!("{service_id}.read");
        release.permissions = vec![permission.clone()];
        release.apis = serde_json::from_value(json!([{
            "api_id": api_id,
            "protocol": "http",
            "port_name": "http",
            "path_prefix": "/objects",
            "methods": ["GET"],
            "visibility": "explicit",
            "auth_mode": "workload",
            "permission": permission,
            "stability": "stable",
            "version": "1.0.0"
        }]))
        .unwrap();
        release
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
        let facts_now = now_ms();
        let facts = NodeRuntimeFactsV1 {
            schema_version: 1,
            report_id: "fixture-report-1".to_string(),
            observed_at_ms: facts_now,
            agent_version: "1.0.0-test".to_string(),
            runtime_policy_sha256: format!("sha256:{}", "9".repeat(64)),
            allowed_contracts: vec![RuntimeContract::standard_v1()],
            judge_sandbox_allowed_images: Vec::new(),
            redis_connection_ids: Vec::new(),
            docker: DockerRuntimeFacts {
                engine: "docker".to_string(),
                server_version: "27.0.0".to_string(),
                operating_system: "Linux".to_string(),
                os_type: "linux".to_string(),
                architecture: "x86_64".to_string(),
                cgroup_version: "2".to_string(),
                memory_limit: true,
                pids_limit: true,
                rootless: false,
                apparmor: true,
                seccomp: true,
                security_options: vec!["name=seccomp,profile=default".to_string()],
            },
            inventory_complete: true,
            inventory_error: String::new(),
            deployment_observations: Vec::new(),
            credential_statuses: Vec::new(),
        };
        sqlite
            .put_node_runtime_facts(&StoredNodeRuntimeFacts {
                node_id: "node-1".to_string(),
                observed_at_ms: facts_now,
                received_at_ms: facts_now,
                facts: serde_json::to_value(facts).unwrap(),
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

    fn event_contract(
        service_id: &str,
        publishes: Value,
        subscribes: Value,
    ) -> ServiceReleaseContract {
        let mut document = serde_json::to_value(release_manifest(
            service_id,
            &format!("registry.example/ojos/{service_id}@{DIGEST}"),
        ))
        .unwrap();
        make_v2_release_document(&mut document);
        document["events"] = json!({
            "publishes": publishes,
            "subscribes": subscribes,
        });
        ServiceReleaseContract::from_yaml_str(&serde_yaml::to_string(&document).unwrap()).unwrap()
    }

    fn make_v2_release_document(document: &mut Value) {
        document["schema_version"] = json!(2);
        document["provides"] = json!({"apis": []});
        document["requires"] = json!({"apis": []});
        document["events"] = json!({"publishes": [], "subscribes": []});
        document["runtime_contract"] = json!({
            "id": "standard-container-v1",
            "sha256": orchestrator_runtime::STANDARD_RUNTIME_PROFILE_SHA256,
            "binding_directory": "/run/ojos/service",
            "identity_mode": "workload",
            "credential_delivery": "file",
            "restart_on_change": false,
        });
    }

    fn add_empty_signed_platform(document: &mut Value, marker: char) {
        let digest = |offset: u8| {
            let nibble = char::from_digit(((marker as u8 + offset) % 16) as u32, 16)
                .expect("hex fixture marker");
            format!("sha256:{}", nibble.to_string().repeat(64))
        };
        document["platform"] = json!({
            "schemaVersion": orchestrator_legacy::RELEASE_PLATFORM_SCHEMA_VERSION,
            "contractDigest": digest(0),
            "sourceDigest": digest(1),
            "releaseLockDigest": digest(2),
            "artifactSubjects": [
                {
                    "slot": "contract",
                    "roles": ["contract"],
                    "mediaType": "application/vnd.ojos.service-contract.v3+json",
                    "digest": digest(3),
                    "size": 1
                },
                {
                    "slot": "provenance",
                    "roles": ["provenance"],
                    "mediaType": "application/vnd.in-toto+json",
                    "digest": digest(4),
                    "size": 1
                },
                {
                    "slot": "sbom",
                    "roles": ["sbom"],
                    "mediaType": "application/spdx+json",
                    "digest": digest(5),
                    "size": 1
                }
            ],
            "packageRequirements": [],
            "resourceClaims": [],
            "contribution": {}
        });
    }

    fn provider_only_v3_contract() -> ServiceReleaseContract {
        let mut document = serde_json::to_value(release_manifest(
            "fixture-provider",
            &format!("registry.example/ojos/fixture-provider@{DIGEST}"),
        ))
        .unwrap();
        make_v2_release_document(&mut document);
        document["permissions"] = json!(["fixture.read"]);
        document["provides"] = json!({"apis": [{
            "id": "fixture.provider.get",
            "version": "1.0.0",
            "protocol": "http",
            "port_name": "http",
            "path": "/",
            "methods": ["GET"],
            "visibility": "explicit",
            "auth": "workload",
            "permission": "fixture.read",
            "stability": "stable"
        }]});
        add_empty_signed_platform(&mut document, 'a');
        ServiceReleaseContract::from_json_value(document).unwrap()
    }

    fn fixture_ed25519_public_key_pem() -> String {
        "-----BEGIN PUBLIC KEY-----\nMCowBQYDK2VwAyEAERERERERERERERERERERERERERERERERERERERERERE=\n-----END PUBLIC KEY-----\n".to_string()
    }

    fn configure_workload_verifier(environment: &mut TestEnv, path: &Path) {
        environment.set(
            "ORCHESTRATOR_WORKLOAD_PUBLIC_KEY_FILE",
            path.to_str().unwrap(),
        );
        environment.set("ORCHESTRATOR_WORKLOAD_KEY_ID", "workload-1");
        environment.set("ORCHESTRATOR_WORKLOAD_ISSUER", "ojos-auth/workload");
        environment.set("ORCHESTRATOR_WORKLOAD_AUDIENCE", "ojos-gateway");
    }

    #[test]
    fn provider_only_v3_receives_platform_workload_verifier_and_fails_closed() {
        let contract = provider_only_v3_contract();
        let fixture = fixture(
            release_manifest(
                "fixture-root",
                &format!("registry.example/ojos/api@{DIGEST}"),
            ),
            "READY",
        );
        let mut environment = TestEnv::lock();
        for name in [
            "ORCHESTRATOR_WORKLOAD_PUBLIC_KEY_FILE",
            "ORCHESTRATOR_WORKLOAD_KEY_ID",
            "ORCHESTRATOR_WORKLOAD_ISSUER",
            "ORCHESTRATOR_WORKLOAD_AUDIENCE",
        ] {
            environment.remove(name);
        }
        let missing =
            managed_service_context_spec(&fixture.durable, &contract, "node-1", &[], false)
                .unwrap_err();
        assert_eq!(missing.code, "STORE_WORKLOAD_VERIFIER_REQUIRED");

        let key_path = fixture._directory.path().join("workload-public-key.pem");
        fs::write(&key_path, fixture_ed25519_public_key_pem()).unwrap();
        configure_workload_verifier(&mut environment, &key_path);
        let context =
            managed_service_context_spec(&fixture.durable, &contract, "node-1", &[], false)
                .unwrap()
                .expect("provider-only v3 receives a managed context");
        assert!(context.bindings.is_empty());
        assert_eq!(context.workload_verifier.unwrap().key_id, "workload-1");
    }

    #[test]
    fn store_rejects_multiple_non_ed25519_and_oversized_workload_keys() {
        let contract = provider_only_v3_contract();
        let fixture = fixture(
            release_manifest(
                "fixture-root",
                &format!("registry.example/ojos/api@{DIGEST}"),
            ),
            "READY",
        );
        let valid = fixture_ed25519_public_key_pem();
        for (name, bytes) in [
            ("multiple", format!("{valid}{valid}").into_bytes()),
            (
                "rsa",
                b"-----BEGIN PUBLIC KEY-----\nMAwwDQYJKoZIhvcNAQEBBQADCwAwCAIBAwIDAQAB\n-----END PUBLIC KEY-----\n".to_vec(),
            ),
            (
                "oversized",
                vec![b'A'; MAX_WORKLOAD_PUBLIC_KEY_FILE_BYTES as usize + 1],
            ),
        ] {
            let mut environment = TestEnv::lock();
            let path = fixture._directory.path().join(format!("{name}.pem"));
            fs::write(&path, bytes).unwrap();
            configure_workload_verifier(&mut environment, &path);
            let error = managed_service_context_spec(
                &fixture.durable,
                &contract,
                "node-1",
                &[],
                false,
            )
            .unwrap_err();
            assert_eq!(error.code, "STORE_WORKLOAD_VERIFIER_INVALID");
        }
    }

    fn replace_fixture_release_metadata(
        fixture: &Fixture,
        service_id: &str,
        version: &str,
        document: &Value,
    ) {
        let catalog_path = fixture._directory.path().join("catalog.json");
        let mut catalog: CatalogV2 =
            serde_json::from_slice(&fs::read(&catalog_path).unwrap()).unwrap();
        let release = catalog
            .modules
            .iter_mut()
            .find(|module| module.id == service_id)
            .and_then(|module| {
                module
                    .releases
                    .iter_mut()
                    .find(|release| release.version == Version::parse(version).unwrap())
            })
            .expect("fixture Catalog release");
        let metadata = serde_yaml::to_string(document).unwrap();
        fs::write(
            fixture._directory.path().join(&release.metadata.url),
            metadata.as_bytes(),
        )
        .unwrap();
        release.metadata.sha256 = format!("sha256:{:x}", Sha256::digest(metadata.as_bytes()))
            .parse()
            .unwrap();
        catalog.signatures.clear();
        let signing_key = SigningKey::from_bytes(&[7_u8; 32]);
        let signature = signing_key.sign(&catalog.signing_payload_jcs().unwrap());
        catalog.signatures.push(Ed25519Signature {
            key_id: "fixture-key".to_string(),
            algorithm: "Ed25519".to_string(),
            signature: STANDARD.encode(signature.to_bytes()),
        });
        fs::write(&catalog_path, serde_json::to_vec_pretty(&catalog).unwrap()).unwrap();
        fixture.registry.bootstrap(&fixture.durable).unwrap();
    }

    fn configure_event_provider(fixture: &Fixture, attest_connection: bool) -> NodeRecord {
        let mut node = fixture
            .durable
            .list_nodes()
            .unwrap()
            .into_iter()
            .find(|node| node.node_id == "node-1")
            .unwrap();
        node.labels = json!({
            "runtime": "docker",
            "os": "linux",
            "arch": "amd64",
            "providers": {
                "redis": {
                    "enabled": true,
                    "connection_id": "shared-events"
                }
            }
        });
        fixture.durable.upsert_node(node.clone()).unwrap();

        let stored = fixture
            .durable
            .node_runtime_facts("node-1")
            .unwrap()
            .unwrap();
        let mut facts: NodeRuntimeFactsV1 = serde_json::from_value(stored.facts).unwrap();
        facts.redis_connection_ids = if attest_connection {
            vec!["shared-events".to_string()]
        } else {
            vec![]
        };
        let observed_at_ms = now_ms();
        facts.observed_at_ms = observed_at_ms;
        fixture
            .durable
            .put_node_runtime_facts(&StoredNodeRuntimeFacts {
                node_id: "node-1".to_string(),
                observed_at_ms,
                received_at_ms: observed_at_ms,
                facts: serde_json::to_value(facts).unwrap(),
            })
            .unwrap();
        node
    }

    #[test]
    fn event_contract_plan_rejects_missing_or_unattested_local_provider() {
        let contract = event_contract(
            "fixture-producer",
            json!([{
                "id": "io.example.fixture.v1",
                "version": "1.0.0",
                "schema_ref": "schemas/io.example.fixture.v1.json"
            }]),
            json!([]),
        );
        let fixture = fixture(
            release_manifest(
                "fixture-root",
                &format!("registry.example/ojos/api@{DIGEST}"),
            ),
            "READY",
        );

        let missing =
            managed_service_context_spec(&fixture.durable, &contract, "node-1", &[], false)
                .unwrap_err();
        assert_eq!(missing.code, "STORE_PROVIDER_REQUIRED");

        configure_event_provider(&fixture, false);
        let unattested =
            managed_service_context_spec(&fixture.durable, &contract, "node-1", &[], false)
                .unwrap_err();
        assert_eq!(unattested.code, "STORE_EVENT_PROVIDER_NOT_ATTESTED");
    }

    #[test]
    fn event_contract_derives_one_shared_stream_and_exact_consumer_group() {
        let producer = event_contract(
            "fixture-producer",
            json!([{
                "id": "io.example.fixture.v1",
                "version": "1.0.0",
                "schema_ref": "schemas/io.example.fixture.v1.json"
            }]),
            json!([]),
        );
        let consumer = event_contract(
            "fixture-consumer",
            json!([]),
            json!([{
                "type": "io.example.fixture.v1",
                "version": "1.0.0",
                "consumer_group": "fixture-consumer"
            }]),
        );
        let fixture = fixture(
            release_manifest(
                "fixture-root",
                &format!("registry.example/ojos/api@{DIGEST}"),
            ),
            "READY",
        );
        let node = configure_event_provider(&fixture, true);

        let producer_context =
            managed_service_context_spec(&fixture.durable, &producer, "node-1", &[], false)
                .unwrap()
                .expect("event-only publisher receives a managed context");
        let consumer_context =
            managed_service_context_spec(&fixture.durable, &consumer, "node-1", &[], false)
                .unwrap()
                .expect("event-only consumer receives a managed context");
        let producer_events = producer_context.events.as_ref().unwrap();
        let consumer_events = consumer_context.events.as_ref().unwrap();
        assert_eq!(producer_events.stream, MANAGED_EVENT_STREAM_V1);
        assert_eq!(consumer_events.stream, MANAGED_EVENT_STREAM_V1);
        assert_eq!(producer_events.connection_id, "shared-events");
        assert_eq!(consumer_events.connection_id, "shared-events");
        assert_eq!(
            consumer_events.subscriptions,
            vec![ManagedEventSubscription {
                event_type: "io.example.fixture.v1".to_string(),
                consumer_group: "fixture-consumer".to_string(),
            }]
        );
        assert!(
            !serde_json::to_string(&consumer_context)
                .unwrap()
                .contains("redis://")
        );

        let install = RuntimeInstallPayload {
            spec: ContainerSpec {
                deployment_id: "deployment-fixture-consumer".to_string(),
                service_id: "fixture-consumer".to_string(),
                generation: 1,
                image: OciImageReference::parse(&format!(
                    "registry.example/ojos/fixture-consumer@{DIGEST}"
                ))
                .unwrap(),
                runtime_contract: RuntimeContract::standard_v1(),
                runtime_context: None,
                managed_service_context: Some(consumer_context),
                resource_secret_file_mounts: Vec::new(),
                retained_volume: None,
                command: vec![],
                environment: vec![],
                labels: HashMap::new(),
                published_endpoint: None,
            },
            start: true,
            health_gate: HealthGatePolicy::default(),
            offline_oci_artifact: None,
        };
        let pipeline = release_pipeline_payload(
            &consumer.release,
            &consumer,
            &install,
            &[],
            &node,
            "operation-event-contract",
            "SKIP",
            "",
            &json!({}),
            &BTreeMap::new(),
        )
        .unwrap()
        .expect("event subscription requires a typed Redis provisioner");
        let resources = pipeline
            .provisioners
            .iter()
            .find_map(|step| match step {
                TypedProvisionerStep::Redis { resources, .. } => Some(resources),
                _ => None,
            })
            .unwrap();
        assert_eq!(resources.len(), 1);
        assert_eq!(resources[0].kind, "consumer-group");
        assert_eq!(resources[0].connection_id, "shared-events");
        assert_eq!(resources[0].namespace, MANAGED_EVENT_STREAM_V1);
        assert_eq!(resources[0].consumer_group, "fixture-consumer");
    }

    #[test]
    fn legacy_api_surfaces_never_create_an_external_registry_step() {
        let mut release = workload_provider_manifest("legacy-provider", "legacy.read");
        release.storage = serde_json::from_value(json!([{
            "object_type": "document",
            "bucket": "legacy-provider",
            "path_prefix": "/legacy-provider/documents"
        }]))
        .unwrap();
        let contract =
            ServiceReleaseContract::from_json_value(serde_json::to_value(&release).unwrap())
                .unwrap();
        let node = NodeRecord {
            node_id: "node-legacy".to_string(),
            host_ip: "127.0.0.2".to_string(),
            parent_node_id: String::new(),
            role: "standalone".to_string(),
            labels: json!({
                "providers": {
                    "auth": true,
                    "storage": {
                        "enabled": true,
                        "backend": "node_directory",
                        "connection_id": "node-files"
                    }
                }
            }),
            status: "READY".to_string(),
            created_at: "t0".to_string(),
            updated_at: "t0".to_string(),
        };
        let install = RuntimeInstallPayload {
            spec: ContainerSpec {
                deployment_id: "deployment-legacy-provider".to_string(),
                service_id: "legacy-provider".to_string(),
                generation: 1,
                image: OciImageReference::parse(&format!(
                    "registry.example/ojos/legacy-provider@{DIGEST}"
                ))
                .unwrap(),
                runtime_contract: RuntimeContract::standard_v1(),
                runtime_context: None,
                managed_service_context: None,
                resource_secret_file_mounts: Vec::new(),
                retained_volume: None,
                command: vec![],
                environment: vec![],
                labels: HashMap::new(),
                published_endpoint: None,
            },
            start: true,
            health_gate: HealthGatePolicy::default(),
            offline_oci_artifact: None,
        };
        let pipeline = release_pipeline_payload(
            &release,
            &contract,
            &install,
            &[],
            &node,
            "operation-legacy-provider",
            "SKIP",
            "",
            &json!({}),
            &BTreeMap::new(),
        )
        .unwrap()
        .expect("storage declaration creates a typed internal pipeline");
        assert!(
            pipeline
                .provisioners
                .iter()
                .any(|step| matches!(step, TypedProvisionerStep::Storage { .. }))
        );
        assert!(
            !pipeline
                .provisioners
                .iter()
                .any(|step| matches!(step, TypedProvisionerStep::ApiRegistry { .. }))
        );
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
        let observed_at_ms = fixture
            .durable
            .node_runtime_facts("node-1")
            .unwrap()
            .expect("fixture runtime facts")
            .observed_at_ms;
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
                    runtime_contract: RuntimeContract::standard_v1(),
                    runtime_policy_sha256: String::new(),
                    effective_runtime_sha256: String::new(),
                    runtime_attested: true,
                    desired_state: RuntimeDesiredState::Running,
                    observed_state: RuntimeObservedState::Running,
                    health: "HEALTHY".to_string(),
                },
                management_mode: orchestrator_storage::RuntimeManagementMode::Managed,
                endpoint: String::new(),
                external_probe_protocol: String::new(),
                external_probe_health_path: String::new(),
                last_observed_at_ms: observed_at_ms,
                drift_reason: String::new(),
                credential_expires_at_ms: 0,
                credential_last_success_at_ms: 0,
                credential_last_error: String::new(),
                updated_at: "t0".to_string(),
            })
            .unwrap();
    }

    fn register_running_provider(
        fixture: &Fixture,
        service_id: &str,
        deployment_id: &str,
        endpoint: &str,
        api_id: &str,
    ) {
        let release = workload_provider_manifest(service_id, api_id);
        let mut sqlite = fixture.sqlite.clone();
        sqlite
            .upsert_service_release(ServiceRelease {
                service_name: service_id.to_string(),
                version: release.version.clone(),
                release_url: release.source.url.clone(),
                manifest: serde_json::to_value(&release).unwrap(),
                checksum: CHECKSUM.to_string(),
                created_at: "t0".to_string(),
            })
            .unwrap();
        put_running_instance(
            fixture,
            service_id,
            deployment_id,
            &format!("container-{deployment_id}"),
            &release.runtime.image,
        );
        let mut runtime = fixture
            .durable
            .runtime_instance(deployment_id)
            .unwrap()
            .unwrap();
        runtime.endpoint = endpoint.to_string();
        fixture.durable.put_runtime_instance(&runtime).unwrap();
    }

    fn apply_provider_topology(
        fixture: &Fixture,
        topology_id: &str,
        service_id: &str,
        deployment_id: &str,
        endpoint: &str,
    ) -> InstallTopologySelection {
        let spec = TopologySpec::new(
            topology_id,
            endpoint,
            "private",
            vec![TopologyEndpointSpec {
                endpoint: endpoint.to_string(),
                service_id: service_id.to_string(),
                protocol: "http".to_string(),
                health_path: "/health".to_string(),
                display_name: service_id.to_string(),
                note: "applied provider".to_string(),
                config: json!({"deployment_id": deployment_id, "node_id": "node-1"}),
            }],
            vec![],
        )
        .unwrap();
        let revision = fixture
            .durable
            .create_initial_topology_revision(
                spec,
                "t1".to_string(),
                "test".to_string(),
                "provider".to_string(),
            )
            .unwrap();
        fixture
            .durable
            .begin_topology_apply(topology_id, revision.revision_id(), "apply-provider", "t2")
            .unwrap();
        fixture
            .durable
            .finish_topology_apply(
                topology_id,
                revision.revision_id(),
                "apply-provider",
                TopologyApplyOutcome::Succeeded,
                "t3",
            )
            .unwrap();
        InstallTopologySelection {
            topology_id: topology_id.to_string(),
            revision_id: revision.revision_id().to_string(),
        }
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
    fn legacy_required_api_import_remains_metadata_only() {
        let service_id = "legacy-import-consumer";
        let manifest = legacy_consumer_manifest(service_id, "storage.object.get");
        let mut fixture = fixture(manifest, "READY");
        let response = route(
            &fixture.state,
            &mut fixture.console,
            Some(&fixture.durable),
            Some(&fixture.registry),
            Some(&fixture.artifact_store),
            &ApiRequest {
                method: "POST".to_string(),
                path: "/api/v1/store/releases:import".to_string(),
                headers: BTreeMap::from([(
                    "idempotency-key".to_string(),
                    "legacy-import-only".to_string(),
                )]),
                body: json!({
                    "service_id": service_id,
                    "version": "2.0.0",
                    "target_node_id": "node-1"
                })
                .to_string(),
            },
            "request-legacy-import",
        )
        .unwrap();
        assert_eq!(response.status, 201, "{}", response.body);
        let imported = fixture
            .console
            .service_releases()
            .unwrap()
            .into_iter()
            .find(|release| release.service_name == service_id && release.version == "2.0.0")
            .expect("legacy metadata was imported");
        let contract = ServiceReleaseContract::from_json_value(imported.manifest).unwrap();
        assert_eq!(contract.contract_version, 1);
        assert_eq!(contract.requirements().len(), 1);
        assert_eq!(
            contract.requirements()[0].binding_name(),
            "storage.object.get"
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
    }

    #[test]
    fn legacy_required_api_validate_and_install_fail_closed_without_provider_or_topology() {
        let service_id = "legacy-gated-consumer";
        let manifest = legacy_consumer_manifest(service_id, "storage.object.get");
        let mut fixture = fixture(manifest, "READY");
        let releases_before = fixture.console.service_releases().unwrap();
        let validate = route(
            &fixture.state,
            &mut fixture.console,
            Some(&fixture.durable),
            Some(&fixture.registry),
            Some(&fixture.artifact_store),
            &ApiRequest {
                method: "POST".to_string(),
                path: "/api/v1/store/releases:validate".to_string(),
                headers: BTreeMap::from([(
                    "idempotency-key".to_string(),
                    "legacy-validate-gate".to_string(),
                )]),
                body: json!({
                    "service_id": service_id,
                    "version": "2.0.0",
                    "target_node_id": "node-1"
                })
                .to_string(),
            },
            "request-legacy-validate-gate",
        )
        .unwrap();
        assert_eq!(validate.status, 200, "{}", validate.body);
        assert_eq!(validate.body["data"]["valid"], false);
        assert_eq!(
            validate.body["data"]["topology_confirmation_required"],
            true
        );
        assert_eq!(
            validate.body["data"]["requirements"][0]["requirement_name"],
            "storage.object.get"
        );
        assert_eq!(validate.body["data"]["requirements"][0]["missing"], true);
        assert_eq!(validate.body["data"]["bindings"], json!([]));
        assert_eq!(fixture.console.service_releases().unwrap(), releases_before);

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
                    "legacy-install-gate".to_string(),
                )]),
                body: json!({
                    "service_id": service_id,
                    "version": "2.0.0",
                    "target_node_id": "node-1"
                })
                .to_string(),
            },
            "request-legacy-install-gate",
        )
        .unwrap();
        assert_eq!(install.status, 422, "{}", install.body);
        assert_eq!(install.body["code"], "STORE_BINDING_UNRESOLVED");
        assert!(
            !fixture
                .console
                .service_releases()
                .unwrap()
                .iter()
                .any(|release| release.service_name == service_id && release.version == "2.0.0"),
            "binding failure must happen before release metadata publication"
        );
        assert!(fixture.durable.operation_store().list().unwrap().is_empty());
    }

    #[test]
    fn legacy_required_api_resolves_only_to_one_healthy_applied_provider() {
        let service_id = "legacy-resolved-consumer";
        let api_id = "storage.object.get";
        let manifest = legacy_consumer_manifest(service_id, api_id);
        let mut fixture = fixture(manifest.clone(), "READY");
        let contract =
            ServiceReleaseContract::from_json_value(serde_json::to_value(manifest).unwrap())
                .unwrap();
        let provider_endpoint = "127.0.0.3:8080:storage-provider";
        register_running_provider(
            &fixture,
            "storage-provider",
            "deployment-storage-a",
            provider_endpoint,
            api_id,
        );
        let consumer_endpoint = "127.0.0.2:8080:legacy-resolved-consumer";
        let validate = route(
            &fixture.state,
            &mut fixture.console,
            Some(&fixture.durable),
            Some(&fixture.registry),
            Some(&fixture.artifact_store),
            &ApiRequest {
                method: "POST".to_string(),
                path: "/api/v1/store/releases:validate".to_string(),
                headers: BTreeMap::from([(
                    "idempotency-key".to_string(),
                    "legacy-unique-validate".to_string(),
                )]),
                body: json!({
                    "service_id": service_id,
                    "version": "2.0.0",
                    "target_node_id": "node-1",
                    "endpoint": consumer_endpoint
                })
                .to_string(),
            },
            "request-legacy-unique-validate",
        )
        .unwrap();
        assert_eq!(validate.status, 200, "{}", validate.body);
        assert_eq!(validate.body["data"]["valid"], false);
        assert_eq!(
            validate.body["data"]["topology_confirmation_required"],
            true
        );
        assert_eq!(
            validate.body["data"]["requirements"][0]["recommended_provider_deployment_id"],
            "deployment-storage-a"
        );
        assert_eq!(validate.body["data"]["bindings"], json!([]));

        let install_without_topology = route(
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
                    "legacy-unique-install".to_string(),
                )]),
                body: json!({
                    "service_id": service_id,
                    "version": "2.0.0",
                    "target_node_id": "node-1",
                    "endpoint": consumer_endpoint
                })
                .to_string(),
            },
            "request-legacy-unique-install",
        )
        .unwrap();
        assert_eq!(install_without_topology.status, 422);
        assert_eq!(
            install_without_topology.body["code"],
            "STORE_BINDING_TOPOLOGY_REQUIRED"
        );
        assert!(
            !fixture
                .console
                .service_releases()
                .unwrap()
                .iter()
                .any(|release| release.service_name == service_id && release.version == "2.0.0")
        );
        assert!(fixture.durable.operation_store().list().unwrap().is_empty());

        let resolved = resolve_install_api_bindings(
            &fixture.console,
            &fixture.durable,
            &contract,
            "deployment-consumer",
            "node-1",
            consumer_endpoint,
            &[],
            None,
            false,
        )
        .unwrap();
        assert_eq!(resolved.len(), 1);
        assert_eq!(resolved[0].requirement_name, api_id);
        assert_eq!(resolved[0].state, ApiBindingState::Resolved);
        assert_eq!(
            ensure_managed_api_bindings_ready(&fixture.durable, &contract, &resolved, None)
                .unwrap_err()
                .code,
            "STORE_BINDING_TOPOLOGY_REQUIRED"
        );

        let topology = apply_provider_topology(
            &fixture,
            "legacy-binding-topology",
            "storage-provider",
            "deployment-storage-a",
            provider_endpoint,
        );
        let resolved = resolve_install_api_bindings(
            &fixture.console,
            &fixture.durable,
            &contract,
            "deployment-consumer",
            "node-1",
            consumer_endpoint,
            &[],
            Some(&topology),
            false,
        )
        .unwrap();
        ensure_managed_api_bindings_ready(&fixture.durable, &contract, &resolved, Some(&topology))
            .unwrap();

        let validate_applied = route(
            &fixture.state,
            &mut fixture.console,
            Some(&fixture.durable),
            Some(&fixture.registry),
            Some(&fixture.artifact_store),
            &ApiRequest {
                method: "POST".to_string(),
                path: "/api/v1/store/releases:validate".to_string(),
                headers: BTreeMap::from([(
                    "idempotency-key".to_string(),
                    "legacy-applied-validate".to_string(),
                )]),
                body: json!({
                    "service_id": service_id,
                    "version": "2.0.0",
                    "target_node_id": "node-1",
                    "endpoint": consumer_endpoint,
                    "topology_id": topology.topology_id,
                    "topology_etag": format!("\"{}\"", topology.revision_id)
                })
                .to_string(),
            },
            "request-legacy-applied-validate",
        )
        .unwrap();
        assert_eq!(validate_applied.status, 200, "{}", validate_applied.body);
        assert_eq!(validate_applied.body["data"]["valid"], true);
        assert_eq!(
            validate_applied.body["data"]["topology_confirmation_required"],
            false
        );
        assert_eq!(
            validate_applied.body["data"]["bindings"][0]["state"],
            "RESOLVED"
        );
        assert!(!validate_applied.body.to_string().contains("PENDING"));

        let mut staged = resolved[0].clone();
        staged.state = ApiBindingState::Pending;
        staged.observed_state = ApiBindingObservedState::Pending;
        staged.topology_id = topology.topology_id;
        staged.topology_revision_id = topology.revision_id;
        let public_plan = production_binding_plan([&staged]);
        assert_eq!(public_plan[0].state, ApiBindingState::Resolved);
        assert_eq!(public_plan[0].observed_state, "RESOLVED");
    }

    #[test]
    fn legacy_required_api_rejects_ambiguous_providers_until_explicitly_selected() {
        let service_id = "legacy-ambiguous-consumer";
        let api_id = "storage.object.get";
        let manifest = legacy_consumer_manifest(service_id, api_id);
        let fixture = fixture(manifest.clone(), "READY");
        let contract =
            ServiceReleaseContract::from_json_value(serde_json::to_value(manifest).unwrap())
                .unwrap();
        register_running_provider(
            &fixture,
            "storage-provider",
            "deployment-storage-a",
            "127.0.0.3:8080:storage-provider",
            api_id,
        );
        register_running_provider(
            &fixture,
            "storage-provider",
            "deployment-storage-b",
            "127.0.0.4:8080:storage-provider",
            api_id,
        );
        let error = resolve_install_api_bindings(
            &fixture.console,
            &fixture.durable,
            &contract,
            "deployment-consumer",
            "node-1",
            "127.0.0.2:8080:legacy-ambiguous-consumer",
            &[],
            None,
            false,
        )
        .unwrap_err();
        assert_eq!(error.code, "STORE_BINDING_UNRESOLVED");
        assert!(error.detail.contains("ambiguous"));

        let selected = resolve_install_api_bindings(
            &fixture.console,
            &fixture.durable,
            &contract,
            "deployment-consumer",
            "node-1",
            "127.0.0.2:8080:legacy-ambiguous-consumer",
            &[InstallBindingSelection {
                name: api_id.to_string(),
                provider_deployment_id: "deployment-storage-b".to_string(),
            }],
            None,
            false,
        )
        .unwrap();
        assert_eq!(selected[0].provider_deployment_id, "deployment-storage-b");
        assert_eq!(selected[0].state, ApiBindingState::Resolved);
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
    fn v2_catalog_install_does_not_require_a_node_local_auth_provider() {
        let service_id = "v2-control-plane-auth";
        let mut release =
            release_manifest(service_id, &format!("registry.example/ojos/api@{DIGEST}"));
        release.permissions = vec!["v2-control-plane-auth.read".to_string()];
        let mut fixture = fixture(release.clone(), "READY");
        let mut document = serde_json::to_value(release).unwrap();
        make_v2_release_document(&mut document);
        replace_fixture_release_metadata(&fixture, service_id, "1.2.3", &document);

        let node = fixture.durable.get_node("node-1").unwrap().unwrap();
        assert!(node_provider_label(&node, "auth").is_none());
        let response = route(
            &fixture.state,
            &mut fixture.console,
            Some(&fixture.durable),
            Some(&fixture.registry),
            Some(&fixture.artifact_store),
            &install_request(service_id, "v2-control-plane-auth-install"),
            "request-v2-control-plane-auth-install",
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
        assert_eq!(operation.planned_jobs.len(), 1);
        assert_eq!(operation.planned_jobs[0].kind, JobKind::Install);
        assert_eq!(
            operation.planned_jobs[0].payload["spec"]["labels"]["ojos.service_contract_version"],
            "2"
        );
        assert!(
            operation.planned_jobs[0].payload.get("auth").is_none(),
            "Service Contract v2 workload identity and grants belong to the control-plane ApiBinding projection"
        );
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
    fn upgrade_stages_signed_contribution_successor_around_runtime_cutover() {
        let service_id = "upgrade-contribution-api";
        let mut fixture = fixture(
            release_manifest(service_id, &format!("registry.example/ojos/api@{DIGEST}")),
            "READY",
        );
        let mut current_document = serde_json::to_value(release_manifest(
            service_id,
            &format!("registry.example/ojos/api@{DIGEST}"),
        ))
        .unwrap();
        make_v2_release_document(&mut current_document);
        add_empty_signed_platform(&mut current_document, '1');
        replace_fixture_release_metadata(&fixture, service_id, "1.2.3", &current_document);
        let mut upgrade_document = serde_json::to_value(release_manifest(
            service_id,
            &format!("registry.example/ojos/api@{UPGRADE_DIGEST}"),
        ))
        .unwrap();
        upgrade_document["version"] = json!("2.0.0");
        make_v2_release_document(&mut upgrade_document);
        add_empty_signed_platform(&mut upgrade_document, '2');
        replace_fixture_release_metadata(&fixture, service_id, "2.0.0", &upgrade_document);

        let current_image = format!("registry.example/ojos/api@{DIGEST}");
        put_running_instance(
            &fixture,
            service_id,
            "deployment-contribution-v1",
            "container-contribution-v1",
            &current_image,
        );
        record_release_history(
            &fixture,
            "op-history-contribution-v1",
            "deployment-contribution-v1",
            service_id,
            "1.2.3",
            &current_image,
            100,
        );
        let current_revision = ContributionRevisionV1::stage(
            "default",
            "deployment-contribution-v1",
            service_id,
            DIGEST,
            format!("sha256:{}", "1".repeat(64)),
            1,
            None,
            Vec::new(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
        )
        .unwrap();
        fixture
            .durable
            .insert_contribution_revision(&current_revision)
            .unwrap();
        let current_head = fixture
            .durable
            .compare_and_swap_contribution_head(None, &current_revision.activate().unwrap())
            .unwrap();

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
                    "upgrade-contribution-1".to_string(),
                )]),
                body: json!({
                    "deployment_id": "deployment-contribution-v1",
                    "version": "2.0.0"
                })
                .to_string(),
            },
            "request-upgrade-contribution",
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
        let runtime = operation
            .planned_jobs
            .iter()
            .find(|job| job.step_id == "runtime-upgrade")
            .unwrap();
        let prepare = operation
            .planned_jobs
            .iter()
            .find(|job| job.step_id.starts_with("contribution-prepare-"))
            .unwrap();
        let commit = operation
            .planned_jobs
            .iter()
            .find(|job| job.step_id.starts_with("contribution-commit-"))
            .unwrap();
        let abort = operation
            .planned_jobs
            .iter()
            .find(|job| job.step_id.starts_with("contribution-abort-"))
            .unwrap();
        let ack_gate = operation
            .planned_jobs
            .iter()
            .find(|job| job.step_id.starts_with("contribution-ack-gate-"))
            .unwrap();
        assert!(runtime.depends_on.contains(&prepare.step_id));
        assert!(commit.depends_on.contains(&runtime.step_id));
        assert!(commit.depends_on.contains(&prepare.step_id));
        assert!(ack_gate.depends_on.contains(&commit.step_id));
        assert!(abort.depends_on.contains(&ack_gate.step_id));
        assert_eq!(abort.condition, PlannedJobCondition::OnFailure);

        // Successor creation remains a read-only preflight until PREPARE is
        // executed by the durable worker.
        assert_eq!(
            fixture
                .durable
                .contribution_head("default", service_id)
                .unwrap()
                .unwrap()
                .etag(),
            current_head.etag()
        );
        assert_eq!(
            fixture
                .durable
                .contribution_revisions("default", Some(service_id))
                .unwrap()
                .len(),
            1
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
    fn v2_release_validate_returns_exact_topology_preview_without_side_effects() {
        let service_id = "v2-preview-api";
        let mut fixture = fixture(
            release_manifest(service_id, &format!("registry.example/ojos/api@{DIGEST}")),
            "READY",
        );
        let mut document = serde_json::to_value(release_manifest(
            service_id,
            &format!("registry.example/ojos/api@{DIGEST}"),
        ))
        .unwrap();
        make_v2_release_document(&mut document);
        replace_fixture_release_metadata(&fixture, service_id, "1.2.3", &document);
        let topology = apply_provider_topology(
            &fixture,
            "v2-preview-topology",
            "existing-provider",
            "deployment-existing-provider",
            "127.0.0.9:8080:existing-provider",
        );
        let request = ApiRequest {
            method: "POST".to_string(),
            path: "/api/v1/store/releases:validate".to_string(),
            headers: BTreeMap::from([(
                "idempotency-key".to_string(),
                "validate-v2-preview".to_string(),
            )]),
            body: json!({
                "service_id": service_id,
                "target_node_id": "node-1",
                "version": "1.2.3",
                "channel": "stable",
                "endpoint": "127.0.0.2:8080:v2-preview-api",
                "topology_id": topology.topology_id,
                "topology_etag": format!("\"{}\"", topology.revision_id),
                "start": true,
                "migration_policy": "SKIP",
                "config": {},
                "secret_refs": {}
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
            "request-v2-preview",
        )
        .unwrap();
        assert_eq!(response.status, 200, "{}", response.body);
        assert_eq!(response.body["data"]["valid"], true);
        let changes = response.body["data"]["topology_diff"]["changes"]
            .as_array()
            .expect("prospective Topology diff changes");
        assert_eq!(changes.len(), 1, "{}", response.body);
        assert_eq!(changes[0]["kind"], "endpoint_added");
        assert_eq!(changes[0]["endpoint"]["service_id"], service_id);
        assert_eq!(
            response.body["data"]["side_effects"],
            json!({
                "release_imports": 0,
                "operations": 0,
                "jobs": 0,
                "runtime_calls": 0,
            })
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
    fn v2_release_validate_rejects_missing_typed_config_before_any_operation() {
        let service_id = "v2-config-api";
        let mut fixture = fixture(
            release_manifest(service_id, &format!("registry.example/ojos/api@{DIGEST}")),
            "READY",
        );
        let mut release =
            release_manifest(service_id, &format!("registry.example/ojos/api@{DIGEST}"));
        release.config_schema = json!({
            "type": "object",
            "properties": {"workers": {"type": "integer"}},
            "required": ["workers"],
            "additionalProperties": false
        });
        release
            .runtime
            .env
            .insert("WORKERS".to_string(), "${config.workers}".to_string());
        let mut document = serde_json::to_value(release).unwrap();
        make_v2_release_document(&mut document);
        replace_fixture_release_metadata(&fixture, service_id, "1.2.3", &document);
        let response = route(
            &fixture.state,
            &mut fixture.console,
            Some(&fixture.durable),
            Some(&fixture.registry),
            Some(&fixture.artifact_store),
            &ApiRequest {
                method: "POST".to_string(),
                path: "/api/v1/store/releases:validate".to_string(),
                headers: BTreeMap::from([(
                    "idempotency-key".to_string(),
                    "validate-v2-config".to_string(),
                )]),
                body: json!({
                    "service_id": service_id,
                    "target_node_id": "node-1",
                    "version": "1.2.3",
                    "config": {},
                    "secret_refs": {}
                })
                .to_string(),
            },
            "request-v2-config",
        )
        .unwrap();
        assert_eq!(response.status, 422, "{}", response.body);
        assert_eq!(response.body["code"], "STORE_CONFIG_REQUIRED");
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
                    runtime_contract: RuntimeContract::standard_v1(),
                    runtime_policy_sha256: String::new(),
                    effective_runtime_sha256: String::new(),
                    runtime_attested: false,
                    desired_state: RuntimeDesiredState::Running,
                    observed_state: RuntimeObservedState::Running,
                    health: "HEALTHY".to_string(),
                },
                management_mode: orchestrator_storage::RuntimeManagementMode::Managed,
                endpoint: String::new(),
                external_probe_protocol: String::new(),
                external_probe_health_path: String::new(),
                last_observed_at_ms: 0,
                drift_reason: String::new(),
                credential_expires_at_ms: 0,
                credential_last_success_at_ms: 0,
                credential_last_error: String::new(),
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

    #[test]
    fn provider_candidates_require_fresh_attested_managed_runtime_evidence() {
        let service_id = "fresh-provider";
        let mut manifest =
            release_manifest(service_id, &format!("registry.example/ojos/api@{DIGEST}"));
        manifest.permissions.push("provider.read".to_string());
        manifest.apis = serde_json::from_value(json!([{
            "api_id": "provider.read",
            "protocol": "http",
            "port_name": "http",
            "path_prefix": "/provider",
            "methods": ["GET"],
            "visibility": "explicit",
            "auth_mode": "service",
            "permission": "provider.read",
            "stability": "stable",
            "version": "1.0.0"
        }]))
        .unwrap();
        let fixture = fixture(manifest, "READY");
        put_running_instance(
            &fixture,
            service_id,
            "deployment-fresh-provider",
            "container-fresh-provider",
            &format!("registry.example/ojos/api@{DIGEST}"),
        );
        let mut runtime = fixture
            .durable
            .runtime_instance("deployment-fresh-provider")
            .unwrap()
            .unwrap();
        runtime.endpoint = "10.0.0.1:8080:fresh-provider".to_string();
        runtime.instance.runtime_attested = true;
        fixture.durable.put_runtime_instance(&runtime).unwrap();
        assert_eq!(
            provider_candidates(&fixture.console, &fixture.durable)
                .unwrap()
                .len(),
            1
        );

        let mut facts = fixture
            .sqlite
            .node_runtime_facts("node-1")
            .unwrap()
            .unwrap();
        facts.received_at_ms = now_ms().saturating_sub(NODE_RUNTIME_FACTS_STALE_MS + 1);
        fixture.sqlite.put_node_runtime_facts(&facts).unwrap();
        assert!(
            provider_candidates(&fixture.console, &fixture.durable)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn composition_discovers_running_api_provider_outside_install_release_graph() {
        let fixture = fixture(
            release_manifest(
                "composition-consumer",
                &format!("registry.example/ojos/consumer@{DIGEST}"),
            ),
            "READY",
        );
        register_running_provider(
            &fixture,
            "problem-service",
            "deployment-problem-provider",
            "10.0.0.1:8080:problem-service",
            "problem.query.v1",
        );
        let node = fixture
            .durable
            .get_node("node-1")
            .unwrap()
            .expect("fixture node");

        // The provider is intentionally absent from the current Catalog
        // documents: requires.apis is a runtime binding, not a package edge.
        let providers = store_composition_providers(&fixture.durable, &[], &node).unwrap();
        assert!(providers.iter().any(|provider| {
            provider.provider_id == "deployment-problem-provider"
                && provider.capability == "problem.query.v1"
                && provider.service_id.as_deref() == Some("problem-service")
                && provider.kind == ProviderKindV1::Managed
        }));
    }

    #[test]
    fn provider_candidates_reject_stale_external_probe_evidence() {
        let service_id = "external-provider";
        let mut manifest =
            release_manifest(service_id, &format!("registry.example/ojos/api@{DIGEST}"));
        manifest.permissions.push("provider.read".to_string());
        manifest.apis = serde_json::from_value(json!([{
            "api_id": "provider.read",
            "protocol": "http",
            "port_name": "http",
            "path_prefix": "/provider",
            "methods": ["GET"],
            "visibility": "explicit",
            "auth_mode": "workload",
            "permission": "provider.read",
            "stability": "stable",
            "version": "1.0.0"
        }]))
        .unwrap();
        let fixture = fixture(manifest, "READY");
        let observed_at_ms = now_ms();
        let mut runtime = StoredRuntimeInstance {
            node_id: "external".to_string(),
            instance: RuntimeInstance {
                deployment_id: "deployment-external-provider".to_string(),
                service_id: service_id.to_string(),
                release_version: "1.2.3".to_string(),
                container_id: String::new(),
                artifact_digest: format!("registry.example/ojos/api@{DIGEST}"),
                runtime_contract: orchestrator_runtime::RuntimeContract::standard_v1(),
                runtime_policy_sha256: String::new(),
                effective_runtime_sha256: String::new(),
                runtime_attested: false,
                desired_state: RuntimeDesiredState::Running,
                observed_state: RuntimeObservedState::Running,
                health: "HEALTHY".to_string(),
            },
            management_mode: orchestrator_storage::RuntimeManagementMode::External,
            endpoint: "http://external.example".to_string(),
            external_probe_protocol: "http".to_string(),
            external_probe_health_path: "/health".to_string(),
            last_observed_at_ms: observed_at_ms,
            drift_reason: String::new(),
            credential_expires_at_ms: 0,
            credential_last_success_at_ms: 0,
            credential_last_error: String::new(),
            updated_at: format!("unix-ms:{observed_at_ms}"),
        };
        fixture.durable.put_runtime_instance(&runtime).unwrap();
        assert_eq!(
            provider_candidates(&fixture.console, &fixture.durable)
                .unwrap()
                .len(),
            1
        );

        runtime.instance.observed_state = RuntimeObservedState::Unknown;
        runtime.instance.health = "UNHEALTHY".to_string();
        runtime.drift_reason = "external health probe failed".to_string();
        fixture.durable.put_runtime_instance(&runtime).unwrap();
        assert!(
            provider_candidates(&fixture.console, &fixture.durable)
                .unwrap()
                .is_empty()
        );

        runtime.instance.observed_state = RuntimeObservedState::Running;
        runtime.instance.health = "HEALTHY".to_string();
        runtime.drift_reason.clear();
        runtime.last_observed_at_ms =
            observed_at_ms.saturating_sub(crate::durable::EXTERNAL_RUNTIME_PROBE_STALE_MS + 1);
        fixture.durable.put_runtime_instance(&runtime).unwrap();
        assert!(
            provider_candidates(&fixture.console, &fixture.durable)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn judge_runtime_plan_requires_the_exact_node_local_oci_allowlist_entry() {
        let fixture = fixture(
            release_manifest(
                "judge-worker",
                &format!("registry.example/ojos/api@{DIGEST}"),
            ),
            "READY",
        );
        let mut contract_value = serde_json::to_value(release_manifest(
            "judge-worker",
            &format!("registry.example/ojos/api@{DIGEST}"),
        ))
        .unwrap();
        contract_value["schema_version"] = json!(2);
        contract_value["provides"] = json!({"apis": []});
        contract_value["requires"] = json!({"apis": []});
        contract_value["events"] = json!({"publishes": [], "subscribes": []});
        contract_value["runtime_contract"] = json!({
            "id": "judge-sandbox-v1",
            "sha256": orchestrator_runtime::JUDGE_SANDBOX_V1_PROFILE_SHA256,
            "binding_directory": "/run/ojos/service",
            "identity_mode": "workload",
            "credential_delivery": "file",
            "restart_on_change": false
        });
        let contract = ServiceReleaseContract::from_json_value(contract_value).unwrap();
        let image = format!("registry.example/ojos/api@{DIGEST}");
        let mut stored_facts = fixture
            .sqlite
            .node_runtime_facts("node-1")
            .unwrap()
            .unwrap();
        let mut facts: NodeRuntimeFactsV1 =
            serde_json::from_value(stored_facts.facts.clone()).unwrap();
        facts
            .allowed_contracts
            .push(RuntimeContract::judge_sandbox_v1());
        facts.judge_sandbox_allowed_images = vec![format!(
            "registry.example/ojos/other@sha256:{}",
            "b".repeat(64)
        )];
        stored_facts.facts = serde_json::to_value(&facts).unwrap();
        fixture
            .sqlite
            .put_node_runtime_facts(&stored_facts)
            .unwrap();
        let node = NodeRecord {
            node_id: "node-1".to_string(),
            host_ip: "127.0.0.2".to_string(),
            parent_node_id: String::new(),
            role: "standalone".to_string(),
            labels: json!({}),
            status: "READY".to_string(),
            created_at: "t0".to_string(),
            updated_at: "t0".to_string(),
        };
        let error = ensure_release_runtime_supported(&fixture.durable, &node, &contract, &image)
            .unwrap_err();
        assert_eq!(error.code, "STORE_RUNTIME_ARTIFACT_NOT_ALLOWED");

        facts.judge_sandbox_allowed_images = vec![image.clone()];
        stored_facts.facts = serde_json::to_value(facts).unwrap();
        fixture
            .sqlite
            .put_node_runtime_facts(&stored_facts)
            .unwrap();
        assert_eq!(
            ensure_release_runtime_supported(&fixture.durable, &node, &contract, &image).unwrap(),
            RuntimeContract::judge_sandbox_v1()
        );
    }

    fn active_merge_binding(
        id: &str,
        consumer: &str,
        source: &str,
        provider: &str,
        target: &str,
    ) -> ApiBinding {
        ApiBinding {
            binding_id: id.to_string(),
            requirement_name: "storage_get".to_string(),
            api_id: "storage.object.get".to_string(),
            api_version: "1.0.0".to_string(),
            consumer_deployment_id: consumer.to_string(),
            consumer_service_id: "worker".to_string(),
            consumer_node_id: "node-b".to_string(),
            consumer_endpoint: source.to_string(),
            provider_deployment_id: provider.to_string(),
            provider_service_id: "storage".to_string(),
            provider_node_id: "node-a".to_string(),
            provider_endpoint: target.to_string(),
            provider_path: "/objects".to_string(),
            virtual_endpoint: "/internal/apis/storage.object.get".to_string(),
            protocol: "http".to_string(),
            methods: vec!["GET".to_string()],
            auth_mode: "workload".to_string(),
            provider_auth_mode: "workload".to_string(),
            permission: "storage.object.read".to_string(),
            timeout_ms: Some(30_000),
            topology_id: "primary".to_string(),
            topology_revision_id: "revision-1".to_string(),
            link_source_endpoint: source.to_string(),
            link_target_endpoint: target.to_string(),
            credential_ref: String::new(),
            credential_generation: 2,
            context_generation: 2,
            desired_state: ApiBindingDesiredState::Active,
            observed_state: ApiBindingObservedState::Active,
            health: ApiBindingHealth::Healthy,
            drift: Vec::new(),
            last_operation_id: "operation-1".to_string(),
            state: ApiBindingState::Active,
            optional: false,
            reason: String::new(),
            created_at: "unix-ms:1".to_string(),
            updated_at: "unix-ms:1".to_string(),
        }
    }

    fn staged_context_binding(
        id: &str,
        requirement: &str,
        api_id: &str,
        provider: &str,
        optional: bool,
    ) -> ApiBinding {
        let mut binding = active_merge_binding(
            id,
            "consumer-worker",
            "10.0.0.2:9101:worker",
            provider,
            &format!("10.0.0.1:8080:{provider}"),
        );
        binding.requirement_name = requirement.to_string();
        binding.api_id = api_id.to_string();
        binding.topology_id = "primary".to_string();
        binding.topology_revision_id = "revision-2".to_string();
        binding.credential_generation = 3;
        binding.context_generation = 3;
        binding.observed_state = ApiBindingObservedState::Pending;
        binding.health = ApiBindingHealth::Unknown;
        binding.last_operation_id = "operation-rebind".to_string();
        binding.state = ApiBindingState::Pending;
        binding.optional = optional;
        binding
    }

    fn staged_context_plan(bindings: Vec<ApiBinding>) -> StoreTopologyApplyPlan {
        StoreTopologyApplyPlan {
            topology_id: "primary".to_string(),
            revision_id: "revision-2".to_string(),
            staged_bindings: bindings,
            previous_bindings: Vec::new(),
        }
    }

    #[test]
    fn staged_apply_context_accepts_required_multi_binding_compatible_provider_rebind() {
        let rebound = staged_context_binding(
            "binding-storage-head",
            "storage_head",
            "storage.object.head",
            "storage-compatible-b",
            false,
        );
        let required_sibling = staged_context_binding(
            "binding-storage-get",
            "storage_get",
            "storage.object.get",
            "storage-a",
            false,
        );
        let plan = staged_context_plan(vec![rebound, required_sibling]);

        let desired = StagedApplyDesiredContextBindings::from_plans(
            std::slice::from_ref(&plan),
            "consumer-worker",
        )
        .unwrap();

        assert_eq!(desired.as_slice().len(), 2);
        assert!(desired.as_slice().iter().all(|binding| {
            binding.state == ApiBindingState::Resolved
                && binding.observed_state == "RESOLVED"
                && binding.context_generation == 3
        }));
        assert_eq!(
            desired
                .as_slice()
                .iter()
                .find(|binding| binding.requirement_name == "storage_head")
                .unwrap()
                .provider_deployment_id,
            "storage-compatible-b"
        );
        assert_eq!(
            desired
                .as_slice()
                .iter()
                .find(|binding| binding.requirement_name == "storage_get")
                .unwrap()
                .provider_deployment_id,
            "storage-a"
        );
        assert!(
            plan.staged_bindings
                .iter()
                .all(|binding| binding.state == ApiBindingState::Pending),
            "the materializable view must not mutate durable staged bindings"
        );
    }

    #[test]
    fn staged_apply_context_optional_revoke_preserves_required_sibling() {
        let mut optional_revoke = staged_context_binding(
            "binding-metrics",
            "metrics",
            "metrics.read",
            "metrics-a",
            true,
        );
        optional_revoke.desired_state = ApiBindingDesiredState::Revoked;
        let required_sibling = staged_context_binding(
            "binding-storage-get",
            "storage_get",
            "storage.object.get",
            "storage-a",
            false,
        );
        let plan = staged_context_plan(vec![optional_revoke, required_sibling]);

        let desired = StagedApplyDesiredContextBindings::from_plans(
            std::slice::from_ref(&plan),
            "consumer-worker",
        )
        .unwrap();

        assert_eq!(desired.as_slice().len(), 1);
        assert_eq!(desired.as_slice()[0].requirement_name, "storage_get");
        assert_eq!(desired.as_slice()[0].provider_deployment_id, "storage-a");
        assert_eq!(desired.as_slice()[0].context_generation, 3);
    }

    #[test]
    fn staged_apply_context_rejects_an_illegal_pending_binding() {
        let mut invalid = staged_context_binding(
            "binding-storage-get",
            "storage_get",
            "storage.object.get",
            "storage-a",
            false,
        );
        invalid.provider_deployment_id.clear();
        let plan = staged_context_plan(vec![invalid]);

        let error = StagedApplyDesiredContextBindings::from_plans(
            std::slice::from_ref(&plan),
            "consumer-worker",
        )
        .unwrap_err();

        assert_eq!(error.status, 409);
        assert_eq!(error.code, "STORE_STAGED_BINDING_CONTEXT_INVALID");

        let first = staged_context_binding(
            "binding-storage-get",
            "storage_get",
            "storage.object.get",
            "storage-a",
            false,
        );
        let mut second = staged_context_binding(
            "binding-storage-head",
            "storage_head",
            "storage.object.head",
            "storage-a",
            false,
        );
        second.last_operation_id = "operation-other".to_string();
        let split_operation_plan = staged_context_plan(vec![first, second]);
        let error = StagedApplyDesiredContextBindings::from_plans(
            std::slice::from_ref(&split_operation_plan),
            "consumer-worker",
        )
        .unwrap_err();
        assert_eq!(error.code, "STORE_STAGED_BINDING_CONTEXT_INVALID");
        assert!(error.detail.contains("more than one Operation"));
    }

    #[test]
    fn managed_service_context_rejects_any_missing_required_sibling() {
        let mut document = serde_json::to_value(release_manifest(
            "consumer-worker",
            &format!("registry.example/ojos/consumer-worker@{DIGEST}"),
        ))
        .unwrap();
        make_v2_release_document(&mut document);
        document["requires"] = json!({
            "apis": [
                {
                    "name": "storage_get",
                    "id": "storage.object.get",
                    "version": ">=1.0.0, <2.0.0",
                    "optional": false,
                    "selection": "explicit",
                    "timeout_ms": 300000
                },
                {
                    "name": "storage_head",
                    "id": "storage.object.head",
                    "version": ">=1.0.0, <2.0.0",
                    "optional": false,
                    "selection": "explicit",
                    "timeout_ms": 300000
                }
            ]
        });
        let contract = ServiceReleaseContract::from_json_value(document).unwrap();
        let fixture = fixture(
            release_manifest(
                "fixture-root",
                &format!("registry.example/ojos/fixture-root@{DIGEST}"),
            ),
            "READY",
        );
        let mut only_storage_get = staged_context_binding(
            "binding-storage-get",
            "storage_get",
            "storage.object.get",
            "storage-a",
            false,
        );
        only_storage_get.state = ApiBindingState::Active;
        only_storage_get.observed_state = ApiBindingObservedState::Active;

        let error = managed_service_context_spec(
            &fixture.durable,
            &contract,
            "node-1",
            &[only_storage_get],
            false,
        )
        .unwrap_err();

        assert_eq!(error.status, 409);
        assert_eq!(error.code, "STORE_REQUIRED_BINDING_CONTEXT_MISSING");
        assert!(error.detail.contains("storage_head"));
        assert!(!error.detail.contains("storage_get"));
    }

    #[test]
    fn binding_context_transition_revokes_last_required_binding_and_can_restore_it() {
        let mut environment = TestEnv::lock();
        environment.set(
            "ORCHESTRATOR_GATEWAY_WORKLOAD_ORIGIN",
            "http://127.0.0.1:18000",
        );
        let consumer_manifest = legacy_consumer_manifest("consumer-worker", "storage.object.get");
        let fixture = fixture(consumer_manifest, "READY");
        put_running_instance(
            &fixture,
            "consumer-worker",
            "consumer-worker",
            "container-consumer-worker",
            &format!("registry.example/ojos/consumer-worker@{DIGEST}"),
        );
        let mut previous = staged_context_binding(
            "binding-storage-get",
            "storage.object.get",
            "storage.object.get",
            "storage-a",
            false,
        );
        previous.consumer_deployment_id = "consumer-worker".to_string();
        previous.state = ApiBindingState::Active;
        previous.observed_state = ApiBindingObservedState::Active;
        previous.desired_state = ApiBindingDesiredState::Active;
        previous.topology_revision_id = "revision-1".to_string();
        fixture
            .durable
            .replace_topology_api_bindings("primary", std::slice::from_ref(&previous))
            .unwrap();
        let previous_context = ManagedServiceContextSpec {
            generation: previous.context_generation,
            node_id: "node-1".to_string(),
            gateway_origin: "http://127.0.0.1:18000".to_string(),
            gateway_ca_pem: None,
            bindings: BTreeMap::from([(
                previous.requirement_name.clone(),
                ManagedApiBinding {
                    binding_id: previous.binding_id.clone(),
                    api_id: previous.api_id.clone(),
                    timeout_ms: previous.timeout_ms.unwrap(),
                    context_generation: previous.context_generation,
                },
            )]),
            events: None,
            workload_verifier: None,
        };
        previous_context.validate().unwrap();
        fixture
            .durable
            .put_state(
                "managed-service-context-v1",
                "consumer-worker",
                &ManagedServiceContextProjection {
                    current: Some(previous_context.clone()),
                    last_nonempty: previous_context.clone(),
                    revoked: false,
                },
            )
            .unwrap();
        let mut revoked = previous.clone();
        revoked.desired_state = ApiBindingDesiredState::Revoked;
        revoked.observed_state = ApiBindingObservedState::Pending;
        revoked.state = ApiBindingState::Pending;
        revoked.topology_revision_id = "revision-2".to_string();
        let plan = StoreTopologyApplyPlan {
            topology_id: "primary".to_string(),
            revision_id: "revision-2".to_string(),
            staged_bindings: vec![revoked],
            previous_bindings: vec![previous],
        };

        let transitions = binding_context_transition_plans(
            &fixture.durable,
            &[plan],
            &BTreeSet::from(["consumer-worker".to_string()]),
        )
        .unwrap();

        assert_eq!(transitions.len(), 1);
        assert!(transitions[0].forward.context.is_none());
        assert_eq!(
            transitions[0].forward.previous_context.as_ref(),
            Some(&previous_context)
        );
        assert_eq!(
            transitions[0].rollback.context.as_ref(),
            Some(&previous_context)
        );
        assert_eq!(
            transitions[0].rollback.previous_context.as_ref(),
            Some(&previous_context)
        );
    }

    #[test]
    fn adding_second_consumer_preserves_first_consumer_binding_identity_and_link() {
        let first = active_merge_binding(
            "binding-first",
            "consumer-first",
            "10.0.0.2:9000:worker",
            "storage-a",
            "10.0.0.1:8080:storage",
        );
        let mut second = active_merge_binding(
            "binding-second",
            "consumer-second",
            "10.0.0.3:9000:worker",
            "storage-a",
            "10.0.0.1:8080:storage",
        );
        second.state = ApiBindingState::Resolved;
        second.observed_state = ApiBindingObservedState::Resolved;
        let merged = merge_store_consumer_bindings(
            std::slice::from_ref(&first),
            std::slice::from_ref(&second),
            StoreConsumerBindingMergeContext {
                consumer_deployment_id: "consumer-second",
                replaced_consumer_deployment_id: None,
                consumer_endpoint: "10.0.0.3:9000:worker",
                topology_id: "primary",
                revision_id: "revision-2",
                operation_id: "operation-2",
            },
        );
        let preserved = merged
            .iter()
            .find(|binding| binding.consumer_deployment_id == "consumer-first")
            .expect("first consumer is preserved");
        assert_eq!(preserved.binding_id, "binding-first");
        assert_eq!(preserved.link_source_endpoint, "10.0.0.2:9000:worker");
        assert_eq!(preserved.link_target_endpoint, "10.0.0.1:8080:storage");
        assert_eq!(preserved.provider_deployment_id, "storage-a");
        let added = merged
            .iter()
            .find(|binding| binding.consumer_deployment_id == "consumer-second")
            .expect("second consumer is added");
        assert_eq!(added.binding_id, "binding-second");
        assert_eq!(added.link_source_endpoint, "10.0.0.3:9000:worker");
    }

    #[test]
    fn formal_store_topology_input_requires_a_strong_etag() {
        let selected = normalize_store_topology_selection("primary", "\"revision-7\"", None)
            .unwrap()
            .unwrap();
        assert_eq!(selected.topology_id, "primary");
        assert_eq!(selected.revision_id, "revision-7");
        let error = normalize_store_topology_selection("primary", "revision-7", None)
            .expect_err("raw revision ids are not ETags");
        assert_eq!(error.code, "STORE_TOPOLOGY_ETAG_INVALID");
    }

    #[test]
    fn backend_worker_uses_a_deterministic_logical_endpoint_without_publishing_a_port() {
        let mut release = release_manifest(
            "judge-worker",
            &format!("registry.example/ojos/api@{DIGEST}"),
        );
        release.service_type = "backend-worker".to_string();
        release.backend.port = 9101;
        let node = NodeRecord {
            node_id: "node-b".to_string(),
            host_ip: "10.20.30.40".to_string(),
            parent_node_id: String::new(),
            role: "node".to_string(),
            labels: json!({}),
            status: "READY".to_string(),
            created_at: "t0".to_string(),
            updated_at: "t0".to_string(),
        };
        let endpoint = effective_managed_endpoint("", &node, &release).unwrap();
        assert_eq!(endpoint, "10.20.30.40:9101:judge-worker");
        assert!(
            managed_published_endpoint(&endpoint, "judge-worker", &node, &release)
                .unwrap()
                .is_none(),
            "the logical endpoint must never publish the worker health port"
        );
        assert_eq!(
            effective_managed_endpoint("10.20.30.40:19101:judge-worker", &node, &release).unwrap(),
            "10.20.30.40:19101:judge-worker"
        );
    }

    fn topology_install_graph_with_contribution_abort(
        operation_id: &str,
        contribution_abort_step: Option<&str>,
    ) -> PlanOperation {
        let mut jobs = vec![PlannedJob {
            step_id: "install-root".to_string(),
            node_id: "node-1".to_string(),
            kind: JobKind::Install,
            depends_on: vec![],
            condition: PlannedJobCondition::OnSuccess,
            payload: json!({"deployment_id": "deployment-root"}),
            max_attempts: 1,
        }];
        append_install_topology_jobs(
            &mut jobs,
            &StoreTopologyApplyPlan {
                topology_id: "topology-1".to_string(),
                revision_id: "revision-2".to_string(),
                staged_bindings: vec![],
                previous_bindings: vec![],
            },
            "install-root",
            "node-1",
            "deployment-root",
        );
        if let Some(contribution_abort_step) = contribution_abort_step {
            jobs.push(PlannedJob {
                step_id: contribution_abort_step.to_string(),
                node_id: CONTROL_PLANE_NODE_ID.to_string(),
                kind: JobKind::TopologyApply,
                depends_on: vec![
                    "install-root".to_string(),
                    "topology-binding-finalize-success".to_string(),
                ],
                condition: PlannedJobCondition::OnFailure,
                payload: json!({"controller": "contribution", "phase": "ABORT"}),
                max_attempts: 1,
            });
            add_job_dependency(
                &mut jobs,
                "remove-root-after-topology-abort",
                contribution_abort_step,
            )
            .unwrap();
        }
        PlanOperation {
            operation_id: operation_id.to_string(),
            action: "release.install".to_string(),
            target_type: "Release".to_string(),
            target_id: "root@1.0.0".to_string(),
            request: json!({"auto_enqueue": true}),
            jobs,
        }
    }

    fn topology_install_graph(operation_id: &str) -> PlanOperation {
        topology_install_graph_with_contribution_abort(operation_id, None)
    }

    fn topology_install_graph_with_contribution_commit(operation_id: &str) -> PlanOperation {
        let mut plan = topology_install_graph(operation_id);
        plan.jobs.push(PlannedJob {
            step_id: "contribution-commit-test".to_string(),
            node_id: CONTROL_PLANE_NODE_ID.to_string(),
            kind: JobKind::TopologyApply,
            depends_on: vec!["install-root".to_string()],
            condition: PlannedJobCondition::OnSuccess,
            payload: json!({"controller": "contribution", "phase": "COMMIT"}),
            max_attempts: 1,
        });
        add_job_dependency(
            &mut plan.jobs,
            "topology-binding-finalize-success",
            "contribution-commit-test",
        )
        .unwrap();
        add_job_dependency(
            &mut plan.jobs,
            "topology-binding-finalize-failure",
            "contribution-commit-test",
        )
        .unwrap();
        plan.jobs.push(PlannedJob {
            step_id: "contribution-abort-test".to_string(),
            node_id: CONTROL_PLANE_NODE_ID.to_string(),
            kind: JobKind::TopologyApply,
            depends_on: vec![
                "contribution-commit-test".to_string(),
                "topology-binding-finalize-success".to_string(),
            ],
            condition: PlannedJobCondition::OnFailure,
            payload: json!({"controller": "contribution", "phase": "ABORT"}),
            max_attempts: 1,
        });
        add_job_dependency(
            &mut plan.jobs,
            "remove-root-after-topology-abort",
            "contribution-abort-test",
        )
        .unwrap();
        plan
    }

    fn start_topology_install_graph(operation_id: &str) -> (MemoryOperationStore, MemoryJobStore) {
        let mut operations = MemoryOperationStore::default();
        let mut jobs = MemoryJobStore::default();
        let mut coordinator = OperationCoordinator::new(&mut operations, &mut jobs);
        coordinator
            .plan(topology_install_graph(operation_id), 0)
            .unwrap();
        coordinator.confirm(operation_id, 1).unwrap();
        coordinator.enqueue(operation_id, 2).unwrap();
        (operations, jobs)
    }

    fn complete_next_topology_job(
        jobs: &mut MemoryJobStore,
        node_id: &str,
        token: &str,
        status: CompletionStatus,
        now_ms: i64,
    ) -> String {
        let job = jobs
            .claim(ClaimRequest {
                node_id: node_id.to_string(),
                instance_id: format!("worker-{node_id}"),
                lease_token: token.to_string(),
                now_ms,
                lease_ms: 30_000,
            })
            .unwrap()
            .expect("expected queued topology install job");
        let job_id = job.job_id.clone();
        jobs.complete(CompleteRequest {
            job_id: job.job_id,
            lease_token: token.to_string(),
            status,
            result: json!({"node_id": node_id}),
            error_message: String::new(),
            now_ms: now_ms + 1,
            events: vec![],
        })
        .unwrap();
        job_id
    }

    #[test]
    fn topology_install_root_failure_aborts_without_uninstalling_an_uncommitted_root() {
        let operation_id = "install-root-failure";
        let (mut operations, mut jobs) = start_topology_install_graph(operation_id);
        complete_next_topology_job(
            &mut jobs,
            "node-1",
            "root-failed",
            CompletionStatus::Failed,
            10,
        );
        let operation = OperationCoordinator::new(&mut operations, &mut jobs)
            .project(operation_id, 12)
            .unwrap();
        assert!(
            operation
                .active_binding("topology-binding-finalize-success")
                .is_none()
        );
        assert!(
            operation
                .active_binding("topology-binding-finalize-failure")
                .is_some()
        );
        complete_next_topology_job(
            &mut jobs,
            CONTROL_PLANE_NODE_ID,
            "abort-after-root",
            CompletionStatus::Succeeded,
            20,
        );
        let operation = OperationCoordinator::new(&mut operations, &mut jobs)
            .project(operation_id, 22)
            .unwrap();
        assert_eq!(operation.status, DurableOperationStatus::Failed);
        assert!(
            operation
                .active_binding("remove-root-after-topology-abort")
                .is_none()
        );
    }

    #[test]
    fn topology_install_finalize_failure_aborts_then_uninstalls_root() {
        let operation_id = "install-finalize-failure";
        let (mut operations, mut jobs) = start_topology_install_graph(operation_id);
        complete_next_topology_job(
            &mut jobs,
            "node-1",
            "root-success",
            CompletionStatus::Succeeded,
            10,
        );
        OperationCoordinator::new(&mut operations, &mut jobs)
            .project(operation_id, 12)
            .unwrap();
        complete_next_topology_job(
            &mut jobs,
            CONTROL_PLANE_NODE_ID,
            "finalize-failed",
            CompletionStatus::Failed,
            20,
        );
        OperationCoordinator::new(&mut operations, &mut jobs)
            .project(operation_id, 22)
            .unwrap();
        complete_next_topology_job(
            &mut jobs,
            CONTROL_PLANE_NODE_ID,
            "abort-success",
            CompletionStatus::Succeeded,
            30,
        );
        let operation = OperationCoordinator::new(&mut operations, &mut jobs)
            .project(operation_id, 32)
            .unwrap();
        assert!(
            operation
                .active_binding("remove-root-after-topology-abort")
                .is_some()
        );
        complete_next_topology_job(
            &mut jobs,
            "node-1",
            "cleanup-success",
            CompletionStatus::Succeeded,
            40,
        );
        let operation = OperationCoordinator::new(&mut operations, &mut jobs)
            .project(operation_id, 42)
            .unwrap();
        assert_eq!(operation.status, DurableOperationStatus::Failed);
        assert_eq!(
            operation.result["remove-root-after-topology-abort"]["status"],
            json!(JobStatus::Succeeded)
        );
    }

    #[test]
    fn topology_cleanup_waits_for_both_topology_and_contribution_abort() {
        let operation_id = "install-dual-abort-gate";
        let mut operations = MemoryOperationStore::default();
        let mut jobs = MemoryJobStore::default();
        let mut coordinator = OperationCoordinator::new(&mut operations, &mut jobs);
        coordinator
            .plan(
                topology_install_graph_with_contribution_abort(
                    operation_id,
                    Some("contribution-abort-test"),
                ),
                0,
            )
            .unwrap();
        coordinator.confirm(operation_id, 1).unwrap();
        coordinator.enqueue(operation_id, 2).unwrap();

        complete_next_topology_job(
            &mut jobs,
            "node-1",
            "root-success",
            CompletionStatus::Succeeded,
            10,
        );
        OperationCoordinator::new(&mut operations, &mut jobs)
            .project(operation_id, 12)
            .unwrap();
        complete_next_topology_job(
            &mut jobs,
            CONTROL_PLANE_NODE_ID,
            "finalize-failed",
            CompletionStatus::Failed,
            20,
        );
        OperationCoordinator::new(&mut operations, &mut jobs)
            .project(operation_id, 22)
            .unwrap();

        complete_next_topology_job(
            &mut jobs,
            CONTROL_PLANE_NODE_ID,
            "first-abort",
            CompletionStatus::Succeeded,
            30,
        );
        let operation = OperationCoordinator::new(&mut operations, &mut jobs)
            .project(operation_id, 32)
            .unwrap();
        assert!(
            operation
                .active_binding("remove-root-after-topology-abort")
                .is_none(),
            "runtime cleanup must wait while either projection ABORT is incomplete"
        );

        complete_next_topology_job(
            &mut jobs,
            CONTROL_PLANE_NODE_ID,
            "second-abort",
            CompletionStatus::Succeeded,
            40,
        );
        let operation = OperationCoordinator::new(&mut operations, &mut jobs)
            .project(operation_id, 42)
            .unwrap();
        assert!(
            operation
                .active_binding("remove-root-after-topology-abort")
                .is_some(),
            "runtime cleanup may materialize only after both ABORT jobs succeed"
        );
    }

    #[test]
    fn contribution_commit_failure_materializes_both_aborts_before_runtime_cleanup() {
        let operation_id = "install-contribution-commit-failure";
        let mut operations = MemoryOperationStore::default();
        let mut jobs = MemoryJobStore::default();
        let mut coordinator = OperationCoordinator::new(&mut operations, &mut jobs);
        coordinator
            .plan(
                topology_install_graph_with_contribution_commit(operation_id),
                0,
            )
            .unwrap();
        coordinator.confirm(operation_id, 1).unwrap();
        coordinator.enqueue(operation_id, 2).unwrap();

        complete_next_topology_job(
            &mut jobs,
            "node-1",
            "root-success",
            CompletionStatus::Succeeded,
            10,
        );
        OperationCoordinator::new(&mut operations, &mut jobs)
            .project(operation_id, 12)
            .unwrap();
        complete_next_topology_job(
            &mut jobs,
            CONTROL_PLANE_NODE_ID,
            "contribution-commit-failed",
            CompletionStatus::Failed,
            20,
        );
        let operation = OperationCoordinator::new(&mut operations, &mut jobs)
            .project(operation_id, 22)
            .unwrap();
        assert!(
            operation
                .active_binding("topology-binding-finalize-success")
                .is_none(),
            "Topology FINALIZE must remain frozen after Contribution COMMIT fails"
        );
        assert!(
            operation
                .active_binding("topology-binding-finalize-failure")
                .is_some(),
            "Topology ABORT must use the failed Contribution COMMIT as a direct witness"
        );
        assert!(
            operation
                .active_binding("contribution-abort-test")
                .is_some(),
            "Contribution ABORT must materialize alongside Topology ABORT"
        );

        complete_next_topology_job(
            &mut jobs,
            CONTROL_PLANE_NODE_ID,
            "first-abort",
            CompletionStatus::Succeeded,
            30,
        );
        let operation = OperationCoordinator::new(&mut operations, &mut jobs)
            .project(operation_id, 32)
            .unwrap();
        assert!(
            operation
                .active_binding("remove-root-after-topology-abort")
                .is_none(),
            "runtime cleanup must not race the second projection ABORT"
        );

        complete_next_topology_job(
            &mut jobs,
            CONTROL_PLANE_NODE_ID,
            "second-abort",
            CompletionStatus::Succeeded,
            40,
        );
        let operation = OperationCoordinator::new(&mut operations, &mut jobs)
            .project(operation_id, 42)
            .unwrap();
        assert!(
            operation
                .active_binding("remove-root-after-topology-abort")
                .is_some(),
            "runtime cleanup may materialize only after both projections restore"
        );
    }

    #[test]
    fn topology_install_success_never_materializes_abort_or_cleanup() {
        let operation_id = "install-finalize-success";
        let (mut operations, mut jobs) = start_topology_install_graph(operation_id);
        complete_next_topology_job(
            &mut jobs,
            "node-1",
            "root-success",
            CompletionStatus::Succeeded,
            10,
        );
        OperationCoordinator::new(&mut operations, &mut jobs)
            .project(operation_id, 12)
            .unwrap();
        complete_next_topology_job(
            &mut jobs,
            CONTROL_PLANE_NODE_ID,
            "finalize-success",
            CompletionStatus::Succeeded,
            20,
        );
        let operation = OperationCoordinator::new(&mut operations, &mut jobs)
            .project(operation_id, 22)
            .unwrap();
        assert_eq!(operation.status, DurableOperationStatus::Succeeded);
        assert!(
            operation
                .active_binding("topology-binding-finalize-failure")
                .is_none()
        );
        assert!(
            operation
                .active_binding("remove-root-after-topology-abort")
                .is_none()
        );
    }
}
