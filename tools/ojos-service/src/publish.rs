use crate::{
    ServiceContractV3, contract_bytes,
    seal::{
        ReleaseLockV1, event_schema_slot, frontend_manifest_slot, release_lock_bytes,
        release_lock_digest, verify_release_lock,
    },
};
use anyhow::{Context, Result, bail, ensure};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use ed25519_dalek::{Signer, SigningKey};
use orchestrator_core::{
    ContributionApiSurfaceV1, ContributionAudienceV1, ContributionFrontendModuleV1,
    ContributionHttpMethodV1, ContributionOperationRouteV1, ContributionPathPermissionScopeV1,
    ContributionPermissionDefinitionV1, ContributionPermissionScopeV1, ContributionRouteAuthV1,
    RELEASE_PLATFORM_SCHEMA_VERSION, ReleaseArtifactSubjectV1, ReleaseConfigSchemaV1,
    ReleaseContributionSpecV1, ReleasePackageRequirementV1, ReleasePlatformContractV1,
    ReleaseResourceClaimV1, ReleaseRuntimeVolumeV1, SERVICE_CONTRACT_VERSION,
    STANDARD_CONTAINER_RUNTIME_SHA256, ServiceReleaseContract,
};
use orchestrator_manager::catalog_v2::{
    CatalogModuleV2, CatalogReleaseV2, CatalogTrustStore, CatalogV2, Ed25519Signature,
    MetadataPackageV2, OciImageReference, ReleaseChannel, TargetPlatform,
};
use semver::Version;
use serde::Serialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

const JUDGE_SANDBOX_V1_SHA256: &str =
    "sha256:a6b35a495f88bd8e723e395d748de40fbb4dcc08619d02cf92fa580fef2a18ec";

#[derive(Debug, Clone)]
pub struct CatalogPublishOptions {
    pub output_directory: PathBuf,
    pub signing_key_file: PathBuf,
    pub public_base_url: String,
    pub key_id: String,
    pub catalog_id: String,
    pub min_orchestrator_version: Version,
    pub target_os: String,
    pub target_arch: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CatalogPublishReport {
    pub service_id: String,
    pub service_version: Version,
    pub release_lock_digest: String,
    pub catalog: PathBuf,
    pub metadata: PathBuf,
    pub release_lock: PathBuf,
    pub service_contract: PathBuf,
    pub trust: PathBuf,
}

pub fn publish_catalog_v2(
    contract: &ServiceContractV3,
    lock: &ReleaseLockV1,
    source_manifest: &Path,
    options: &CatalogPublishOptions,
) -> Result<CatalogPublishReport> {
    verify_release_lock(contract, lock).context("verify sealed Release lock")?;
    ensure!(
        !options.output_directory.exists(),
        "publish output {} already exists; signed Catalog publication never overwrites",
        options.output_directory.display()
    );
    let public_base_url = options.public_base_url.trim_end_matches('/');
    ensure!(
        public_base_url.starts_with("https://")
            && !public_base_url.chars().any(char::is_whitespace),
        "public-base-url must be HTTPS without whitespace"
    );
    ensure!(
        contract
            .package_requirements
            .iter()
            .all(|requirement| requirement.development),
        "single-service publish cannot omit production package dependencies; publish an aggregate Catalog"
    );

    let runtime_artifact = lock
        .artifacts
        .get(&contract.runtime.artifact)
        .context("release lock omits runtime artifact")?;
    let runtime_reference = required_reference(&contract.runtime.artifact, runtime_artifact)?;
    let runtime_image: OciImageReference = runtime_reference
        .parse()
        .context("runtime artifact reference must be repository@sha256:<digest>")?;
    ensure!(
        runtime_image.digest().as_str() == runtime_artifact.digest,
        "runtime OCI reference digest does not match sealed runtime artifact"
    );
    validate_all_artifact_references(contract, lock)?;

    let metadata_name = format!(
        "{}-{}.release.json",
        contract.service_id, contract.service_version
    );
    let metadata_relative = format!("metadata/{metadata_name}");
    let metadata_url = format!("{public_base_url}/{metadata_relative}");
    let release_document = release_projection(
        contract,
        lock,
        source_manifest,
        &metadata_url,
        runtime_reference,
    )?;
    let metadata_bytes = pretty_json(&release_document)?;
    let metadata_sha256 = digest(&metadata_bytes);
    let canonical_contract = contract_bytes(contract)?;
    let contract_subject = lock
        .artifacts
        .get(crate::seal::CONTRACT_SLOT)
        .context("release lock omits canonical contract slot")?;
    ensure!(
        digest(&canonical_contract) == contract_subject.digest
            && canonical_contract.len() as u64 == contract_subject.size
            && lock.contract_digest == contract_subject.digest,
        "canonical contract bytes do not match release lock contract subject"
    );

    let signing_key = load_signing_key(&options.signing_key_file)?;
    let mut catalog = CatalogV2 {
        schema_version: 2,
        id: options.catalog_id.clone(),
        name: format!("OJOS {} signed Service Contract v3", contract.service_id),
        modules: vec![CatalogModuleV2 {
            id: contract.service_id.clone(),
            name: contract.display_name.clone(),
            description: format!("{} service", contract.display_name),
            kind: "backend-api".to_string(),
            tags: vec![
                "catalog-v2".to_string(),
                "service-contract-v3".to_string(),
                "artifact-graph-sealed".to_string(),
            ],
            releases: vec![CatalogReleaseV2 {
                version: contract.service_version.clone(),
                channel: ReleaseChannel::Stable,
                platforms: vec![TargetPlatform::new(
                    options.target_os.clone(),
                    options.target_arch.clone(),
                )],
                min_orchestrator_version: options.min_orchestrator_version.clone(),
                dependencies: Vec::new(),
                runtime_capabilities: Vec::new(),
                metadata: MetadataPackageV2 {
                    url: metadata_url,
                    sha256: metadata_sha256.parse()?,
                },
                oci_image: runtime_image,
            }],
        }],
        signatures: Vec::new(),
    };
    let signature = signing_key.sign(&catalog.signing_payload_jcs()?);
    catalog.signatures.push(Ed25519Signature {
        key_id: options.key_id.clone(),
        algorithm: "Ed25519".to_string(),
        signature: STANDARD.encode(signature.to_bytes()),
    });
    let public_key = STANDARD.encode(signing_key.verifying_key().to_bytes());
    let mut trust = CatalogTrustStore::new();
    trust.insert_base64(options.key_id.clone(), &public_key)?;
    catalog
        .validate_trusted(&trust)
        .context("self-verify signed Catalog v2")?;

    let staging = staging_path(&options.output_directory)?;
    fs::create_dir(&staging).context("create Catalog staging directory")?;
    fs::create_dir(staging.join("metadata")).context("create metadata directory")?;
    write_new(&staging.join(&metadata_relative), &metadata_bytes)?;
    let lock_name = format!(
        "{}-{}.release.lock.json",
        contract.service_id, contract.service_version
    );
    let contract_name = format!(
        "{}-{}.service.contract.json",
        contract.service_id, contract.service_version
    );
    write_new(
        &staging.join("metadata").join(&lock_name),
        &release_lock_bytes(lock)?,
    )?;
    write_new(
        &staging.join("metadata").join(&contract_name),
        &canonical_contract,
    )?;
    write_new(&staging.join("catalog.json"), &pretty_json(&catalog)?)?;
    write_new(
        &staging.join("trust.json"),
        &pretty_json(&json!({options.key_id.clone(): public_key}))?,
    )?;
    write_new(
        &staging.join("catalog-source.json"),
        &pretty_json(&json!([{
            "id": options.catalog_id,
            "url": format!("{public_base_url}/catalog.json"),
            "required_key_id": options.key_id,
            "auth_secret_ref": "",
            "enabled": true,
            "offline_oci_layouts": {}
        }]))?,
    )?;
    fs::rename(&staging, &options.output_directory)
        .context("atomically publish signed Catalog directory")?;

    Ok(CatalogPublishReport {
        service_id: contract.service_id.clone(),
        service_version: contract.service_version.clone(),
        release_lock_digest: release_lock_digest(lock)?,
        catalog: options.output_directory.join("catalog.json"),
        metadata: options.output_directory.join(metadata_relative),
        release_lock: options.output_directory.join("metadata").join(lock_name),
        service_contract: options
            .output_directory
            .join("metadata")
            .join(contract_name),
        trust: options.output_directory.join("trust.json"),
    })
}

fn release_projection(
    contract: &ServiceContractV3,
    lock: &ReleaseLockV1,
    source_manifest: &Path,
    metadata_url: &str,
    runtime_reference: &str,
) -> Result<Value> {
    let platform = platform_projection(contract, lock, source_manifest)?;
    let mut permissions = contract
        .permissions
        .iter()
        .map(|permission| permission.key.clone())
        .collect::<Vec<_>>();
    permissions.extend(contract.permission_references.iter().cloned());
    permissions.sort();
    permissions.dedup();
    let apis = contract
        .api_surfaces
        .iter()
        .map(|surface| legacy_api_projection(contract, surface))
        .collect::<Result<Vec<_>>>()?;
    let required_apis = contract
        .api_requirements
        .iter()
        .map(|requirement| {
            json!({
                "name": requirement.id,
                "id": requirement.id,
                "version": requirement.version,
                "optional": requirement.optional,
                "selection": if requirement.selection == "explicit" { "explicit" } else { "nearest-healthy" },
                "timeout_ms": 30_000
            })
        })
        .collect::<Vec<_>>();
    let publishes = contract
        .events
        .publishes
        .iter()
        .map(|event| {
            let slot = event_schema_slot(&event.event_type, event.version);
            json!({
                "id": event.event_type,
                "version": format!("{}.0.0", event.version),
                "schema_ref": required_reference(&slot, &lock.artifacts[&slot]).unwrap()
            })
        })
        .collect::<Vec<_>>();
    let subscribes = contract
        .events
        .subscribes
        .iter()
        .map(|event| {
            json!({
                "type": event.event_type,
                "version": format!("{}.0.0", event.version),
                "optional": false,
                "consumer_group": format!(
                    "{}-{}-v{}",
                    contract.service_id,
                    stable_token(&event.event_type),
                    event.version
                )
            })
        })
        .collect::<Vec<_>>();
    let migrations = contract
        .migrations
        .iter()
        .map(|migration| {
            let artifact = lock.artifacts.get(&migration.artifact).with_context(|| {
                format!("migration {} artifact is missing", migration.id)
            })?;
            let image = required_reference(&migration.artifact, artifact)?;
            let parsed: OciImageReference = image.parse().with_context(|| {
                format!("migration {} must resolve to an OCI reference", migration.id)
            })?;
            ensure!(
                parsed.digest().as_str() == artifact.digest,
                "migration {} reference digest differs from sealed artifact",
                migration.id
            );
            Ok(json!({
                "version": migration.id,
                "path": format!("services/{}/migrations/{}.up.sql", contract.service_id, migration.id),
                "checksum": artifact.digest,
                "destructive": false,
                "oci": {
                    "image": image,
                    "command": ["/ojos-migrate"],
                    "env": {"OJOS_RESOURCE_CLAIM": migration.resource},
                    "timeout_ms": 300_000
                }
            }))
        })
        .collect::<Result<Vec<_>>>()?;
    let config_schema = platform
        .config_schema
        .as_ref()
        .map(|config| config.schema.clone())
        .unwrap_or_else(|| json!({}));
    let secrets = secret_property_names(&config_schema);
    let runtime_profile_sha = match contract.runtime.profile.as_str() {
        "standard-container-v1" => STANDARD_CONTAINER_RUNTIME_SHA256,
        "judge-sandbox-v1" => JUDGE_SANDBOX_V1_SHA256,
        other => bail!("unsupported runtime profile {other}"),
    };
    let document = json!({
        "schema_version": SERVICE_CONTRACT_VERSION,
        "service_name": contract.service_id,
        "version": contract.service_version,
        "description": format!("{} compiled Service Contract v3", contract.display_name),
        "service_type": "backend-api",
        "source": {"kind": "url", "url": metadata_url, "checksum": ""},
        "runtime": {
            "kind": "image", "image": runtime_reference, "binary": "", "system_service": "",
            "env": {
                "OJOS_SERVICE_CONTEXT_FILE": "/run/ojos/service/context.json",
                "OJOS_EVENT_CONTEXT_FILE": "/run/ojos/service/events.json"
            }
        },
        "frontend": {"enabled": false, "route_prefix": "", "remote_entry": "", "menu_items": []},
        "backend": {"protocol": "http", "port": contract.runtime.http_port, "health_path": contract.runtime.health.path},
        "migrations": migrations,
        "permissions": permissions,
        "routes": [],
        "provides": {
            "apis": apis,
            "events": publishes
        },
        "requires": {
            "apis": required_apis,
            "events": subscribes
        },
        "events": {"publishes": publishes, "subscribes": subscribes},
        "runtime_contract": {
            "id": contract.runtime.profile,
            "sha256": runtime_profile_sha,
            "binding_directory": "/run/ojos/service",
            "identity_mode": "workload",
            "credential_delivery": "file",
            "restart_on_change": false,
            "env_projection": {}
        },
        "redis": [],
        "storage": [],
        "dependencies": [],
        "service_identity": {"service_name": contract.service_id, "allowed_apis": []},
        "config_schema": config_schema,
        "secrets": secrets,
        "observability": {"metrics": true, "jaeger": true},
        "platform": platform
    });
    let parsed = ServiceReleaseContract::from_json_value(document)
        .context("self-validate generated Catalog Release metadata")?;
    ensure!(
        parsed.contract_version == SERVICE_CONTRACT_VERSION,
        "generated metadata downgraded its Service Contract version"
    );
    parsed.to_json_value().map_err(anyhow::Error::from)
}

fn platform_projection(
    contract: &ServiceContractV3,
    lock: &ReleaseLockV1,
    source_manifest: &Path,
) -> Result<ReleasePlatformContractV1> {
    let mut roles_by_slot = BTreeMap::<String, Vec<String>>::new();
    for binding in &lock.bindings {
        roles_by_slot
            .entry(binding.slot.clone())
            .or_default()
            .push(binding.role.clone());
    }
    let mut artifact_subjects = Vec::with_capacity(lock.artifacts.len());
    for (slot, artifact) in &lock.artifacts {
        let mut roles = roles_by_slot.remove(slot).unwrap_or_default();
        roles.sort();
        roles.dedup();
        artifact_subjects.push(ReleaseArtifactSubjectV1 {
            slot: slot.clone(),
            roles,
            media_type: artifact.media_type.clone(),
            digest: artifact.digest.clone(),
            size: artifact.size,
            reference: artifact.reference.clone(),
        });
    }
    artifact_subjects.sort();

    let config_schema = contract
        .config_schema
        .as_ref()
        .map(|artifact| {
            let root = source_manifest.parent().unwrap_or_else(|| Path::new("."));
            let bytes = fs::read(root.join(&artifact.path))
                .with_context(|| format!("read config schema {}", artifact.path))?;
            let schema: Value = serde_json::from_slice(&bytes)
                .with_context(|| format!("parse config schema {}", artifact.path))?;
            let actual = digest(&serde_json_canonicalizer::to_vec(&schema)?);
            ensure!(
                actual == artifact.digest,
                "config schema digest changed after compile"
            );
            Ok(ReleaseConfigSchemaV1 {
                digest: artifact.digest.clone(),
                schema,
            })
        })
        .transpose()?;

    Ok(ReleasePlatformContractV1 {
        schema_version: RELEASE_PLATFORM_SCHEMA_VERSION.to_string(),
        contract_digest: digest(&contract_bytes(contract)?),
        source_digest: contract.source_digest.clone(),
        release_lock_digest: release_lock_digest(lock)?,
        artifact_subjects,
        package_requirements: contract
            .package_requirements
            .iter()
            .map(|requirement| ReleasePackageRequirementV1 {
                service_id: requirement.id.clone(),
                version_requirement: requirement.version.clone(),
                development: requirement.development,
            })
            .collect(),
        resource_claims: contract
            .resource_claims
            .iter()
            .map(|claim| ReleaseResourceClaimV1 {
                name: claim.name.clone(),
                resource_type: claim.resource_type.clone(),
                lifecycle: claim.lifecycle.clone(),
            })
            .collect(),
        runtime_volumes: contract
            .runtime
            .volumes
            .iter()
            .map(|volume| ReleaseRuntimeVolumeV1 {
                name: volume.name.clone(),
                kind: volume.kind.clone(),
                target: volume.target.clone(),
                access: volume.access.clone(),
                lifecycle: volume.lifecycle.clone(),
            })
            .collect(),
        config_schema,
        contribution: contribution_projection(contract, lock)?,
    })
}

fn contribution_projection(
    contract: &ServiceContractV3,
    lock: &ReleaseLockV1,
) -> Result<ReleaseContributionSpecV1> {
    let api_surfaces = contract
        .api_surfaces
        .iter()
        .map(|surface| ContributionApiSurfaceV1 {
            api_id: surface.api_id.clone(),
            api_version: surface.version.to_string(),
            protocol: "http".to_string(),
            base_path: "/".to_string(),
        })
        .collect();
    let operation_routes = contract
        .routes
        .iter()
        .map(|route| {
            Ok(ContributionOperationRouteV1 {
                audience: match route.audience.as_str() {
                    "user" => ContributionAudienceV1::User,
                    "public" => ContributionAudienceV1::Public,
                    "admin" => ContributionAudienceV1::Admin,
                    other => bail!("unsupported contribution audience {other}"),
                },
                method: contribution_method(&route.method)?,
                path: route.path.clone(),
                api_id: route.api_id.clone(),
                operation_id: route.operation_id.clone(),
                provider_path: route.provider_path.clone(),
                auth: match route.auth.as_str() {
                    "anonymous" => ContributionRouteAuthV1::Anonymous,
                    "optional" => ContributionRouteAuthV1::Optional,
                    "required" => ContributionRouteAuthV1::Required,
                    other => bail!("unsupported contribution auth mode {other}"),
                },
                permission: route.permission.clone(),
                permission_scope: route.permission_scope.as_ref().map(|scope| match scope {
                    crate::PermissionScopeV1::System(_) => ContributionPermissionScopeV1::system(),
                    crate::PermissionScopeV1::PathParameter(scope) => {
                        ContributionPermissionScopeV1::PathParameter(
                            ContributionPathPermissionScopeV1 {
                                scope_type: scope.scope_type.clone(),
                                path_parameter: scope.path_parameter.clone(),
                            },
                        )
                    }
                }),
            })
        })
        .collect::<Result<Vec<_>>>()?;
    let permission_definitions = contract
        .permissions
        .iter()
        .map(|permission| ContributionPermissionDefinitionV1 {
            key: permission.key.clone(),
            title: permission.title.clone(),
            description: String::new(),
        })
        .collect();
    let mut user_frontend_modules = Vec::new();
    let mut admin_frontend_modules = Vec::new();
    for frontend in &contract.frontends {
        let manifest_slot = frontend_manifest_slot(&frontend.module.module_id);
        let manifest_artifact = lock
            .artifacts
            .get(&manifest_slot)
            .with_context(|| format!("missing frontend manifest slot {manifest_slot}"))?;
        let manifest_digest = manifest_artifact.digest.clone();
        let manifest_reference = required_reference(&manifest_slot, manifest_artifact)?;
        let bundle_artifact = lock
            .artifacts
            .get(&frontend.module.artifact)
            .with_context(|| {
                format!("missing frontend bundle slot {}", frontend.module.artifact)
            })?;
        let bundle_digest = bundle_artifact.digest.clone();
        let bundle_reference = required_reference(&frontend.module.artifact, bundle_artifact)?;
        for route in &frontend.module.routes {
            let module = ContributionFrontendModuleV1 {
                // A logical module is activated once; each item represents one
                // independently routable/mountable surface of that module.
                module_id: frontend.module.module_id.clone(),
                surface_id: route.id.clone(),
                route: route.path.clone(),
                menu_label: route.title.clone(),
                menu: route.menu,
                order: route.order,
                permission: route.permission.clone(),
                artifact: frontend.module.artifact.clone(),
                host_api_range: frontend.module.host_api_range.clone(),
                manifest_digest: manifest_digest.clone(),
                manifest_reference: manifest_reference.to_string(),
                bundle_digest: bundle_digest.clone(),
                bundle_reference: bundle_reference.to_string(),
            };
            match frontend.target.as_str() {
                "user-shell" => user_frontend_modules.push(module),
                "admin-shell" => admin_frontend_modules.push(module),
                other => bail!("unsupported frontend target {other}"),
            }
        }
    }
    Ok(ReleaseContributionSpecV1 {
        api_surfaces,
        operation_routes,
        permission_definitions,
        user_frontend_modules,
        admin_frontend_modules,
    })
}

fn legacy_api_projection(
    contract: &ServiceContractV3,
    surface: &crate::ApiSurfaceV3,
) -> Result<Value> {
    let operations = contract
        .operations
        .iter()
        .filter(|operation| operation.api_id == surface.api_id)
        .collect::<Vec<_>>();
    ensure!(
        !operations.is_empty(),
        "API {} has no operations",
        surface.api_id
    );
    let methods = operations
        .iter()
        .map(|operation| operation.method.clone())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    let permission = operations
        .iter()
        .find_map(|operation| operation.permission.clone())
        .unwrap_or_else(|| "public".to_string());
    let auth = if operations
        .iter()
        .all(|operation| operation.auth == "anonymous")
    {
        "public"
    } else if operations
        .iter()
        .any(|operation| operation.audience == "internal")
    {
        "workload"
    } else {
        "user"
    };
    Ok(json!({
        "id": surface.api_id,
        "version": surface.version,
        "protocol": "http",
        "port_name": "http",
        "path": "/",
        "methods": methods,
        "visibility": "explicit",
        "auth": auth,
        "permission": permission,
        "stability": "stable",
        "timeout_ms": 30_000
    }))
}

fn contribution_method(value: &str) -> Result<ContributionHttpMethodV1> {
    Ok(match value {
        "GET" => ContributionHttpMethodV1::Get,
        "POST" => ContributionHttpMethodV1::Post,
        "PUT" => ContributionHttpMethodV1::Put,
        "PATCH" => ContributionHttpMethodV1::Patch,
        "DELETE" => ContributionHttpMethodV1::Delete,
        "HEAD" => ContributionHttpMethodV1::Head,
        other => bail!("unsupported contribution HTTP method {other}"),
    })
}

fn validate_all_artifact_references(
    contract: &ServiceContractV3,
    lock: &ReleaseLockV1,
) -> Result<()> {
    let migration_slots = contract
        .migrations
        .iter()
        .map(|migration| migration.artifact.as_str())
        .collect::<BTreeSet<_>>();
    for (slot, artifact) in &lock.artifacts {
        let reference = required_reference(slot, artifact)?;
        if slot == &contract.runtime.artifact || migration_slots.contains(slot.as_str()) {
            let image: OciImageReference = reference
                .parse()
                .with_context(|| format!("artifact {slot} must use an immutable OCI reference"))?;
            ensure!(
                image.digest().as_str() == artifact.digest,
                "artifact {slot} OCI reference digest does not match its sealed digest"
            );
        } else {
            ensure!(
                reference.starts_with("https://")
                    && reference.contains(artifact.digest.trim_start_matches("sha256:")),
                "artifact {slot} must use an HTTPS content-addressed reference containing its digest"
            );
        }
    }
    Ok(())
}

fn required_reference<'a>(
    slot: &str,
    artifact: &'a crate::seal::ResolvedArtifactV1,
) -> Result<&'a str> {
    artifact
        .reference
        .as_deref()
        .with_context(|| format!("artifact {slot} has no immutable publication reference"))
}

fn secret_property_names(schema: &Value) -> Vec<String> {
    fn visit(value: &Value, names: &mut BTreeSet<String>) {
        if let Some(object) = value.as_object() {
            if let Some(properties) = object.get("properties").and_then(Value::as_object) {
                for (name, property) in properties {
                    if property.get("writeOnly").and_then(Value::as_bool) == Some(true)
                        && property.get("x-ojos-secret").and_then(Value::as_bool) == Some(true)
                    {
                        // `release.secrets` is a legacy compatibility field
                        // whose identifier grammar cannot represent ordinary
                        // JSON property names such as `inviteSigningKey`.
                        // Never invent an alias without a signed reverse
                        // mapping: the authoritative v3 config schema retains
                        // every name and Store derives secret requirements from
                        // `writeOnly + x-ojos-secret` directly.
                        if name.bytes().all(|byte| {
                            byte.is_ascii_lowercase()
                                || byte.is_ascii_digit()
                                || matches!(byte, b'.' | b'_' | b':' | b'-')
                        }) && name
                            .as_bytes()
                            .first()
                            .is_some_and(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
                        {
                            names.insert(name.clone());
                        }
                    }
                    visit(property, names);
                }
            }
            for (key, child) in object {
                if key != "properties" {
                    visit(child, names);
                }
            }
        } else if let Some(items) = value.as_array() {
            for child in items {
                visit(child, names);
            }
        }
    }
    let mut names = BTreeSet::new();
    visit(schema, &mut names);
    names.into_iter().collect()
}

fn stable_token(value: &str) -> String {
    value
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() {
                character.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect::<String>()
        .trim_matches('-')
        .to_string()
}

fn load_signing_key(path: &Path) -> Result<SigningKey> {
    let encoded = fs::read_to_string(path).context("read Ed25519 signing key file")?;
    let encoded = encoded.trim();
    let bytes = STANDARD
        .decode(encoded)
        .context("signing key must be canonical padded base64")?;
    ensure!(
        STANDARD.encode(&bytes) == encoded,
        "signing key must use canonical padded base64"
    );
    let seed: [u8; 32] = bytes.try_into().map_err(|bytes: Vec<u8>| {
        anyhow::anyhow!("signing key is {} bytes, expected 32", bytes.len())
    })?;
    Ok(SigningKey::from_bytes(&seed))
}

fn digest(bytes: &[u8]) -> String {
    format!("sha256:{:x}", Sha256::digest(bytes))
}

fn pretty_json(value: &impl Serialize) -> Result<Vec<u8>> {
    let mut bytes = serde_json::to_vec_pretty(value)?;
    bytes.push(b'\n');
    Ok(bytes)
}

fn staging_path(output: &Path) -> Result<PathBuf> {
    let parent = output
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent).context("create Catalog output parent")?;
    let name = output
        .file_name()
        .and_then(|value| value.to_str())
        .context("Catalog output directory requires a UTF-8 name")?;
    Ok(parent.join(format!(
        ".{name}.pending-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    )))
}

fn write_new(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .with_context(|| format!("create {}", path.display()))?;
    file.write_all(bytes)
        .with_context(|| format!("write {}", path.display()))?;
    file.sync_all()
        .with_context(|| format!("sync {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        ApiOperationV3, ApiSurfaceV3, EventsContractV1, FrontendContractV1, FrontendManifestV1,
        FrontendRouteV1, HealthSource, MigrationSource, PermissionSource, ResourceSource,
        RuntimeSource, seal::ResolvedArtifactV1,
    };

    fn sha(character: char) -> String {
        format!("sha256:{}", character.to_string().repeat(64))
    }

    fn contract() -> ServiceContractV3 {
        ServiceContractV3 {
            schema_version: crate::SERVICE_CONTRACT_SCHEMA_VERSION.to_string(),
            compiler_version: crate::COMPILER_VERSION.to_string(),
            service_id: "contest-service".to_string(),
            service_version: Version::parse("1.0.0").unwrap(),
            display_name: "Contest".to_string(),
            source_digest: sha('1'),
            runtime: RuntimeSource {
                profile: "standard-container-v1".to_string(),
                artifact: "runtime".to_string(),
                http_port: 8080,
                health: HealthSource {
                    path: "/healthz".to_string(),
                },
                volumes: Vec::new(),
            },
            api_surfaces: vec![ApiSurfaceV3 {
                api_id: "contest.api".to_string(),
                version: Version::parse("1.0.0").unwrap(),
                document: "api/openapi.yaml".to_string(),
                document_digest: sha('2'),
            }],
            operations: vec![ApiOperationV3 {
                api_id: "contest.api".to_string(),
                api_version: Version::parse("1.0.0").unwrap(),
                operation_id: "listContests".to_string(),
                provider_path: "/contests".to_string(),
                method: "GET".to_string(),
                audience: "user".to_string(),
                auth: "required".to_string(),
                permission: Some("contest-service.view".to_string()),
                permission_scope: Some(crate::PermissionScopeV1::system()),
                parameters: Vec::new(),
                request_body: None,
                responses: Vec::new(),
            }],
            api_requirements: Vec::new(),
            package_requirements: Vec::new(),
            resource_claims: vec![ResourceSource {
                name: "database".to_string(),
                resource_type: "postgresql.database/v1".to_string(),
                lifecycle: "retain".to_string(),
            }],
            migrations: vec![MigrationSource {
                id: "schema-v1".to_string(),
                artifact: "migration".to_string(),
                resource: "database".to_string(),
            }],
            events: EventsContractV1::default(),
            permissions: vec![PermissionSource {
                key: "contest-service.view".to_string(),
                title: "View contests".to_string(),
            }],
            permission_references: Vec::new(),
            exposures: Vec::new(),
            routes: vec![crate::RouteContributionV1 {
                exposure_id: "user".to_string(),
                audience: "user".to_string(),
                method: "GET".to_string(),
                path: "/api/contests".to_string(),
                api_id: "contest.api".to_string(),
                operation_id: "listContests".to_string(),
                provider_path: "/contests".to_string(),
                auth: "required".to_string(),
                permission: Some("contest-service.view".to_string()),
                permission_scope: Some(crate::PermissionScopeV1::system()),
            }],
            frontends: vec![FrontendContractV1 {
                target: "user-shell".to_string(),
                manifest: crate::ArtifactFileV1 {
                    path: "frontend/manifest.json".to_string(),
                    digest: sha('3'),
                },
                module: FrontendManifestV1 {
                    schema_version: "ojos.frontend/v1".to_string(),
                    module_id: "contest.user".to_string(),
                    target: "user-shell".to_string(),
                    artifact: "frontend".to_string(),
                    host_api_range: "^1".to_string(),
                    routes: vec![FrontendRouteV1 {
                        id: "list".to_string(),
                        path: "/contests".to_string(),
                        title: "Contests".to_string(),
                        menu: true,
                        order: 1,
                        permission: Some("contest-service.view".to_string()),
                    }],
                },
            }],
            config_schema: None,
        }
    }

    fn lock(contract: &ServiceContractV3) -> ReleaseLockV1 {
        let contract_digest = digest(&contract_bytes(contract).unwrap());
        let contract_size = contract_bytes(contract).unwrap().len() as u64;
        let mut artifacts = BTreeMap::from([
            (
                "contract".to_string(),
                ResolvedArtifactV1 {
                    media_type: "application/json".to_string(),
                    digest: contract_digest.clone(),
                    size: contract_size,
                    reference: Some(format!(
                        "https://artifacts.example/sha256/{}/contract.json",
                        contract_digest.trim_start_matches("sha256:")
                    )),
                },
            ),
            (
                "runtime".to_string(),
                ResolvedArtifactV1 {
                    media_type: "application/vnd.oci.image.manifest.v1+json".to_string(),
                    digest: sha('5'),
                    size: 50,
                    reference: Some(format!("registry.example/contest@{}", sha('5'))),
                },
            ),
            (
                "migration".to_string(),
                ResolvedArtifactV1 {
                    media_type: "application/vnd.oci.image.manifest.v1+json".to_string(),
                    digest: sha('6'),
                    size: 60,
                    reference: Some(format!("registry.example/contest-migrate@{}", sha('6'))),
                },
            ),
            ("sbom".to_string(), https_artifact(sha('7'))),
            ("provenance".to_string(), https_artifact(sha('8'))),
            ("events".to_string(), https_artifact(sha('9'))),
            ("openapi.contest.api".to_string(), https_artifact(sha('2'))),
            (
                "frontend.contest.user.manifest".to_string(),
                https_artifact(sha('3')),
            ),
            ("frontend".to_string(), https_artifact(sha('a'))),
        ]);
        for requirement in crate::seal::artifact_requirements(contract).unwrap() {
            let artifact = artifacts
                .get_mut(&requirement.slot)
                .unwrap_or_else(|| panic!("test fixture omits slot {}", requirement.slot));
            if let Some(expected_digest) = requirement.expected_digest {
                artifact.digest = expected_digest.clone();
                if artifact.reference.as_deref().is_some_and(|reference| {
                    reference.starts_with("https://artifacts.example/sha256/")
                }) {
                    artifact.reference = Some(format!(
                        "https://artifacts.example/sha256/{}/subject",
                        expected_digest.trim_start_matches("sha256:")
                    ));
                }
            }
            if let Some(expected_size) = requirement.expected_size {
                artifact.size = expected_size;
            }
        }
        crate::seal::seal(
            contract,
            &crate::seal::ResolvedArtifactsV1 {
                schema_version: crate::seal::RESOLVED_ARTIFACTS_SCHEMA_VERSION.to_string(),
                artifacts,
            },
        )
        .unwrap()
    }

    fn https_artifact(digest: String) -> ResolvedArtifactV1 {
        ResolvedArtifactV1 {
            media_type: "application/octet-stream".to_string(),
            size: 10,
            reference: Some(format!(
                "https://artifacts.example/sha256/{}/subject",
                digest.trim_start_matches("sha256:")
            )),
            digest,
        }
    }

    #[test]
    fn publication_is_signed_self_verified_and_carries_platform_projection() {
        let temp = tempfile::tempdir().unwrap();
        let key = temp.path().join("key");
        fs::write(&key, STANDARD.encode([7_u8; 32])).unwrap();
        let output = temp.path().join("catalog");
        let contract = contract();
        let lock = lock(&contract);
        verify_release_lock(&contract, &lock).unwrap();
        let report = publish_catalog_v2(
            &contract,
            &lock,
            temp.path().join("ojos.service.yaml").as_path(),
            &CatalogPublishOptions {
                output_directory: output.clone(),
                signing_key_file: key,
                public_base_url: "https://catalog.example/contest".to_string(),
                key_id: "contest-test-key".to_string(),
                catalog_id: "contest-test".to_string(),
                min_orchestrator_version: Version::parse("1.0.0").unwrap(),
                target_os: "linux".to_string(),
                target_arch: "x86_64".to_string(),
            },
        )
        .unwrap();
        assert!(report.catalog.is_file());
        assert!(report.service_contract.is_file());
        let canonical_contract = fs::read(&report.service_contract).unwrap();
        let contract_subject = &lock.artifacts[crate::seal::CONTRACT_SLOT];
        assert_eq!(digest(&canonical_contract), contract_subject.digest);
        assert_eq!(canonical_contract.len() as u64, contract_subject.size);
        let metadata: Value = serde_json::from_slice(&fs::read(report.metadata).unwrap()).unwrap();
        assert_eq!(metadata["schema_version"], 2);
        assert_eq!(
            metadata["platform"]["schemaVersion"],
            RELEASE_PLATFORM_SCHEMA_VERSION
        );
        assert_eq!(metadata["routes"], json!([]));
        assert_eq!(metadata["frontend"]["enabled"], false);
        assert_eq!(
            metadata["platform"]["resourceClaims"][0]["lifecycle"],
            "retain"
        );
    }

    #[test]
    fn publication_rejects_missing_and_digest_mismatched_references() {
        let contract = contract();
        let mut missing = lock(&contract);
        missing.artifacts.get_mut("runtime").unwrap().reference = None;
        assert!(validate_all_artifact_references(&contract, &missing).is_err());

        let mut mismatched = lock(&contract);
        mismatched.artifacts.get_mut("runtime").unwrap().reference =
            Some(format!("registry.example/contest@{}", sha('f')));
        assert!(validate_all_artifact_references(&contract, &mismatched).is_err());
    }

    #[test]
    fn external_permission_reference_guards_route_without_becoming_owned_definition() {
        let mut contract = contract();
        contract.permission_references = vec!["system.admin".to_string()];
        contract.operations[0].permission = Some("system.admin".to_string());
        contract.routes[0].permission = Some("system.admin".to_string());
        let lock = lock(&contract);
        let runtime_reference = lock.artifacts["runtime"].reference.as_deref().unwrap();
        let metadata = release_projection(
            &contract,
            &lock,
            Path::new("ojos.service.yaml"),
            "https://catalog.example/contest/metadata.json",
            runtime_reference,
        )
        .unwrap();

        let legacy_permissions = metadata["permissions"].as_array().unwrap();
        assert!(legacy_permissions.contains(&json!("contest-service.view")));
        assert!(legacy_permissions.contains(&json!("system.admin")));
        assert_eq!(
            metadata["platform"]["contribution"]["permissionDefinitions"],
            json!([{
                "key": "contest-service.view",
                "title": "View contests",
                "description": ""
            }])
        );
        assert_eq!(
            metadata["platform"]["contribution"]["operationRoutes"][0]["permission"],
            "system.admin"
        );
    }

    #[test]
    fn legacy_secret_projection_never_renames_json_schema_properties() {
        let schema = json!({
            "type": "object",
            "properties": {
                "db_password": {"type": "string", "writeOnly": true, "x-ojos-secret": true},
                "inviteSigningKey": {"type": "string", "writeOnly": true, "x-ojos-secret": true}
            }
        });
        assert_eq!(secret_property_names(&schema), vec!["db_password"]);
        assert!(schema["properties"].get("inviteSigningKey").is_some());
    }
}
