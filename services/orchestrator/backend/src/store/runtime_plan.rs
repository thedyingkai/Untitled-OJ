//! Store runtime plan responsibilities.
use crate::store::artifacts::is_sha256;
use crate::store::commands::required_text;
use crate::store::error::StoreError;
use crate::store::node::{
    node_provider_label, provider_identifier, require_node_provider, storage_provider_selection,
};
use orchestrator_core::ApiBinding;
use orchestrator_core::ApiBindingState;
use orchestrator_core::NodeRecord;
use orchestrator_core::ServiceReleaseContract;
use orchestrator_core::ServiceReleaseManifest;
use orchestrator_core::parse_endpoint_id;
use orchestrator_core::validate_endpoint_id;
use orchestrator_manager::MigrationPolicyV2;
use orchestrator_manager::store::config::ValidatedReleaseConfig;
use orchestrator_manager::store::config::validate_release_config;
use orchestrator_manager::store::stable_service_instance_id;
use orchestrator_runtime::AuthPipelineStep;
use orchestrator_runtime::AuthServiceIdentitySpec;
use orchestrator_runtime::GatewayPipelineStep;
use orchestrator_runtime::GatewayRouteSpec;
use orchestrator_runtime::MANAGED_EVENT_STREAM_V1;
use orchestrator_runtime::OciImageReference;
use orchestrator_runtime::OciMigrationStep;
use orchestrator_runtime::RedisNamespaceSpec;
use orchestrator_runtime::ReleasePipelinePayload;
use orchestrator_runtime::ResourceClaimStepV1;
use orchestrator_runtime::RuntimeInstallPayload;
use orchestrator_runtime::RuntimeMaterializationStep;
use orchestrator_runtime::StorageResourceSpec;
use orchestrator_runtime::TypedProvisionerStep;
use serde_json::Value;
use sha2::Digest;
use sha2::Sha256;
use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::net::IpAddr;

#[allow(clippy::too_many_arguments)]
pub(crate) fn release_pipeline_payload(
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
) -> Result<Option<ReleasePipelinePayload>, StoreError> {
    if !release.runtime.kind.eq_ignore_ascii_case("image") {
        return Err(StoreError::new(
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
            return Err(StoreError::new(
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
            return Err(StoreError::new(
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
            return Err(StoreError::new(
                422,
                "STORE_DESTRUCTIVE_MIGRATION_REJECTED",
                format!(
                    "migration {} is destructive; v1 Store requires a separately approved migration workflow",
                    migration.version
                ),
            ));
        }
        if !is_sha256(migration.checksum.trim()) {
            return Err(StoreError::new(
                422,
                "STORE_MIGRATION_CHECKSUM_REQUIRED",
                format!(
                    "migration {} requires sha256:<64 lowercase hex> checksum",
                    migration.version
                ),
            ));
        }
        let oci = migration.oci.as_ref().ok_or_else(|| {
            StoreError::new(
                422,
                "STORE_MIGRATION_OCI_REQUIRED",
                format!(
                    "migration {} must declare a signed one-shot OCI runner",
                    migration.version
                ),
            )
        })?;
        if oci.command.is_empty() || oci.timeout_ms == 0 || oci.timeout_ms > 60 * 60_000 {
            return Err(StoreError::new(
                422,
                "STORE_MIGRATION_OCI_INVALID",
                format!(
                    "migration {} OCI runner requires command and timeout_ms between 1 and 3600000",
                    migration.version
                ),
            ));
        }
        let image = OciImageReference::parse(&oci.image).map_err(|error| {
            StoreError::new(
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
            return Err(StoreError::new(
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
            return Err(StoreError::new(
                422,
                "STORE_NODE_HOST_REQUIRED",
                "target Node must advertise host_ip before Gateway routes can be published",
            ));
        }
        if !release.routes.is_empty()
            && !matches!(release.backend.protocol.as_str(), "http" | "https")
        {
            return Err(StoreError::new(
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
                return Err(StoreError::new(
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
                return Err(StoreError::new(
                    422,
                    "STORE_BINDING_GATEWAY_PROTOCOL_UNSUPPORTED",
                    format!(
                        "API binding {} uses unsupported provider protocol {}",
                        binding.requirement_name, binding.protocol
                    ),
                ));
            }
            validate_endpoint_id(&binding.provider_endpoint).map_err(|error| {
                StoreError::new(
                    422,
                    "STORE_BINDING_PROVIDER_ENDPOINT_INVALID",
                    format!(
                        "API binding {} provider endpoint is invalid: {error}",
                        binding.requirement_name
                    ),
                )
            })?;
            let identity = parse_endpoint_id(&binding.provider_endpoint).map_err(|error| {
                StoreError::new(
                    422,
                    "STORE_BINDING_PROVIDER_ENDPOINT_INVALID",
                    format!(
                        "API binding {} provider endpoint is invalid: {error}",
                        binding.requirement_name
                    ),
                )
            })?;
            let provider_host = identity.host.parse::<IpAddr>().map_err(|_| {
                StoreError::new(
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

pub(crate) fn build_resource_claim_steps(
    contract: &ServiceReleaseContract,
    install: &RuntimeInstallPayload,
    node: &NodeRecord,
) -> Result<Vec<ResourceClaimStepV1>, StoreError> {
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
                return Err(StoreError::new(
                    422,
                    "STORE_RESOURCE_TYPE_UNSUPPORTED",
                    format!(
                        "resource {} declares unsupported type {}; v1 implements postgresql.database/v1",
                        resource.name, resource.resource_type
                    ),
                ));
            }
            if !resource.lifecycle.eq_ignore_ascii_case("retain") {
                return Err(StoreError::new(
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
                StoreError::new(
                    422,
                    "STORE_RESOURCE_CLAIM_INVALID",
                    format!("resource {} could not be materialized: {error}", resource.name),
                )
            })?;
            Ok(step)
        })
        .collect::<Result<Vec<_>, StoreError>>()?;
    claims.sort_by(|left, right| left.resource_name.cmp(&right.resource_name));
    Ok(claims)
}

pub(crate) fn migration_resource_claims(
    migration: &orchestrator_core::ReleaseMigrationDecl,
    claims: &[ResourceClaimStepV1],
) -> Result<Vec<String>, StoreError> {
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
                StoreError::new(
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

pub(crate) fn stable_resource_claim_id(service_id: &str, resource_name: &str) -> String {
    let owner_instance_id = stable_service_instance_id(service_id);
    let digest = Sha256::digest(format!("{owner_instance_id}\0{resource_name}").as_bytes());
    format!("claim-{digest:x}")
}

/// Store v1 has one installation slot for each service in the default scope.

pub(crate) fn resource_output_environment(resource_name: &str) -> String {
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

pub(crate) fn build_runtime_materialization(
    release: &ServiceReleaseManifest,
    node: &NodeRecord,
    requested_config: &Value,
    requested_secret_refs: &BTreeMap<String, String>,
) -> Result<Option<RuntimeMaterializationStep>, StoreError> {
    let ValidatedReleaseConfig {
        values: config,
        secret_paths: schema_secrets,
        schema_controls_requiredness,
    } = validate_release_config(
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
        return Err(StoreError::new(
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
            return Err(StoreError::new(
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
                return Err(StoreError::new(
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
                return Err(StoreError::new(
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

pub(crate) fn environment_token(value: &str) -> String {
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

pub(crate) fn provider_token(value: &str) -> String {
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
