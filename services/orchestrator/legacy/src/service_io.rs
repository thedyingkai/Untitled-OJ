use crate::{
    DeploymentTemplate, DeploymentTemplateService, OrchestratorError, ReleaseEventsContract,
    ReleaseRedisDecl, Result, ServiceManifest, ServiceReleaseContract, ServiceReleaseManifest,
    sanitize_path_for_error, validate_deployment_template, validate_service_manifest,
    validate_service_release,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::path::{Component, Path};

pub(crate) const SERVICE_CONTRACT_V2_EVENT_STREAM: &str = "ojos:events:v1";
const SERVICE_CONTRACT_V2_EVENT_RESOURCE_SCHEMA: &str =
    "ojos.service-contract-v2.event-consumer.v1";

/// Structured metadata carried through the legacy `usage` column while the
/// v0.2 Redis registry is still serving the v1 install pipeline. This keeps
/// the shared event stream and the exact consumer-group identity explicit;
/// the runtime provisioner never has to infer either value from a service
/// name.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct LegacyEventRedisUsage {
    pub schema: String,
    pub stream: String,
    pub consumer_group: String,
    pub events: Vec<String>,
}

pub(crate) fn parse_legacy_event_redis_usage(value: &str) -> Option<LegacyEventRedisUsage> {
    let usage = serde_json::from_str::<LegacyEventRedisUsage>(value).ok()?;
    (usage.schema == SERVICE_CONTRACT_V2_EVENT_RESOURCE_SCHEMA
        && usage.stream == SERVICE_CONTRACT_V2_EVENT_STREAM
        && !usage.consumer_group.trim().is_empty())
    .then_some(usage)
}

/// Convert the formal v2 contract into the normalized v1-shaped payload used
/// by the legacy store. API declarations have already been projected by
/// `ServiceReleaseContract`; event subscriptions additionally become typed
/// consumer-group resources on the single shared event stream. Existing v1
/// `redis` declarations are retained byte-for-byte.
pub(crate) fn legacy_release_manifest_from_contract(
    contract: ServiceReleaseContract,
) -> Result<ServiceReleaseManifest> {
    let mut release = contract.release;
    if contract.contract_version >= 2 {
        project_event_subscriptions(&mut release, &contract.events)?;
    }
    validate_service_release(&release)?;
    Ok(release)
}

/// Read either a legacy release manifest or a full Service Contract v2 from a
/// durable `ServiceRelease.manifest` value and return the v1-shaped projection
/// consumed by the legacy planner/runtime pipeline.  Durable storage keeps the
/// full v2 document; only this compatibility boundary performs the projection.
pub(crate) fn legacy_release_manifest_from_json_value(
    value: Value,
) -> Result<ServiceReleaseManifest> {
    legacy_release_manifest_from_contract(ServiceReleaseContract::from_json_value(value)?)
}

fn project_event_subscriptions(
    release: &mut ServiceReleaseManifest,
    events: &ReleaseEventsContract,
) -> Result<()> {
    let mut events_by_group = BTreeMap::<String, BTreeSet<String>>::new();
    for subscription in &events.subscribes {
        let group = subscription.consumer_group().trim();
        if group.is_empty() {
            return Err(OrchestratorError::InvalidManifest(format!(
                "Service Contract v2 event subscriber {} requires consumer_group",
                subscription.event_id()
            )));
        }
        events_by_group
            .entry(group.to_string())
            .or_default()
            .insert(subscription.event_id().to_string());
    }

    for (consumer_group, event_ids) in events_by_group {
        let digest = Sha256::digest(consumer_group.as_bytes());
        let name = format!("events:{}", hex_lower(&digest));
        let usage = LegacyEventRedisUsage {
            schema: SERVICE_CONTRACT_V2_EVENT_RESOURCE_SCHEMA.to_string(),
            stream: SERVICE_CONTRACT_V2_EVENT_STREAM.to_string(),
            consumer_group,
            events: event_ids.into_iter().collect(),
        };
        let projected = ReleaseRedisDecl {
            name: name.clone(),
            kind: "consumer-group".to_string(),
            usage: serde_json::to_string(&usage)?,
        };
        if let Some(existing) = release.redis.iter().find(|item| item.name == name) {
            if existing != &projected {
                return Err(OrchestratorError::InvalidManifest(format!(
                    "Service Contract v2 event consumer resource {name} conflicts with legacy redis declaration"
                )));
            }
            continue;
        }
        release.redis.push(projected);
    }
    Ok(())
}

fn hex_lower(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

pub fn validate_service_manifest_file(
    repo_root: &Path,
    manifest_path: &Path,
) -> Result<ServiceManifest> {
    validate_service_manifest_path(repo_root, manifest_path)?;
    let text = read_text(repo_root.join(manifest_path), "service manifest")?;
    let manifest: ServiceManifest = serde_yaml::from_str(&text)?;
    validate_service_manifest(&manifest)?;
    Ok(manifest)
}

pub fn validate_service_release_file(
    repo_root: &Path,
    release_path: &Path,
) -> Result<ServiceReleaseManifest> {
    validate_release_path(repo_root, release_path)?;
    let text = read_text(repo_root.join(release_path), "release manifest")?;
    let contract = ServiceReleaseContract::from_yaml_str(&text)?;
    let contract_version = contract.contract_version;
    let release = legacy_release_manifest_from_contract(contract)?;
    let service_path = release_path
        .parent()
        .unwrap_or_else(|| Path::new("services"))
        .join("service.yaml");
    if repo_root.join(&service_path).is_file() {
        let service = validate_service_manifest_file(repo_root, &service_path)?;
        ensure(
            service.id == release.service_name,
            "release service_name must match service.yaml id",
        )?;
        ensure(
            service.version == release.version,
            "release version must match service.yaml version",
        )?;
        ensure(
            service.kind == release.service_type,
            "release service_type must match service.yaml kind",
        )?;
        ensure(
            service.endpoint.protocol == release.backend.protocol,
            "release backend protocol must match service endpoint protocol",
        )?;
        ensure(
            service.endpoint.default_port == release.backend.port,
            "release backend port must match service endpoint default_port",
        )?;
        ensure(
            service.endpoint.health_path == release.backend.health_path,
            "release backend health_path must match service endpoint health_path",
        )?;
        for permission in service
            .permissions
            .iter()
            .chain(service.ui.permissions.iter())
        {
            ensure(
                release.permissions.iter().any(|item| item == permission),
                "release permissions must cover service.yaml permissions",
            )?;
        }
        ensure(
            release.frontend.enabled == service.ui.enabled,
            "release frontend enabled must match service.yaml ui enabled",
        )?;
        if service.ui.enabled {
            for route in &service.ui.routes {
                ensure(
                    release.frontend.route_prefix == *route,
                    "release frontend route_prefix must cover service.yaml ui routes",
                )?;
            }
        }
        for bucket in &service.provides.storage_buckets {
            ensure(
                release.storage.iter().any(|item| item.bucket == *bucket),
                "release storage must cover service.yaml storage_buckets",
            )?;
        }
        // service.yaml remains the development/0.2 descriptor.  A v2 Release
        // replaces its service-name, queue and shared-secret dependencies
        // with named API/event bindings and a fixed runtime contract.  Making
        // the v2 production contract repeat the legacy declarations would
        // reintroduce the global worker token and implicit judge-api coupling
        // that Service Contract v2 deliberately removed.
        if contract_version < 2 {
            for dependency in &service.requires.services {
                ensure(
                    release.dependencies.iter().any(|item| item == dependency),
                    "release dependencies must cover service.yaml requires.services",
                )?;
            }
            for queue in &service.requires.queue {
                ensure(
                    release.redis.iter().any(|item| item.name == *queue),
                    "release redis must cover service.yaml requires.queue",
                )?;
            }
            for secret in service
                .requires
                .secrets
                .iter()
                .chain(service.security.required_secrets.iter())
            {
                ensure(
                    release.secrets.iter().any(|item| item == secret),
                    "release secrets must cover service.yaml secrets",
                )?;
            }
        }
        for route in service
            .endpoint
            .routes
            .iter()
            .chain(service.provides.routes.iter())
        {
            ensure(
                release
                    .routes
                    .iter()
                    .any(|candidate| release_route_covers_service_route(&candidate.path, route)),
                "release routes must cover service.yaml routes",
            )?;
        }
    }
    Ok(release)
}

pub fn validate_deployment_template_file(
    repo_root: &Path,
    set_path: &Path,
) -> Result<DeploymentTemplate> {
    validate_set_path(repo_root, set_path)?;
    let text = read_text(repo_root.join(set_path), "deployment template")?;
    let set: DeploymentTemplate = serde_yaml::from_str(&text)?;
    validate_deployment_template(&set)?;
    validate_deployment_template_references(repo_root, &set)?;
    Ok(set)
}

pub fn validate_deployment_template_references(
    repo_root: &Path,
    set: &DeploymentTemplate,
) -> Result<()> {
    let manifests = discover_service_manifests(repo_root)?;
    let service_ids = manifests
        .iter()
        .map(|service| service.id.as_str())
        .collect::<HashSet<_>>();
    let set_service_ids = set
        .services
        .iter()
        .map(DeploymentTemplateService::id)
        .collect::<HashSet<_>>();
    for service in &set.services {
        ensure(
            service_ids.contains(service.id()),
            &format!("set references missing service {}", service.id()),
        )?;
    }
    for endpoint in &set.default_endpoints {
        ensure(
            set_service_ids.contains(endpoint.service.as_str()),
            "default endpoint service is not in set",
        )?;
        ensure(
            service_ids.contains(endpoint.service.as_str()),
            &format!(
                "default endpoint references missing service {}",
                endpoint.service
            ),
        )?;
    }
    for link in &set.default_links {
        ensure(
            service_ids.contains(link.from.as_str()) && service_ids.contains(link.to.as_str()),
            &format!(
                "default link references missing service {} -> {}",
                link.from, link.to
            ),
        )?;
    }
    for service in &set.services {
        let Some(manifest) = manifests.iter().find(|item| item.id == service.id()) else {
            continue;
        };
        for required in &manifest.requires.links {
            let target = required.id.as_str();
            if set_service_ids.contains(target) {
                ensure(
                    set.default_links.iter().any(|link| {
                        link.from == manifest.id
                            && link.to == target
                            && link_protocol_matches(&link.protocol, &required.protocol)
                    }),
                    &format!(
                        "set default_links must cover required link {} -> {}",
                        manifest.id, target
                    ),
                )?;
            } else {
                ensure(
                    set_declares_external_required_link(set, &manifest.id, target),
                    &format!(
                        "set policies.network.required_external_links must cover required link {} -> {}",
                        manifest.id, target
                    ),
                )?;
            }
        }
    }
    validate_operation_order(
        &set.operations.install_order,
        &set_service_ids,
        "install_order",
    )?;
    validate_operation_order(&set.operations.start_order, &set_service_ids, "start_order")?;
    validate_operation_order(&set.operations.stop_order, &set_service_ids, "stop_order")?;
    Ok(())
}

fn read_text(path: impl AsRef<Path>, label: &str) -> Result<String> {
    std::fs::read_to_string(path).map_err(|error| {
        OrchestratorError::Dependency(format!("cannot read {label}: {}", error.kind()))
    })
}

fn discover_service_manifests(repo_root: &Path) -> Result<Vec<ServiceManifest>> {
    let services_dir = repo_root.join("services");
    if !services_dir.is_dir() {
        return Err(OrchestratorError::UnsafePath(
            "services directory is not available".into(),
        ));
    }
    let entries = std::fs::read_dir(&services_dir).map_err(|error| {
        OrchestratorError::Dependency(format!("cannot list services directory: {}", error.kind()))
    })?;
    let mut services = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|error| {
            OrchestratorError::Dependency(format!("cannot read services entry: {}", error.kind()))
        })?;
        if !entry
            .file_type()
            .map_err(|error| {
                OrchestratorError::Dependency(format!(
                    "cannot inspect services entry: {}",
                    error.kind()
                ))
            })?
            .is_dir()
        {
            continue;
        }
        let rel = Path::new("services")
            .join(entry.file_name())
            .join("service.yaml");
        if repo_root.join(&rel).is_file() {
            services.push(validate_service_manifest_file(repo_root, &rel)?);
        }
    }
    Ok(services)
}

fn validate_operation_order(items: &[String], services: &HashSet<&str>, field: &str) -> Result<()> {
    let mut seen = HashSet::new();
    for item in items {
        ensure(
            services.contains(item.as_str()),
            &format!("{field} references service outside set"),
        )?;
        if !seen.insert(item.as_str()) {
            return Err(OrchestratorError::InvalidManifest(format!(
                "{field} contains duplicate service"
            )));
        }
    }
    Ok(())
}

fn validate_service_manifest_path(repo_root: &Path, path: &Path) -> Result<()> {
    validate_repo_path(
        repo_root,
        path,
        "services",
        Some("service.yaml"),
        "service manifest",
    )
}

fn validate_release_path(repo_root: &Path, path: &Path) -> Result<()> {
    validate_repo_path(
        repo_root,
        path,
        "services",
        Some("release.yaml"),
        "release manifest",
    )
}

fn validate_set_path(repo_root: &Path, path: &Path) -> Result<()> {
    validate_repo_path(repo_root, path, "sets", None, "set file")
}

fn validate_repo_path(
    repo_root: &Path,
    path: &Path,
    scope: &str,
    expected_name: Option<&str>,
    label: &str,
) -> Result<()> {
    ensure(
        !path.is_absolute(),
        &format!("{label} path must be relative"),
    )?;
    reject_path_components(path)?;
    if let Some(name) = expected_name {
        ensure(
            path.file_name().and_then(|value| value.to_str()) == Some(name),
            &format!("{label} file must be {name}"),
        )?;
    }
    let canonical_scope = repo_root.join(scope).canonicalize().map_err(|_| {
        OrchestratorError::UnsafePath(format!("{scope} directory is not available"))
    })?;
    let canonical_path = repo_root
        .join(path)
        .canonicalize()
        .map_err(|_| OrchestratorError::UnsafePath(sanitize_path_for_error(path)))?;
    ensure(
        canonical_path.starts_with(canonical_scope),
        &format!("{label} must stay under {scope}"),
    )
}

fn reject_path_components(path: &Path) -> Result<()> {
    let banned = [".tmp", ".env", "node_modules", "dist", "target", ".git"];
    for component in path.components() {
        match component {
            Component::ParentDir => {
                return Err(OrchestratorError::UnsafePath(
                    "path traversal is not allowed".into(),
                ));
            }
            Component::RootDir | Component::Prefix(_) => {
                return Err(OrchestratorError::UnsafePath(
                    "absolute path is not allowed".into(),
                ));
            }
            Component::Normal(value) => {
                let text = value.to_str().ok_or_else(|| {
                    OrchestratorError::UnsafePath("path segment must be UTF-8".into())
                })?;
                if banned.iter().any(|item| text.eq_ignore_ascii_case(item)) {
                    return Err(OrchestratorError::UnsafePath(format!(
                        "banned path segment {text}"
                    )));
                }
            }
            Component::CurDir => {}
        }
    }
    Ok(())
}

fn release_route_covers_service_route(release_route: &str, service_route: &str) -> bool {
    let release_base = release_route
        .trim_end_matches("/**")
        .trim_end_matches("/*")
        .trim_end_matches('*')
        .trim_end_matches('/');
    let service_base = service_route.trim_end_matches('/');
    release_base == service_base || service_base.starts_with(&format!("{release_base}/"))
}

fn link_protocol_matches(set_protocol: &str, required_protocol: &str) -> bool {
    set_protocol.trim().is_empty()
        || required_protocol.trim().is_empty()
        || set_protocol.eq_ignore_ascii_case(required_protocol)
}

fn set_declares_external_required_link(set: &DeploymentTemplate, from: &str, to: &str) -> bool {
    set.policies
        .get("network")
        .and_then(|network| network.get("required_external_links"))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .any(|item| item.trim() == format!("{from} -> {to}"))
}

fn ensure(ok: bool, message: &str) -> Result<()> {
    if ok {
        Ok(())
    } else {
        Err(OrchestratorError::InvalidManifest(message.to_string()))
    }
}
