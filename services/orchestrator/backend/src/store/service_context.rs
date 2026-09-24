//! Store service context responsibilities.
use crate::durable::DurableStore;
use crate::store::error::{StoreError, storage_error};
use crate::store::node::{node_runtime_facts, provider_identifier};
use orchestrator_core::ApiBinding;
use orchestrator_core::ApiBindingState;
use orchestrator_core::ServiceReleaseContract;
use orchestrator_manager::store::stable_service_instance_id;
use orchestrator_runtime::ContainerSpec;
use orchestrator_runtime::MANAGED_EVENT_STREAM_V1;
use orchestrator_runtime::ManagedApiBinding;
use orchestrator_runtime::ManagedEventBinding;
use orchestrator_runtime::ManagedEventSubscription;
use orchestrator_runtime::ManagedServiceContextSpec;
use orchestrator_runtime::ManagedWorkloadVerifierSpec;
use orchestrator_runtime::RetainedVolumeAttachmentV1;
use orchestrator_runtime::SERVICE_CONTRACT_GENERATION_LABEL;
use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::fs;

pub(crate) fn managed_service_context_spec(
    storage: &DurableStore,
    contract: &ServiceReleaseContract,
    node_id: &str,
    bindings: &[ApiBinding],
    mount_unbound_optional_context: bool,
) -> Result<Option<ManagedServiceContextSpec>, StoreError> {
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
        return Err(StoreError::new(
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
        return Err(StoreError::new(
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
            StoreError::new(
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
                return Err(StoreError::new(
                    503,
                    "STORE_GATEWAY_WORKLOAD_CA_INVALID",
                    "ORCHESTRATOR_GATEWAY_WORKLOAD_CA_FILE must not be empty",
                ));
            }
            fs::read_to_string(path).map_err(|error| {
                StoreError::new(
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
        StoreError::new(
            503,
            "STORE_GATEWAY_WORKLOAD_CONTEXT_INVALID",
            error.to_string(),
        )
    })?;
    Ok(Some(context))
}

pub(crate) const MAX_WORKLOAD_PUBLIC_KEY_FILE_BYTES: u64 = 16 * 1024;

pub(crate) fn load_managed_workload_verifier() -> Result<ManagedWorkloadVerifierSpec, StoreError> {
    let path = required_workload_verifier_env("ORCHESTRATOR_WORKLOAD_PUBLIC_KEY_FILE")?;
    let metadata = fs::metadata(&path).map_err(|error| {
        StoreError::new(
            503,
            "STORE_WORKLOAD_VERIFIER_UNREADABLE",
            format!("read workload verifier public key metadata: {error}"),
        )
    })?;
    if !metadata.is_file()
        || metadata.len() == 0
        || metadata.len() > MAX_WORKLOAD_PUBLIC_KEY_FILE_BYTES
    {
        return Err(StoreError::new(
            503,
            "STORE_WORKLOAD_VERIFIER_INVALID",
            "ORCHESTRATOR_WORKLOAD_PUBLIC_KEY_FILE must name one non-empty regular file no larger than 16 KiB",
        ));
    }
    let public_key_pem = fs::read_to_string(&path).map_err(|error| {
        StoreError::new(
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
        StoreError::new(503, "STORE_WORKLOAD_VERIFIER_INVALID", error.to_string())
    })?;
    Ok(verifier)
}

pub(crate) fn required_workload_verifier_env(name: &str) -> Result<String, StoreError> {
    std::env::var(name)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            StoreError::new(
                503,
                "STORE_WORKLOAD_VERIFIER_REQUIRED",
                format!("signed v3 workload API providers require {name}"),
            )
        })
}

pub(crate) fn contract_has_retained_runtime_volume(contract: &ServiceReleaseContract) -> bool {
    contract
        .platform
        .as_ref()
        .is_some_and(|platform| !platform.runtime_volumes.is_empty())
}

pub(crate) fn attach_release_runtime_volume(
    spec: &mut ContainerSpec,
    contract: &ServiceReleaseContract,
) -> Result<(), StoreError> {
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
        return Err(StoreError::new(
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
            StoreError::new(
                422,
                "STORE_RUNTIME_VOLUME_INVALID",
                format!("signed runtime volume contract is invalid: {error}"),
            )
        })?;
    spec.retained_volume = Some(attachment);
    Ok(())
}

pub(crate) fn managed_event_binding(
    storage: &DurableStore,
    contract: &ServiceReleaseContract,
    node_id: &str,
    generation: u64,
) -> Result<Option<ManagedEventBinding>, StoreError> {
    if contract.events.publishes.is_empty() && contract.events.subscribes.is_empty() {
        return Ok(None);
    }
    let node = storage
        .list_nodes()
        .map_err(storage_error)?
        .into_iter()
        .find(|node| node.node_id == node_id)
        .ok_or_else(|| {
            StoreError::new(
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
        return Err(StoreError::new(
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
