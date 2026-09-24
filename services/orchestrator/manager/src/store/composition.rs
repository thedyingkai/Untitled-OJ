//! Pure conversion from verified release metadata to composition plans and inputs.
use super::config::collect_config_secret_paths;
use super::{StoreRuleError, VerifiedReleaseDocument, stable_service_instance_id};
use orchestrator_core::ServiceReleaseContract;
use orchestrator_core::composition::{
    ApiRequirementV1 as CompositionApiRequirementV1, CompositionNodeSpecV1,
    CompositionPlanBindingV1, CompositionPlanV1, CompositionReleaseV1, ConfigRequirementV1,
    INSTALL_INPUTS_SCHEMA_VERSION, InstallInputsV1, PackageDependencyV1, ProvidedApiV1,
    ProviderPolicyV1, ReleaseGraphV1, ResourceLifecycleV1, ResourceRequirementV1,
    SecretRequirementV1, ValidatedInstallInputsV1, validate_install_inputs,
};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

pub fn plan_composition(
    graph: ReleaseGraphV1,
    providers: &[orchestrator_core::composition::ProviderCandidateV1],
) -> Result<CompositionPlanV1, StoreRuleError> {
    orchestrator_core::composition::build_composition_plan(
        graph,
        providers,
        orchestrator_core::composition::CompositionModeV1::Production,
    )
    .map_err(composition_error)
}

pub fn release_contract_from_document(
    document: &VerifiedReleaseDocument,
) -> Result<ServiceReleaseContract, StoreRuleError> {
    let text = std::str::from_utf8(&document.bytes).map_err(|error| {
        StoreRuleError::invalid(
            "STORE_RELEASE_INVALID",
            format!(
                "release {}@{} metadata is not UTF-8: {error}",
                document.selection.module_id, document.selection.release.version
            ),
        )
    })?;
    let contract = ServiceReleaseContract::from_yaml_str(text).map_err(|error| {
        StoreRuleError::invalid(
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
        return Err(StoreRuleError::conflict(
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

pub fn release_graph(
    documents: &[VerifiedReleaseDocument],
    root_service_id: &str,
) -> Result<ReleaseGraphV1, StoreRuleError> {
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
    Ok(ReleaseGraphV1 {
        schema_version: orchestrator_core::composition::RELEASE_GRAPH_SCHEMA_VERSION.to_string(),
        root_service_id: root_service_id.to_string(),
        releases,
    })
}

pub fn validate_store_composition_inputs(
    plan: &CompositionPlanV1,
    plan_digest: &str,
    release_graph_digest: &str,
    inputs: &BTreeMap<String, BTreeMap<String, Value>>,
    config: Option<Value>,
    secret_refs: BTreeMap<String, String>,
) -> Result<ValidatedInstallInputsV1, StoreRuleError> {
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

pub fn legacy_composition_inputs(
    plan: &CompositionPlanV1,
    config: &Value,
    secret_refs: &BTreeMap<String, String>,
) -> Result<ValidatedInstallInputsV1, StoreRuleError> {
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

pub fn composition_inputs_for_service(
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

pub fn normalize_composition_version(value: &str) -> Option<semver::Version> {
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

pub(crate) fn composition_error(error: impl std::fmt::Display) -> StoreRuleError {
    StoreRuleError::invalid("STORE_COMPOSITION_INVALID", error.to_string())
}
