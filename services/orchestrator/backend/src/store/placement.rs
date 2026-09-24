//! Store placement responsibilities.
use crate::durable::DurableStore;
use crate::store::error::{StoreError, storage_error};
use orchestrator_control_plane::OperationRepository;
use orchestrator_core::NodeRecord;
use orchestrator_core::ServiceReleaseManifest;
use orchestrator_core::parse_endpoint_id;
use orchestrator_core::validate_endpoint_id;
use orchestrator_runtime::ContainerSpec;
use orchestrator_runtime::OciImageReference;
use orchestrator_runtime::PublishedEndpoint;
use orchestrator_runtime::PublishedPortProtocol;
use orchestrator_runtime::RuntimeContract;
use serde_json::Value;
use sha2::Digest;
use sha2::Sha256;
use std::collections::BTreeSet;
use std::collections::HashMap;
use std::net::IpAddr;

pub(crate) fn ensure_no_active_replacement(
    storage: &DurableStore,
    deployment_id: &str,
    expected_operation_id: Option<&str>,
) -> Result<(), StoreError> {
    let active = storage
        .operation_store()
        .list()
        .map_err(|error| StoreError::new(500, "STORE_OPERATION_ERROR", error.to_string()))?
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
        Err(StoreError::new(
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

pub(crate) fn ensure_no_active_deployment_mutation(
    storage: &DurableStore,
    deployment_id: &str,
    expected_operation_id: Option<&str>,
) -> Result<(), StoreError> {
    let active = storage
        .operation_store()
        .list()
        .map_err(|error| StoreError::new(500, "STORE_OPERATION_ERROR", error.to_string()))?
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
        Err(StoreError::new(
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

// These values are the complete signed release/runtime binding and keeping
// them explicit makes it difficult for a caller to omit one accidentally.
#[allow(clippy::too_many_arguments)]
pub(crate) fn container_spec(
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

pub(crate) fn managed_published_endpoint(
    endpoint: &str,
    service_id: &str,
    node: &NodeRecord,
    release: &ServiceReleaseManifest,
) -> Result<Option<PublishedEndpoint>, StoreError> {
    let endpoint = endpoint.trim();
    if endpoint.is_empty() {
        return Ok(None);
    }
    validate_endpoint_id(endpoint).map_err(|error| {
        StoreError::new(
            422,
            "STORE_MANAGED_ENDPOINT_INVALID",
            format!("managed endpoint is invalid: {error}"),
        )
    })?;
    let identity = parse_endpoint_id(endpoint).map_err(|error| {
        StoreError::new(
            422,
            "STORE_MANAGED_ENDPOINT_INVALID",
            format!("managed endpoint is invalid: {error}"),
        )
    })?;
    if identity.service_name != service_id {
        return Err(StoreError::new(
            422,
            "STORE_MANAGED_ENDPOINT_SERVICE_MISMATCH",
            format!(
                "managed endpoint service {} must match release service {service_id}",
                identity.service_name
            ),
        ));
    }
    let advertised_host = identity.host.parse::<IpAddr>().map_err(|_| {
        StoreError::new(
            422,
            "STORE_MANAGED_ENDPOINT_INVALID",
            "managed endpoint host must be an IP address",
        )
    })?;
    let node_host = node.host_ip.parse::<IpAddr>().map_err(|_| {
        StoreError::new(
            422,
            "STORE_TARGET_NODE_ENDPOINT_UNAVAILABLE",
            format!(
                "target Node {} does not advertise a valid host_ip",
                node.node_id
            ),
        )
    })?;
    if advertised_host != node_host {
        return Err(StoreError::new(
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
        StoreError::new(
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
        StoreError::new(
            422,
            "STORE_MANAGED_ENDPOINT_INVALID",
            format!("managed endpoint cannot be published: {error}"),
        )
    })?;
    Ok(Some(published))
}

pub(crate) fn effective_managed_endpoint(
    requested: &str,
    node: &NodeRecord,
    release: &ServiceReleaseManifest,
) -> Result<String, StoreError> {
    if !requested.trim().is_empty() {
        return Ok(requested.trim().to_string());
    }
    if !release.service_type.eq_ignore_ascii_case("backend-worker") {
        return Ok(String::new());
    }
    node.host_ip.parse::<IpAddr>().map_err(|_| {
        StoreError::new(
            422,
            "STORE_TARGET_NODE_ENDPOINT_UNAVAILABLE",
            format!(
                "target Node {} does not advertise a valid host_ip for its logical worker endpoint",
                node.node_id
            ),
        )
    })?;
    if release.backend.port == 0 {
        return Err(StoreError::new(
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

pub(crate) fn endpoint_socket(endpoint: &str) -> Option<(IpAddr, u16)> {
    let identity = parse_endpoint_id(endpoint).ok()?;
    Some((identity.host.parse().ok()?, identity.port.parse().ok()?))
}

pub(crate) fn allocate_replacement_endpoint(
    storage: &DurableStore,
    current_endpoint: &str,
    service_id: &str,
    deployment_id: &str,
) -> Result<String, StoreError> {
    let identity = parse_endpoint_id(current_endpoint).map_err(|error| {
        StoreError::new(422, "STORE_REPLACEMENT_ENDPOINT_INVALID", error.to_string())
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
    Err(StoreError::new(
        503,
        "STORE_REPLACEMENT_ENDPOINT_EXHAUSTED",
        format!(
            "no temporary replacement endpoint is available on {}",
            identity.host
        ),
    ))
}

pub(crate) fn ensure_endpoint_available(
    storage: &DurableStore,
    endpoint: &PublishedEndpoint,
    excluded_deployment_id: Option<&str>,
    expected_operation_id: Option<&str>,
) -> Result<(), StoreError> {
    let desired_socket = endpoint_socket(&endpoint.endpoint).ok_or_else(|| {
        StoreError::new(
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
        return Err(StoreError::new(
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
        .map_err(|error| StoreError::new(500, "STORE_OPERATION_ERROR", error.to_string()))?;
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
        return Err(StoreError::new(
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

pub(crate) fn ensure_deployment_available(
    storage: &DurableStore,
    deployment_id: &str,
    expected_digest: &str,
    expected_operation_id: Option<&str>,
) -> Result<(), StoreError> {
    if let Some(existing) = storage
        .runtime_instance(deployment_id)
        .map_err(storage_error)?
    {
        let same_digest = existing.instance.artifact_digest == expected_digest
            || existing.instance.artifact_digest.ends_with(expected_digest);
        return Err(StoreError::new(
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
        .map_err(|error| StoreError::new(500, "STORE_OPERATION_ERROR", error.to_string()))?;
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
        return Err(StoreError::new(
            409,
            "STORE_INSTALL_IN_PROGRESS",
            format!("deployment {deployment_id} already has an active Operation"),
        ));
    }
    Ok(())
}
