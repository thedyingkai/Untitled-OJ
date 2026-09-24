//! Store node responsibilities.
use crate::durable::DurableStore;
use crate::store::context::now_ms;
use crate::store::error::{StoreError, storage_error};
use orchestrator_core::NodeRecord;
use orchestrator_core::ServiceReleaseContract;
use orchestrator_manager::catalog_v2::TargetPlatform;
use orchestrator_protocol::NodeRuntimeFactsV1;
use orchestrator_protocol::RuntimeContract;
use orchestrator_protocol::RuntimeProfile;
use serde_json::Value;

pub(crate) const NODE_RUNTIME_FACTS_STALE_MS: i64 = 60_000;

pub(crate) fn require_node_provider(node: &NodeRecord, provider: &str) -> Result<(), StoreError> {
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
        Err(StoreError::new(
            422,
            "STORE_PROVIDER_REQUIRED",
            format!(
                "target Node {} does not advertise providers.{provider}=true",
                node.node_id
            ),
        ))
    }
}

pub(crate) fn node_provider_label<'a>(node: &'a NodeRecord, provider: &str) -> Option<&'a Value> {
    node.labels
        .get("providers")
        .and_then(|providers| providers.get(provider))
        .or_else(|| node.labels.get(format!("provider.{provider}")))
}

pub(crate) fn provider_identifier(
    node: &NodeRecord,
    provider: &str,
    field: &str,
) -> Result<String, StoreError> {
    require_node_provider(node, provider)?;
    match node_provider_label(node, provider) {
        Some(Value::Object(configuration)) => configuration
            .get(field)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
            .ok_or_else(|| {
                StoreError::new(
                    422,
                    "STORE_PROVIDER_CONFIGURATION_INVALID",
                    format!(
                        "target Node {} providers.{provider}.{field} must be a non-empty identifier",
                        node.node_id
                    ),
                )
            }),
        Some(_) => Err(StoreError::new(
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

pub(crate) fn storage_provider_selection(
    node: &NodeRecord,
) -> Result<(String, String), StoreError> {
    require_node_provider(node, "storage")?;
    let Some(Value::Object(configuration)) = node_provider_label(node, "storage") else {
        return Err(StoreError::new(
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
        return Err(StoreError::new(
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

pub(crate) fn ensure_ready_docker_node(
    storage: &DurableStore,
    node: &NodeRecord,
) -> Result<(), StoreError> {
    if !node.status.eq_ignore_ascii_case("READY") {
        return Err(StoreError::new(
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
        return Err(StoreError::new(
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

pub(crate) fn target_platform(
    storage: &DurableStore,
    node: &NodeRecord,
) -> Result<TargetPlatform, StoreError> {
    let facts = node_runtime_facts(storage, &node.node_id)?;
    let os = facts.docker.os_type.trim();
    let arch = facts.docker.architecture.trim();
    if !valid_platform_token(os) || !valid_platform_token(arch) {
        return Err(StoreError::new(
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

pub(crate) fn node_runtime_facts(
    storage: &DurableStore,
    node_id: &str,
) -> Result<NodeRuntimeFactsV1, StoreError> {
    let stored = storage
        .node_runtime_facts(node_id)
        .map_err(storage_error)?
        .ok_or_else(|| {
            StoreError::new(
                422,
                "STORE_NODE_RUNTIME_FACTS_REQUIRED",
                format!("target Node {node_id} has not submitted authenticated runtime facts"),
            )
        })?;
    let now = now_ms();
    if stored.is_stale_at(now, NODE_RUNTIME_FACTS_STALE_MS) {
        return Err(StoreError::new(
            409,
            "STORE_NODE_RUNTIME_FACTS_STALE",
            format!(
                "target Node {node_id} runtime facts are older than {} seconds",
                NODE_RUNTIME_FACTS_STALE_MS / 1_000
            ),
        ));
    }
    serde_json::from_value(stored.facts).map_err(|error| {
        StoreError::new(
            500,
            "STORE_NODE_RUNTIME_FACTS_INVALID",
            format!("target Node {node_id} runtime facts cannot be decoded: {error}"),
        )
    })
}

pub(crate) fn release_runtime_contract(
    contract: &ServiceReleaseContract,
) -> Result<RuntimeContract, StoreError> {
    if contract.contract_version == 1 {
        return Ok(RuntimeContract::standard_v1());
    }
    let id = match contract.runtime_contract.id.as_str() {
        "standard-container-v1" => RuntimeProfile::StandardV1,
        "judge-sandbox-v1" => RuntimeProfile::JudgeSandboxV1,
        other => {
            return Err(StoreError::new(
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
        StoreError::new(
            422,
            "STORE_RUNTIME_CONTRACT_DIGEST_MISMATCH",
            error.to_string(),
        )
    })?;
    Ok(selected)
}

pub(crate) fn ensure_release_runtime_supported(
    storage: &DurableStore,
    node: &NodeRecord,
    contract: &ServiceReleaseContract,
    image: &str,
) -> Result<RuntimeContract, StoreError> {
    let requested = release_runtime_contract(contract)?;
    let facts = node_runtime_facts(storage, &node.node_id)?;
    if !facts
        .allowed_contracts
        .iter()
        .any(|allowed| allowed == &requested)
    {
        return Err(StoreError::new(
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
        return Err(StoreError::new(
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

pub(crate) fn host_platform() -> TargetPlatform {
    TargetPlatform::new(
        normalize_os(std::env::consts::OS),
        normalize_arch(std::env::consts::ARCH),
    )
}

pub(crate) fn valid_platform_token(value: &str) -> bool {
    let value = value.trim();
    !value.is_empty()
        && value.len() <= 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}

pub(crate) fn normalize_os(value: &str) -> String {
    match value.trim().to_ascii_lowercase().as_str() {
        "win32" => "windows".to_string(),
        "darwin" => "macos".to_string(),
        value => value.to_string(),
    }
}

pub(crate) fn normalize_arch(value: &str) -> String {
    match value.trim().to_ascii_lowercase().as_str() {
        "amd64" | "x64" => "x86_64".to_string(),
        "arm64" => "aarch64".to_string(),
        value => value.to_string(),
    }
}
