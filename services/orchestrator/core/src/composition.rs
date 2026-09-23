//! Deterministic service composition planning.
//!
//! This module is deliberately pure: it does not read a catalog, database, provider
//! registry, or secret backend. Callers snapshot those facts into a [`ReleaseGraphV1`]
//! and a set of [`ProviderCandidateV1`] values, build a plan, then require a matching
//! [`CompositionPlanBindingV1`] when validating install inputs. Only the validated
//! result is suitable for crossing the side-effect boundary into Job creation.

use semver::{Version, VersionReq};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use thiserror::Error;

pub const RELEASE_GRAPH_SCHEMA_VERSION: &str = "ojos.dev/release-graph/v1";
pub const COMPOSITION_PLAN_SCHEMA_VERSION: &str = "ojos.dev/composition-plan/v1";
pub const INSTALL_INPUTS_SCHEMA_VERSION: &str = "ojos.dev/install-inputs/v1";
pub const VALIDATED_INSTALL_INPUTS_SCHEMA_VERSION: &str = "ojos.dev/validated-install-inputs/v1";

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum CompositionError {
    #[error("unsupported {document} schema version {actual}; expected {expected}")]
    UnsupportedSchemaVersion {
        document: &'static str,
        expected: &'static str,
        actual: String,
    },
    #[error("{field} has invalid identifier {value}")]
    InvalidIdentifier { field: &'static str, value: String },
    #[error("{field} has invalid canonical sha256 digest {value}")]
    InvalidDigest { field: &'static str, value: String },
    #[error("{field} must be a JSON object")]
    InvalidObject { field: &'static str },
    #[error("{field} has invalid semantic version requirement {value}: {message}")]
    InvalidVersionRequirement {
        field: &'static str,
        value: String,
        message: String,
    },
    #[error("duplicate release for service {0}")]
    DuplicateRelease(String),
    #[error("release graph does not contain root service {0}")]
    MissingRootRelease(String),
    #[error("service {service} requires missing package {dependency}")]
    MissingPackageDependency { service: String, dependency: String },
    #[error(
        "service {service} requires package {dependency} {requirement}, but graph selected {actual}"
    )]
    PackageVersionMismatch {
        service: String,
        dependency: String,
        requirement: String,
        actual: Version,
    },
    #[error("duplicate {kind} {name} in service {service}")]
    DuplicateRequirement {
        service: String,
        kind: &'static str,
        name: String,
    },
    #[error("duplicate provider {provider_id} for capability {capability}")]
    DuplicateProvider {
        provider_id: String,
        capability: String,
    },
    #[error("provider {provider_id} conflicts with a package provider for capability {capability}")]
    ProviderIdentityConflict {
        provider_id: String,
        capability: String,
    },
    #[error("duplicate composition node {0}")]
    DuplicateNode(String),
    #[error("composition node {node_id} references unknown service {service_id}")]
    UnknownNodeService { node_id: String, service_id: String },
    #[error("duplicate composition edge {from} -> {to} ({relationship:?})")]
    DuplicateEdge {
        from: String,
        to: String,
        relationship: CompositionEdgeRelationshipV1,
    },
    #[error("composition edge references unknown node {0}")]
    UnknownEdgeNode(String),
    #[error("composition edge cannot reference the same node twice: {0}")]
    SelfEdge(String),
    #[error("composition graph contains a cycle: {cycle}")]
    DependencyCycle { cycle: String },
    #[error("{field} is not in canonical sorted order")]
    NonCanonicalOrder { field: &'static str },
    #[error("composition node {node_id} has invalid provider selection: {message}")]
    InvalidProviderSelection { node_id: String, message: String },
    #[error("composition node {node_id} has invalid unresolved input {key}: {message}")]
    InvalidInputDeclaration {
        node_id: String,
        key: String,
        message: String,
    },
    #[error("release graph digest mismatch: expected {expected}, found {actual}")]
    ReleaseGraphDigestMismatch { expected: String, actual: String },
    #[error("composition plan digest mismatch: expected {expected}, found {actual}")]
    PlanDigestMismatch { expected: String, actual: String },
    #[error("install request is stale: expected {field} {expected}, found {actual}")]
    StaleInstallRequest {
        field: &'static str,
        expected: String,
        actual: String,
    },
    #[error("caller supplied a stale plan: expected {field} {expected}, found {actual}")]
    StalePlan {
        field: &'static str,
        expected: String,
        actual: String,
    },
    #[error("input references unknown composition node {0}")]
    UnknownInputNode(String),
    #[error("input {key} is not accepted by composition node {node_id}")]
    UnknownNodeInput { node_id: String, key: String },
    #[error("required input {key} is missing for composition node {node_id}")]
    MissingNodeInput { node_id: String, key: String },
    #[error("input {key} for composition node {node_id} is invalid: {message}")]
    InvalidNodeInput {
        node_id: String,
        key: String,
        message: String,
    },
    #[error("legacy root alias {alias} has no matching root composition node")]
    UnsupportedLegacyAlias { alias: String },
    #[error("legacy root alias {alias} conflicts with node input {node_id}.{key}")]
    LegacyAliasConflict {
        alias: String,
        node_id: String,
        key: String,
    },
    #[error("cannot serialize deterministic composition material: {0}")]
    Serialization(String),
}

pub type Result<T> = std::result::Result<T, CompositionError>;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "kebab-case")]
pub enum CompositionModeV1 {
    Production,
    Development,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReleaseGraphV1 {
    pub schema_version: String,
    pub root_service_id: String,
    #[serde(default)]
    pub releases: Vec<CompositionReleaseV1>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CompositionReleaseV1 {
    pub service_id: String,
    /// Stable deployment/service-instance identity. Node identities bind this
    /// value so two installations of the same signed release cannot alias one
    /// another in inputs, claims, or idempotency records.
    pub owner_instance_id: String,
    pub version: Version,
    pub release_digest: String,
    #[serde(default)]
    pub package_dependencies: Vec<PackageDependencyV1>,
    #[serde(default)]
    pub provided_apis: Vec<ProvidedApiV1>,
    #[serde(default)]
    pub required_apis: Vec<ApiRequirementV1>,
    #[serde(default)]
    pub resource_claims: Vec<ResourceRequirementV1>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config: Option<ConfigRequirementV1>,
    #[serde(default)]
    pub secrets: Vec<SecretRequirementV1>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PackageDependencyV1 {
    pub service_id: String,
    pub version_requirement: String,
    #[serde(default)]
    pub development: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProvidedApiV1 {
    pub api_id: String,
    pub version: Version,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ApiRequirementV1 {
    pub name: String,
    pub api_id: String,
    pub version_requirement: String,
    #[serde(default)]
    pub optional: bool,
    #[serde(default)]
    pub provider_policy: ProviderPolicyV1,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ResourceRequirementV1 {
    pub name: String,
    pub resource_type: String,
    #[serde(default = "default_any_version")]
    pub version_requirement: String,
    #[serde(default)]
    pub optional: bool,
    #[serde(default)]
    pub provider_policy: ProviderPolicyV1,
    #[serde(default)]
    pub lifecycle: ResourceLifecycleV1,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ConfigRequirementV1 {
    pub schema: Value,
    #[serde(default = "default_true")]
    pub required: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SecretRequirementV1 {
    pub name: String,
    #[serde(default = "default_true")]
    pub required: bool,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum ProviderPolicyV1 {
    #[default]
    UniqueHealthy,
    Explicit,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "UPPERCASE")]
pub enum ResourceLifecycleV1 {
    #[default]
    Retain,
    Delete,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "UPPERCASE")]
pub enum ProviderKindV1 {
    Managed,
    External,
    Package,
}

/// An eligible provider snapshot. Health, placement, trust and operator policy
/// are evaluated by the caller before a candidate enters this pure planner.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProviderCandidateV1 {
    pub provider_id: String,
    pub capability: String,
    pub version: Version,
    pub kind: ProviderKindV1,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub service_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CompositionPlanV1 {
    pub schema_version: String,
    pub mode: CompositionModeV1,
    pub root_service_id: String,
    pub release_graph_digest: String,
    pub plan_digest: String,
    pub release_graph: Vec<CompositionReleaseV1>,
    pub nodes: Vec<CompositionNodeV1>,
    pub edges: Vec<CompositionEdgeV1>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CompositionNodeV1 {
    pub node_id: String,
    pub service_id: String,
    #[serde(flatten)]
    pub spec: CompositionNodeSpecV1,
    #[serde(default)]
    pub unresolved_inputs: Vec<UnresolvedInputV1>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
pub enum CompositionNodeSpecV1 {
    Package {
        version: Version,
        release_digest: String,
    },
    ApiBinding {
        name: String,
        api_id: String,
        version_requirement: String,
        optional: bool,
        provider: ProviderSelectionV1,
    },
    ResourceClaim {
        name: String,
        resource_type: String,
        version_requirement: String,
        optional: bool,
        lifecycle: ResourceLifecycleV1,
        provider: ProviderSelectionV1,
    },
    Config {
        schema: Value,
        schema_digest: String,
        required: bool,
    },
    Secret {
        name: String,
        required: bool,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProviderSelectionV1 {
    pub capability: String,
    pub version_requirement: String,
    pub policy: ProviderPolicyV1,
    #[serde(default)]
    pub candidates: Vec<ProviderCandidateRefV1>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selected_provider_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProviderCandidateRefV1 {
    pub provider_id: String,
    pub version: Version,
    pub kind: ProviderKindV1,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub service_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UnresolvedInputV1 {
    pub key: String,
    pub value_type: CompositionInputTypeV1,
    pub required: bool,
    pub sensitive: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub allowed_values: Vec<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "kebab-case")]
pub enum CompositionInputTypeV1 {
    ProviderId,
    JsonObject,
    SecretRef,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CompositionEdgeV1 {
    pub from: String,
    pub to: String,
    pub relationship: CompositionEdgeRelationshipV1,
}

/// Edges point from prerequisite to consumer.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "kebab-case")]
pub enum CompositionEdgeRelationshipV1 {
    PackageDependency,
    Provider,
    Requirement,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CompositionPlanBindingV1 {
    pub plan_digest: String,
    pub release_graph_digest: String,
}

impl From<&CompositionPlanV1> for CompositionPlanBindingV1 {
    fn from(plan: &CompositionPlanV1) -> Self {
        Self {
            plan_digest: plan.plan_digest.clone(),
            release_graph_digest: plan.release_graph_digest.clone(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct InstallInputsV1 {
    pub schema_version: String,
    pub plan_digest: String,
    pub release_graph_digest: String,
    #[serde(default)]
    pub inputs: BTreeMap<String, BTreeMap<String, Value>>,
    /// One-release migration alias for the root service's config node.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config: Option<Value>,
    /// One-release migration alias for the root service's secret nodes.
    #[serde(default)]
    pub secret_refs: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ValidatedInstallInputsV1 {
    pub schema_version: String,
    pub plan_digest: String,
    pub release_graph_digest: String,
    pub inputs: BTreeMap<String, BTreeMap<String, Value>>,
    pub normalized_legacy_aliases: bool,
}

pub fn build_composition_plan(
    graph: ReleaseGraphV1,
    providers: &[ProviderCandidateV1],
    mode: CompositionModeV1,
) -> Result<CompositionPlanV1> {
    validate_release_graph_source(&graph)?;
    validate_provider_candidates(providers)?;
    let release_graph = reachable_release_graph(&graph, mode)?;
    let root_service_id = graph.root_service_id;
    let release_graph_digest =
        compute_release_graph_digest(mode, &root_service_id, &release_graph)?;

    let releases = release_graph
        .iter()
        .map(|release| (release.service_id.as_str(), release))
        .collect::<BTreeMap<_, _>>();
    let mut nodes = Vec::new();
    let mut edges = Vec::new();

    for release in &release_graph {
        let package_id = package_node_id(release);
        nodes.push(CompositionNodeV1 {
            node_id: package_id.clone(),
            service_id: release.service_id.clone(),
            spec: CompositionNodeSpecV1::Package {
                version: release.version.clone(),
                release_digest: release.release_digest.clone(),
            },
            unresolved_inputs: Vec::new(),
        });
        for dependency in &release.package_dependencies {
            let dependency_release = releases
                .get(dependency.service_id.as_str())
                .expect("reachable graph contains every package dependency");
            edges.push(CompositionEdgeV1 {
                from: package_node_id(dependency_release),
                to: package_id.clone(),
                relationship: CompositionEdgeRelationshipV1::PackageDependency,
            });
        }

        for requirement in &release.required_apis {
            let node_id = api_node_id(release, &requirement.name);
            let candidates = api_candidates(requirement, &release_graph, providers)?;
            let provider = select_provider(
                &node_id,
                &requirement.api_id,
                &requirement.version_requirement,
                requirement.provider_policy,
                candidates,
            )?;
            let unresolved_inputs = provider_input(&provider, !requirement.optional);
            if let Some(service_id) = provider
                .selected_provider_id
                .as_deref()
                .and_then(|id| selected_package_service(&provider, id))
            {
                let provider_release = releases
                    .get(service_id)
                    .expect("selected package provider belongs to the release graph");
                edges.push(CompositionEdgeV1 {
                    from: package_node_id(provider_release),
                    to: node_id.clone(),
                    relationship: CompositionEdgeRelationshipV1::Provider,
                });
            }
            edges.push(CompositionEdgeV1 {
                from: node_id.clone(),
                to: package_id.clone(),
                relationship: CompositionEdgeRelationshipV1::Requirement,
            });
            nodes.push(CompositionNodeV1 {
                node_id,
                service_id: release.service_id.clone(),
                spec: CompositionNodeSpecV1::ApiBinding {
                    name: requirement.name.clone(),
                    api_id: requirement.api_id.clone(),
                    version_requirement: requirement.version_requirement.clone(),
                    optional: requirement.optional,
                    provider,
                },
                unresolved_inputs,
            });
        }

        for requirement in &release.resource_claims {
            let node_id = resource_node_id(release, &requirement.name);
            let candidates = capability_candidates(
                &requirement.resource_type,
                &requirement.version_requirement,
                providers,
            )?;
            let provider = select_provider(
                &node_id,
                &requirement.resource_type,
                &requirement.version_requirement,
                requirement.provider_policy,
                candidates,
            )?;
            let unresolved_inputs = provider_input(&provider, !requirement.optional);
            if let Some(service_id) = provider
                .selected_provider_id
                .as_deref()
                .and_then(|id| selected_package_service(&provider, id))
                .filter(|service_id| releases.contains_key(service_id))
            {
                let provider_release = releases
                    .get(service_id)
                    .expect("selected package provider belongs to the release graph");
                edges.push(CompositionEdgeV1 {
                    from: package_node_id(provider_release),
                    to: node_id.clone(),
                    relationship: CompositionEdgeRelationshipV1::Provider,
                });
            }
            edges.push(CompositionEdgeV1 {
                from: node_id.clone(),
                to: package_id.clone(),
                relationship: CompositionEdgeRelationshipV1::Requirement,
            });
            nodes.push(CompositionNodeV1 {
                node_id,
                service_id: release.service_id.clone(),
                spec: CompositionNodeSpecV1::ResourceClaim {
                    name: requirement.name.clone(),
                    resource_type: requirement.resource_type.clone(),
                    version_requirement: requirement.version_requirement.clone(),
                    optional: requirement.optional,
                    lifecycle: requirement.lifecycle,
                    provider,
                },
                unresolved_inputs,
            });
        }

        if let Some(config) = &release.config {
            let node_id = config_node_id(release);
            let schema_digest = digest_serializable(&config.schema)?;
            nodes.push(CompositionNodeV1 {
                node_id: node_id.clone(),
                service_id: release.service_id.clone(),
                spec: CompositionNodeSpecV1::Config {
                    schema: config.schema.clone(),
                    schema_digest,
                    required: config.required,
                },
                unresolved_inputs: vec![UnresolvedInputV1 {
                    key: "config".to_string(),
                    value_type: CompositionInputTypeV1::JsonObject,
                    required: config.required,
                    sensitive: false,
                    allowed_values: Vec::new(),
                }],
            });
            edges.push(CompositionEdgeV1 {
                from: node_id,
                to: package_id.clone(),
                relationship: CompositionEdgeRelationshipV1::Requirement,
            });
        }

        for secret in &release.secrets {
            let node_id = secret_node_id(release, &secret.name);
            nodes.push(CompositionNodeV1 {
                node_id: node_id.clone(),
                service_id: release.service_id.clone(),
                spec: CompositionNodeSpecV1::Secret {
                    name: secret.name.clone(),
                    required: secret.required,
                },
                unresolved_inputs: vec![UnresolvedInputV1 {
                    key: "secretRef".to_string(),
                    value_type: CompositionInputTypeV1::SecretRef,
                    required: secret.required,
                    sensitive: true,
                    allowed_values: Vec::new(),
                }],
            });
            edges.push(CompositionEdgeV1 {
                from: node_id,
                to: package_id.clone(),
                relationship: CompositionEdgeRelationshipV1::Requirement,
            });
        }
    }

    nodes.sort_by(|left, right| left.node_id.cmp(&right.node_id));
    edges.sort();
    let mut plan = CompositionPlanV1 {
        schema_version: COMPOSITION_PLAN_SCHEMA_VERSION.to_string(),
        mode,
        root_service_id,
        release_graph_digest,
        plan_digest: String::new(),
        release_graph,
        nodes,
        edges,
    };
    validate_plan_structure(&plan, false)?;
    plan.plan_digest = compute_plan_digest(&plan)?;
    plan.validate()?;
    Ok(plan)
}

impl CompositionPlanV1 {
    pub fn binding(&self) -> CompositionPlanBindingV1 {
        self.into()
    }

    pub fn validate(&self) -> Result<()> {
        validate_plan_structure(self, true)
    }
}

pub fn compute_release_graph_digest(
    mode: CompositionModeV1,
    root_service_id: &str,
    releases: &[CompositionReleaseV1],
) -> Result<String> {
    #[derive(Serialize)]
    #[serde(rename_all = "camelCase")]
    struct Material<'a> {
        schema_version: &'static str,
        mode: CompositionModeV1,
        root_service_id: &'a str,
        releases: &'a [CompositionReleaseV1],
    }
    digest_serializable(&Material {
        schema_version: RELEASE_GRAPH_SCHEMA_VERSION,
        mode,
        root_service_id,
        releases,
    })
}

pub fn compute_plan_digest(plan: &CompositionPlanV1) -> Result<String> {
    #[derive(Serialize)]
    #[serde(rename_all = "camelCase")]
    struct Material<'a> {
        schema_version: &'a str,
        mode: CompositionModeV1,
        root_service_id: &'a str,
        release_graph_digest: &'a str,
        release_graph: &'a [CompositionReleaseV1],
        nodes: &'a [CompositionNodeV1],
        edges: &'a [CompositionEdgeV1],
    }
    digest_serializable(&Material {
        schema_version: &plan.schema_version,
        mode: plan.mode,
        root_service_id: &plan.root_service_id,
        release_graph_digest: &plan.release_graph_digest,
        release_graph: &plan.release_graph,
        nodes: &plan.nodes,
        edges: &plan.edges,
    })
}

/// Validates digest preconditions and all per-node inputs without side effects.
///
/// `expected` is the latest plan binding held by the caller (normally from a
/// validate response or CAS record). Both the supplied plan and request must match
/// it. Digest checks intentionally happen before any missing/invalid input errors.
pub fn validate_install_inputs(
    plan: &CompositionPlanV1,
    request: &InstallInputsV1,
    expected: &CompositionPlanBindingV1,
) -> Result<ValidatedInstallInputsV1> {
    plan.validate()?;
    validate_digest("expected plan digest", &expected.plan_digest)?;
    validate_digest(
        "expected release graph digest",
        &expected.release_graph_digest,
    )?;
    if plan.plan_digest != expected.plan_digest {
        return Err(CompositionError::StalePlan {
            field: "planDigest",
            expected: expected.plan_digest.clone(),
            actual: plan.plan_digest.clone(),
        });
    }
    if plan.release_graph_digest != expected.release_graph_digest {
        return Err(CompositionError::StalePlan {
            field: "releaseGraphDigest",
            expected: expected.release_graph_digest.clone(),
            actual: plan.release_graph_digest.clone(),
        });
    }
    if request.schema_version != INSTALL_INPUTS_SCHEMA_VERSION {
        return Err(CompositionError::UnsupportedSchemaVersion {
            document: "install inputs",
            expected: INSTALL_INPUTS_SCHEMA_VERSION,
            actual: request.schema_version.clone(),
        });
    }
    if request.plan_digest != expected.plan_digest {
        return Err(CompositionError::StaleInstallRequest {
            field: "planDigest",
            expected: expected.plan_digest.clone(),
            actual: request.plan_digest.clone(),
        });
    }
    if request.release_graph_digest != expected.release_graph_digest {
        return Err(CompositionError::StaleInstallRequest {
            field: "releaseGraphDigest",
            expected: expected.release_graph_digest.clone(),
            actual: request.release_graph_digest.clone(),
        });
    }

    let (normalized, normalized_legacy_aliases) = normalize_legacy_root_aliases(plan, request)?;
    let nodes = plan
        .nodes
        .iter()
        .map(|node| (node.node_id.as_str(), node))
        .collect::<BTreeMap<_, _>>();

    for (node_id, values) in &normalized {
        let node = nodes
            .get(node_id.as_str())
            .ok_or_else(|| CompositionError::UnknownInputNode(node_id.clone()))?;
        let accepted = node
            .unresolved_inputs
            .iter()
            .map(|input| input.key.as_str())
            .collect::<BTreeSet<_>>();
        for key in values.keys() {
            if !accepted.contains(key.as_str()) {
                return Err(CompositionError::UnknownNodeInput {
                    node_id: node_id.clone(),
                    key: key.clone(),
                });
            }
        }
    }

    let mut validated = BTreeMap::new();
    for node in &plan.nodes {
        let supplied = normalized.get(&node.node_id);
        let mut values = BTreeMap::new();
        if let Some(provider) = node_provider(&node.spec)
            && let Some(selected) = &provider.selected_provider_id
        {
            values.insert("providerId".to_string(), Value::String(selected.clone()));
        }
        for declaration in &node.unresolved_inputs {
            match supplied.and_then(|values| values.get(&declaration.key)) {
                Some(value) => {
                    validate_input_value(&node.node_id, declaration, value)?;
                    values.insert(declaration.key.clone(), value.clone());
                }
                None if declaration.required => {
                    return Err(CompositionError::MissingNodeInput {
                        node_id: node.node_id.clone(),
                        key: declaration.key.clone(),
                    });
                }
                None => {}
            }
        }
        if !values.is_empty() {
            validated.insert(node.node_id.clone(), values);
        }
    }

    Ok(ValidatedInstallInputsV1 {
        schema_version: VALIDATED_INSTALL_INPUTS_SCHEMA_VERSION.to_string(),
        plan_digest: plan.plan_digest.clone(),
        release_graph_digest: plan.release_graph_digest.clone(),
        inputs: validated,
        normalized_legacy_aliases,
    })
}

fn validate_release_graph_source(graph: &ReleaseGraphV1) -> Result<()> {
    if graph.schema_version != RELEASE_GRAPH_SCHEMA_VERSION {
        return Err(CompositionError::UnsupportedSchemaVersion {
            document: "release graph",
            expected: RELEASE_GRAPH_SCHEMA_VERSION,
            actual: graph.schema_version.clone(),
        });
    }
    validate_identifier("rootServiceId", &graph.root_service_id)?;
    let mut releases = BTreeSet::new();
    for release in &graph.releases {
        validate_release(release)?;
        if !releases.insert(release.service_id.as_str()) {
            return Err(CompositionError::DuplicateRelease(
                release.service_id.clone(),
            ));
        }
    }
    if !releases.contains(graph.root_service_id.as_str()) {
        return Err(CompositionError::MissingRootRelease(
            graph.root_service_id.clone(),
        ));
    }
    Ok(())
}

fn validate_release(release: &CompositionReleaseV1) -> Result<()> {
    validate_identifier("release.serviceId", &release.service_id)?;
    validate_identifier("release.ownerInstanceId", &release.owner_instance_id)?;
    validate_digest("release.releaseDigest", &release.release_digest)?;
    let mut package_dependencies = BTreeSet::new();
    for dependency in &release.package_dependencies {
        validate_identifier("packageDependency.serviceId", &dependency.service_id)?;
        parse_requirement(
            "packageDependency.versionRequirement",
            &dependency.version_requirement,
        )?;
        if !package_dependencies.insert(dependency.service_id.as_str()) {
            return Err(CompositionError::DuplicateRequirement {
                service: release.service_id.clone(),
                kind: "package dependency",
                name: dependency.service_id.clone(),
            });
        }
    }
    let mut provided_apis = BTreeSet::new();
    for api in &release.provided_apis {
        validate_identifier("providedApi.apiId", &api.api_id)?;
        if !provided_apis.insert(api.api_id.as_str()) {
            return Err(CompositionError::DuplicateRequirement {
                service: release.service_id.clone(),
                kind: "provided API",
                name: api.api_id.clone(),
            });
        }
    }
    let mut required_apis = BTreeSet::new();
    for api in &release.required_apis {
        validate_identifier("requiredApi.name", &api.name)?;
        validate_identifier("requiredApi.apiId", &api.api_id)?;
        parse_requirement("requiredApi.versionRequirement", &api.version_requirement)?;
        if !required_apis.insert(api.name.as_str()) {
            return Err(CompositionError::DuplicateRequirement {
                service: release.service_id.clone(),
                kind: "required API",
                name: api.name.clone(),
            });
        }
    }
    let mut resources = BTreeSet::new();
    for resource in &release.resource_claims {
        validate_identifier("resource.name", &resource.name)?;
        validate_identifier("resource.resourceType", &resource.resource_type)?;
        parse_requirement("resource.versionRequirement", &resource.version_requirement)?;
        if !resources.insert(resource.name.as_str()) {
            return Err(CompositionError::DuplicateRequirement {
                service: release.service_id.clone(),
                kind: "resource claim",
                name: resource.name.clone(),
            });
        }
        if resource.lifecycle != ResourceLifecycleV1::Retain {
            return Err(CompositionError::InvalidProviderSelection {
                node_id: composition_node_id(release, "resource", &resource.name),
                message: "resource lifecycle v1 is RETAIN-only; purge is a separate audited action"
                    .to_string(),
            });
        }
    }
    if let Some(config) = &release.config
        && !config.schema.is_object()
    {
        return Err(CompositionError::InvalidObject {
            field: "config.schema",
        });
    }
    let mut secrets = BTreeSet::new();
    for secret in &release.secrets {
        validate_secret_property_path("secret.name", &secret.name)?;
        if !secrets.insert(secret.name.as_str()) {
            return Err(CompositionError::DuplicateRequirement {
                service: release.service_id.clone(),
                kind: "secret",
                name: secret.name.clone(),
            });
        }
    }
    Ok(())
}

fn validate_provider_candidates(providers: &[ProviderCandidateV1]) -> Result<()> {
    let mut identities = BTreeSet::new();
    for provider in providers {
        validate_identifier("provider.providerId", &provider.provider_id)?;
        validate_identifier("provider.capability", &provider.capability)?;
        if let Some(service_id) = &provider.service_id {
            validate_identifier("provider.serviceId", service_id)?;
        }
        if !identities.insert((provider.capability.as_str(), provider.provider_id.as_str())) {
            return Err(CompositionError::DuplicateProvider {
                provider_id: provider.provider_id.clone(),
                capability: provider.capability.clone(),
            });
        }
    }
    Ok(())
}

fn reachable_release_graph(
    graph: &ReleaseGraphV1,
    mode: CompositionModeV1,
) -> Result<Vec<CompositionReleaseV1>> {
    let by_id = graph
        .releases
        .iter()
        .map(|release| (release.service_id.as_str(), release))
        .collect::<BTreeMap<_, _>>();
    let mut pending = vec![graph.root_service_id.as_str()];
    let mut reachable = BTreeSet::new();
    while let Some(service_id) = pending.pop() {
        if !reachable.insert(service_id) {
            continue;
        }
        let release = by_id.get(service_id).copied().ok_or_else(|| {
            CompositionError::MissingPackageDependency {
                service: graph.root_service_id.clone(),
                dependency: service_id.to_string(),
            }
        })?;
        for dependency in &release.package_dependencies {
            if mode == CompositionModeV1::Production && dependency.development {
                continue;
            }
            let target = by_id
                .get(dependency.service_id.as_str())
                .copied()
                .ok_or_else(|| CompositionError::MissingPackageDependency {
                    service: release.service_id.clone(),
                    dependency: dependency.service_id.clone(),
                })?;
            let requirement = parse_requirement(
                "packageDependency.versionRequirement",
                &dependency.version_requirement,
            )?;
            if !requirement.matches(&target.version) {
                return Err(CompositionError::PackageVersionMismatch {
                    service: release.service_id.clone(),
                    dependency: dependency.service_id.clone(),
                    requirement: dependency.version_requirement.clone(),
                    actual: target.version.clone(),
                });
            }
            pending.push(&target.service_id);
        }
    }
    let mut releases = graph
        .releases
        .iter()
        .filter(|release| reachable.contains(release.service_id.as_str()))
        .cloned()
        .collect::<Vec<_>>();
    for release in &mut releases {
        if mode == CompositionModeV1::Production {
            release
                .package_dependencies
                .retain(|dependency| !dependency.development);
        }
        sort_release(release);
    }
    releases.sort_by(|left, right| left.service_id.cmp(&right.service_id));
    Ok(releases)
}

fn sort_release(release: &mut CompositionReleaseV1) {
    release.package_dependencies.sort_by(|left, right| {
        left.service_id
            .cmp(&right.service_id)
            .then(left.version_requirement.cmp(&right.version_requirement))
    });
    release
        .provided_apis
        .sort_by(|left, right| left.api_id.cmp(&right.api_id));
    release
        .required_apis
        .sort_by(|left, right| left.name.cmp(&right.name));
    release
        .resource_claims
        .sort_by(|left, right| left.name.cmp(&right.name));
    release
        .secrets
        .sort_by(|left, right| left.name.cmp(&right.name));
}

fn api_candidates(
    requirement: &ApiRequirementV1,
    releases: &[CompositionReleaseV1],
    providers: &[ProviderCandidateV1],
) -> Result<Vec<ProviderCandidateRefV1>> {
    let version_requirement = parse_requirement(
        "requiredApi.versionRequirement",
        &requirement.version_requirement,
    )?;
    let mut candidates = BTreeMap::new();
    for release in releases {
        for api in &release.provided_apis {
            if api.api_id != requirement.api_id || !version_requirement.matches(&api.version) {
                continue;
            }
            let candidate = ProviderCandidateRefV1 {
                provider_id: package_provider_id(&release.service_id),
                version: api.version.clone(),
                kind: ProviderKindV1::Package,
                service_id: Some(release.service_id.clone()),
            };
            candidates.insert(candidate.provider_id.clone(), candidate);
        }
    }
    for provider in providers {
        if provider.capability != requirement.api_id
            || !version_requirement.matches(&provider.version)
        {
            continue;
        }
        let candidate = candidate_ref(provider);
        match candidates.entry(candidate.provider_id.clone()) {
            std::collections::btree_map::Entry::Vacant(entry) => {
                entry.insert(candidate);
            }
            std::collections::btree_map::Entry::Occupied(entry) if entry.get() == &candidate => {}
            std::collections::btree_map::Entry::Occupied(_) => {
                return Err(CompositionError::ProviderIdentityConflict {
                    provider_id: candidate.provider_id,
                    capability: requirement.api_id.clone(),
                });
            }
        }
    }
    Ok(candidates.into_values().collect())
}

fn capability_candidates(
    capability: &str,
    version_requirement: &str,
    providers: &[ProviderCandidateV1],
) -> Result<Vec<ProviderCandidateRefV1>> {
    let requirement = parse_requirement("provider.versionRequirement", version_requirement)?;
    Ok(providers
        .iter()
        .filter(|provider| {
            provider.capability == capability && requirement.matches(&provider.version)
        })
        .map(candidate_ref)
        .collect())
}

fn candidate_ref(provider: &ProviderCandidateV1) -> ProviderCandidateRefV1 {
    ProviderCandidateRefV1 {
        provider_id: provider.provider_id.clone(),
        version: provider.version.clone(),
        kind: provider.kind,
        service_id: provider.service_id.clone(),
    }
}

fn select_provider(
    node_id: &str,
    capability: &str,
    version_requirement: &str,
    policy: ProviderPolicyV1,
    mut candidates: Vec<ProviderCandidateRefV1>,
) -> Result<ProviderSelectionV1> {
    candidates.sort();
    let selected_provider_id = match policy {
        ProviderPolicyV1::UniqueHealthy if candidates.len() == 1 => {
            Some(candidates[0].provider_id.clone())
        }
        ProviderPolicyV1::UniqueHealthy | ProviderPolicyV1::Explicit => None,
    };
    let provider = ProviderSelectionV1 {
        capability: capability.to_string(),
        version_requirement: version_requirement.to_string(),
        policy,
        candidates,
        selected_provider_id,
    };
    validate_provider_selection(node_id, &provider)?;
    Ok(provider)
}

fn provider_input(provider: &ProviderSelectionV1, required: bool) -> Vec<UnresolvedInputV1> {
    if provider.selected_provider_id.is_some() || (!required && provider.candidates.is_empty()) {
        return Vec::new();
    }
    vec![UnresolvedInputV1 {
        key: "providerId".to_string(),
        value_type: CompositionInputTypeV1::ProviderId,
        required,
        sensitive: false,
        allowed_values: provider
            .candidates
            .iter()
            .map(|candidate| candidate.provider_id.clone())
            .collect(),
    }]
}

fn selected_package_service<'a>(provider: &'a ProviderSelectionV1, id: &str) -> Option<&'a str> {
    provider
        .candidates
        .iter()
        .find(|candidate| candidate.provider_id == id)
        .filter(|candidate| candidate.kind == ProviderKindV1::Package)
        .and_then(|candidate| candidate.service_id.as_deref())
}

fn validate_plan_structure(plan: &CompositionPlanV1, validate_digests: bool) -> Result<()> {
    if plan.schema_version != COMPOSITION_PLAN_SCHEMA_VERSION {
        return Err(CompositionError::UnsupportedSchemaVersion {
            document: "composition plan",
            expected: COMPOSITION_PLAN_SCHEMA_VERSION,
            actual: plan.schema_version.clone(),
        });
    }
    validate_identifier("plan.rootServiceId", &plan.root_service_id)?;
    ensure_sorted_unique_by("releaseGraph", &plan.release_graph, |release| {
        release.service_id.as_str()
    })?;
    let release_ids = plan
        .release_graph
        .iter()
        .map(|release| release.service_id.as_str())
        .collect::<BTreeSet<_>>();
    let releases_by_service = plan
        .release_graph
        .iter()
        .map(|release| (release.service_id.as_str(), release))
        .collect::<BTreeMap<_, _>>();
    if !release_ids.contains(plan.root_service_id.as_str()) {
        return Err(CompositionError::MissingRootRelease(
            plan.root_service_id.clone(),
        ));
    }
    for release in &plan.release_graph {
        validate_release(release)?;
        ensure_release_sorted(release)?;
        for dependency in &release.package_dependencies {
            if plan.mode == CompositionModeV1::Production && dependency.development {
                return Err(CompositionError::InvalidProviderSelection {
                    node_id: package_node_id(release),
                    message: "production release graph contains a development dependency"
                        .to_string(),
                });
            }
            if !release_ids.contains(dependency.service_id.as_str()) {
                return Err(CompositionError::MissingPackageDependency {
                    service: release.service_id.clone(),
                    dependency: dependency.service_id.clone(),
                });
            }
        }
    }
    ensure_sorted_unique_by("nodes", &plan.nodes, |node| node.node_id.as_str())?;
    if plan.edges.windows(2).any(|pair| pair[0] >= pair[1]) {
        return Err(CompositionError::NonCanonicalOrder { field: "edges" });
    }
    let mut node_ids = BTreeSet::new();
    for node in &plan.nodes {
        validate_identifier("node.nodeId", &node.node_id)?;
        validate_identifier("node.serviceId", &node.service_id)?;
        if !node_ids.insert(node.node_id.as_str()) {
            return Err(CompositionError::DuplicateNode(node.node_id.clone()));
        }
        if !release_ids.contains(node.service_id.as_str()) {
            return Err(CompositionError::UnknownNodeService {
                node_id: node.node_id.clone(),
                service_id: node.service_id.clone(),
            });
        }
        validate_node(
            node,
            releases_by_service
                .get(node.service_id.as_str())
                .expect("release identity was checked above"),
        )?;
    }
    let mut edges = BTreeSet::new();
    for edge in &plan.edges {
        if edge.from == edge.to {
            return Err(CompositionError::SelfEdge(edge.from.clone()));
        }
        for endpoint in [&edge.from, &edge.to] {
            if !node_ids.contains(endpoint.as_str()) {
                return Err(CompositionError::UnknownEdgeNode(endpoint.clone()));
            }
        }
        if !edges.insert(edge) {
            return Err(CompositionError::DuplicateEdge {
                from: edge.from.clone(),
                to: edge.to.clone(),
                relationship: edge.relationship,
            });
        }
    }
    detect_cycle(&node_ids, &plan.edges)?;

    if validate_digests {
        validate_digest("plan.releaseGraphDigest", &plan.release_graph_digest)?;
        validate_digest("plan.planDigest", &plan.plan_digest)?;
        let release_graph_digest =
            compute_release_graph_digest(plan.mode, &plan.root_service_id, &plan.release_graph)?;
        if release_graph_digest != plan.release_graph_digest {
            return Err(CompositionError::ReleaseGraphDigestMismatch {
                expected: release_graph_digest,
                actual: plan.release_graph_digest.clone(),
            });
        }
        let plan_digest = compute_plan_digest(plan)?;
        if plan_digest != plan.plan_digest {
            return Err(CompositionError::PlanDigestMismatch {
                expected: plan_digest,
                actual: plan.plan_digest.clone(),
            });
        }
    }
    Ok(())
}

fn validate_node(node: &CompositionNodeV1, release: &CompositionReleaseV1) -> Result<()> {
    let expected_id = match &node.spec {
        CompositionNodeSpecV1::Package { release_digest, .. } => {
            validate_digest("package.releaseDigest", release_digest)?;
            if release.release_digest != *release_digest {
                return Err(CompositionError::InvalidDigest {
                    field: "package.releaseDigest",
                    value: release_digest.clone(),
                });
            }
            package_node_id(release)
        }
        CompositionNodeSpecV1::ApiBinding {
            name,
            api_id,
            version_requirement,
            provider,
            ..
        } => {
            validate_identifier("apiBinding.name", name)?;
            validate_identifier("apiBinding.apiId", api_id)?;
            parse_requirement("apiBinding.versionRequirement", version_requirement)?;
            if provider.capability != *api_id
                || provider.version_requirement != *version_requirement
            {
                return Err(CompositionError::InvalidProviderSelection {
                    node_id: node.node_id.clone(),
                    message: "provider capability/version does not match API requirement"
                        .to_string(),
                });
            }
            validate_provider_selection(&node.node_id, provider)?;
            api_node_id(release, name)
        }
        CompositionNodeSpecV1::ResourceClaim {
            name,
            resource_type,
            version_requirement,
            provider,
            ..
        } => {
            validate_identifier("resourceClaim.name", name)?;
            validate_identifier("resourceClaim.resourceType", resource_type)?;
            parse_requirement("resourceClaim.versionRequirement", version_requirement)?;
            if provider.capability != *resource_type
                || provider.version_requirement != *version_requirement
            {
                return Err(CompositionError::InvalidProviderSelection {
                    node_id: node.node_id.clone(),
                    message: "provider capability/version does not match resource requirement"
                        .to_string(),
                });
            }
            validate_provider_selection(&node.node_id, provider)?;
            resource_node_id(release, name)
        }
        CompositionNodeSpecV1::Config {
            schema,
            schema_digest,
            ..
        } => {
            if !schema.is_object() {
                return Err(CompositionError::InvalidObject {
                    field: "config.schema",
                });
            }
            let expected = digest_serializable(schema)?;
            if &expected != schema_digest {
                return Err(CompositionError::ReleaseGraphDigestMismatch {
                    expected,
                    actual: schema_digest.clone(),
                });
            }
            config_node_id(release)
        }
        CompositionNodeSpecV1::Secret { name, .. } => {
            validate_secret_property_path("secret.name", name)?;
            secret_node_id(release, name)
        }
    };
    if node.node_id != expected_id {
        return Err(CompositionError::InvalidIdentifier {
            field: "node.nodeId",
            value: node.node_id.clone(),
        });
    }
    if node
        .unresolved_inputs
        .windows(2)
        .any(|pair| pair[0].key >= pair[1].key)
    {
        return Err(CompositionError::NonCanonicalOrder {
            field: "node.unresolvedInputs",
        });
    }
    let mut keys = BTreeSet::new();
    for input in &node.unresolved_inputs {
        if !keys.insert(input.key.as_str()) {
            return Err(CompositionError::InvalidInputDeclaration {
                node_id: node.node_id.clone(),
                key: input.key.clone(),
                message: "duplicate key".to_string(),
            });
        }
        let expected_key = match input.value_type {
            CompositionInputTypeV1::ProviderId => "providerId",
            CompositionInputTypeV1::JsonObject => "config",
            CompositionInputTypeV1::SecretRef => "secretRef",
        };
        if input.key != expected_key {
            return Err(CompositionError::InvalidInputDeclaration {
                node_id: node.node_id.clone(),
                key: input.key.clone(),
                message: format!("input type requires key {expected_key}"),
            });
        }
        if input.value_type == CompositionInputTypeV1::SecretRef && !input.sensitive {
            return Err(CompositionError::InvalidInputDeclaration {
                node_id: node.node_id.clone(),
                key: input.key.clone(),
                message: "secret references must be marked sensitive".to_string(),
            });
        }
        if input
            .allowed_values
            .windows(2)
            .any(|pair| pair[0] >= pair[1])
        {
            return Err(CompositionError::NonCanonicalOrder {
                field: "input.allowedValues",
            });
        }
    }
    validate_node_input_shape(node)
}

fn validate_node_input_shape(node: &CompositionNodeV1) -> Result<()> {
    let expected = match &node.spec {
        CompositionNodeSpecV1::Package { .. } => Vec::new(),
        CompositionNodeSpecV1::ApiBinding {
            optional, provider, ..
        }
        | CompositionNodeSpecV1::ResourceClaim {
            optional, provider, ..
        } => provider_input(provider, !*optional),
        CompositionNodeSpecV1::Config { required, .. } => vec![UnresolvedInputV1 {
            key: "config".to_string(),
            value_type: CompositionInputTypeV1::JsonObject,
            required: *required,
            sensitive: false,
            allowed_values: Vec::new(),
        }],
        CompositionNodeSpecV1::Secret { required, .. } => vec![UnresolvedInputV1 {
            key: "secretRef".to_string(),
            value_type: CompositionInputTypeV1::SecretRef,
            required: *required,
            sensitive: true,
            allowed_values: Vec::new(),
        }],
    };
    if node.unresolved_inputs != expected {
        return Err(CompositionError::InvalidInputDeclaration {
            node_id: node.node_id.clone(),
            key: "*".to_string(),
            message: "declarations do not match the typed node requirement".to_string(),
        });
    }
    Ok(())
}

fn validate_provider_selection(node_id: &str, provider: &ProviderSelectionV1) -> Result<()> {
    validate_identifier("provider.capability", &provider.capability)?;
    let requirement =
        parse_requirement("provider.versionRequirement", &provider.version_requirement)?;
    if provider
        .candidates
        .windows(2)
        .any(|pair| pair[0] >= pair[1])
    {
        return Err(CompositionError::NonCanonicalOrder {
            field: "provider.candidates",
        });
    }
    let mut ids = BTreeSet::new();
    for candidate in &provider.candidates {
        validate_identifier("providerCandidate.providerId", &candidate.provider_id)?;
        if let Some(service_id) = &candidate.service_id {
            validate_identifier("providerCandidate.serviceId", service_id)?;
        }
        if !ids.insert(candidate.provider_id.as_str()) {
            return Err(CompositionError::InvalidProviderSelection {
                node_id: node_id.to_string(),
                message: format!("duplicate candidate {}", candidate.provider_id),
            });
        }
        if !requirement.matches(&candidate.version) {
            return Err(CompositionError::InvalidProviderSelection {
                node_id: node_id.to_string(),
                message: format!(
                    "candidate {} version {} does not satisfy {}",
                    candidate.provider_id, candidate.version, provider.version_requirement
                ),
            });
        }
        if candidate.kind == ProviderKindV1::Package && candidate.service_id.is_none() {
            return Err(CompositionError::InvalidProviderSelection {
                node_id: node_id.to_string(),
                message: "package candidate requires serviceId".to_string(),
            });
        }
    }
    if let Some(selected) = &provider.selected_provider_id {
        if provider.policy == ProviderPolicyV1::Explicit {
            return Err(CompositionError::InvalidProviderSelection {
                node_id: node_id.to_string(),
                message: "explicit policy cannot preselect a provider".to_string(),
            });
        }
        if provider.candidates.len() != 1 || !ids.contains(selected.as_str()) {
            return Err(CompositionError::InvalidProviderSelection {
                node_id: node_id.to_string(),
                message: "preselection requires exactly one matching candidate".to_string(),
            });
        }
    }
    Ok(())
}

fn ensure_release_sorted(release: &CompositionReleaseV1) -> Result<()> {
    ensure_sorted_unique_by(
        "release.packageDependencies",
        &release.package_dependencies,
        |value| value.service_id.as_str(),
    )?;
    ensure_sorted_unique_by("release.providedApis", &release.provided_apis, |value| {
        value.api_id.as_str()
    })?;
    ensure_sorted_unique_by("release.requiredApis", &release.required_apis, |value| {
        value.name.as_str()
    })?;
    ensure_sorted_unique_by(
        "release.resourceClaims",
        &release.resource_claims,
        |value| value.name.as_str(),
    )?;
    ensure_sorted_unique_by("release.secrets", &release.secrets, |value| {
        value.name.as_str()
    })
}

fn ensure_sorted_unique_by<T, F>(field: &'static str, values: &[T], key: F) -> Result<()>
where
    F: Fn(&T) -> &str,
{
    if values.windows(2).any(|pair| key(&pair[0]) >= key(&pair[1])) {
        return Err(CompositionError::NonCanonicalOrder { field });
    }
    Ok(())
}

fn detect_cycle(node_ids: &BTreeSet<&str>, edges: &[CompositionEdgeV1]) -> Result<()> {
    let mut adjacency = node_ids
        .iter()
        .map(|node| (*node, Vec::new()))
        .collect::<BTreeMap<_, Vec<&str>>>();
    for edge in edges {
        adjacency
            .get_mut(edge.from.as_str())
            .expect("edge endpoints were validated")
            .push(edge.to.as_str());
    }
    for targets in adjacency.values_mut() {
        targets.sort();
        targets.dedup();
    }
    let mut states = BTreeMap::<&str, u8>::new();
    let mut stack = Vec::new();
    for node in node_ids {
        if states.get(node).copied().unwrap_or_default() == 0 {
            visit_cycle(node, &adjacency, &mut states, &mut stack)?;
        }
    }
    Ok(())
}

fn visit_cycle<'a>(
    node: &'a str,
    adjacency: &BTreeMap<&'a str, Vec<&'a str>>,
    states: &mut BTreeMap<&'a str, u8>,
    stack: &mut Vec<&'a str>,
) -> Result<()> {
    states.insert(node, 1);
    stack.push(node);
    for target in adjacency.get(node).into_iter().flatten() {
        match states.get(target).copied().unwrap_or_default() {
            0 => visit_cycle(target, adjacency, states, stack)?,
            1 => {
                let start = stack
                    .iter()
                    .position(|candidate| candidate == target)
                    .unwrap_or(0);
                let mut cycle = stack[start..].to_vec();
                cycle.push(target);
                return Err(CompositionError::DependencyCycle {
                    cycle: cycle.join(" -> "),
                });
            }
            _ => {}
        }
    }
    stack.pop();
    states.insert(node, 2);
    Ok(())
}

type InputsByNode = BTreeMap<String, BTreeMap<String, Value>>;

fn normalize_legacy_root_aliases(
    plan: &CompositionPlanV1,
    request: &InstallInputsV1,
) -> Result<(InputsByNode, bool)> {
    let mut inputs = request.inputs.clone();
    let mut normalized = false;
    let root_release = plan
        .release_graph
        .iter()
        .find(|release| release.service_id == plan.root_service_id)
        .ok_or_else(|| CompositionError::MissingRootRelease(plan.root_service_id.clone()))?;
    if let Some(config) = &request.config {
        normalized = true;
        let node_id = config_node_id(root_release);
        if !plan.nodes.iter().any(|node| node.node_id == node_id) {
            return Err(CompositionError::UnsupportedLegacyAlias {
                alias: "config".to_string(),
            });
        }
        insert_legacy_alias(&mut inputs, "config", &node_id, "config", config.clone())?;
    }
    for (name, secret_ref) in &request.secret_refs {
        normalized = true;
        let node_id = secret_node_id(root_release, name);
        if !plan.nodes.iter().any(|node| {
            node.node_id == node_id
                && matches!(&node.spec, CompositionNodeSpecV1::Secret { name: node_name, .. } if node_name == name)
        }) {
            return Err(CompositionError::UnsupportedLegacyAlias {
                alias: format!("secretRefs.{name}"),
            });
        }
        insert_legacy_alias(
            &mut inputs,
            &format!("secretRefs.{name}"),
            &node_id,
            "secretRef",
            Value::String(secret_ref.clone()),
        )?;
    }
    Ok((inputs, normalized))
}

fn insert_legacy_alias(
    inputs: &mut BTreeMap<String, BTreeMap<String, Value>>,
    alias: &str,
    node_id: &str,
    key: &str,
    value: Value,
) -> Result<()> {
    let node_inputs = inputs.entry(node_id.to_string()).or_default();
    if node_inputs.contains_key(key) {
        return Err(CompositionError::LegacyAliasConflict {
            alias: alias.to_string(),
            node_id: node_id.to_string(),
            key: key.to_string(),
        });
    }
    node_inputs.insert(key.to_string(), value);
    Ok(())
}

fn validate_input_value(
    node_id: &str,
    declaration: &UnresolvedInputV1,
    value: &Value,
) -> Result<()> {
    let invalid = |message: String| CompositionError::InvalidNodeInput {
        node_id: node_id.to_string(),
        key: declaration.key.clone(),
        message,
    };
    match declaration.value_type {
        CompositionInputTypeV1::ProviderId => {
            let provider_id = value
                .as_str()
                .ok_or_else(|| invalid("must be a provider identifier string".to_string()))?;
            validate_identifier("input.providerId", provider_id)
                .map_err(|error| invalid(error.to_string()))?;
            if declaration.allowed_values.is_empty() {
                return Err(invalid(
                    "plan contains no eligible providers; revalidate after registering one"
                        .to_string(),
                ));
            }
            if !declaration
                .allowed_values
                .iter()
                .any(|candidate| candidate == provider_id)
            {
                return Err(invalid(format!(
                    "provider {provider_id} is not an eligible candidate"
                )));
            }
        }
        CompositionInputTypeV1::JsonObject => {
            if !value.is_object() {
                return Err(invalid("must be a JSON object".to_string()));
            }
        }
        CompositionInputTypeV1::SecretRef => {
            let reference = value
                .as_str()
                .ok_or_else(|| invalid("must be a secret reference string".to_string()))?;
            if !valid_secret_ref(reference) {
                return Err(invalid(
                    "must be an opaque URI-like reference, never plaintext".to_string(),
                ));
            }
        }
    }
    Ok(())
}

fn node_provider(spec: &CompositionNodeSpecV1) -> Option<&ProviderSelectionV1> {
    match spec {
        CompositionNodeSpecV1::ApiBinding { provider, .. }
        | CompositionNodeSpecV1::ResourceClaim { provider, .. } => Some(provider),
        _ => None,
    }
}

fn parse_requirement(field: &'static str, value: &str) -> Result<VersionReq> {
    VersionReq::parse(value).map_err(|error| CompositionError::InvalidVersionRequirement {
        field,
        value: value.to_string(),
        message: error.to_string(),
    })
}

fn validate_identifier(field: &'static str, value: &str) -> Result<()> {
    if value.is_empty()
        || value.len() > 160
        || !value
            .bytes()
            .next()
            .is_some_and(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
        || !value
            .bytes()
            .last()
            .is_some_and(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
        || !value.bytes().all(|byte| {
            byte.is_ascii_lowercase()
                || byte.is_ascii_digit()
                || matches!(byte, b'-' | b'_' | b'.' | b':')
        })
    {
        return Err(CompositionError::InvalidIdentifier {
            field,
            value: value.to_string(),
        });
    }
    Ok(())
}

/// Secret names originate in JSON Schema property paths, not in the platform
/// identifier namespace. Preserve the signed JSON spelling (including
/// camelCase and non-ASCII property names) while rejecting paths that cannot
/// be addressed unambiguously by Store's dotted-path materialization.
fn validate_secret_property_path(field: &'static str, value: &str) -> Result<()> {
    const MAX_PATH_BYTES: usize = 512;
    const MAX_SEGMENT_BYTES: usize = 128;

    let valid = !value.is_empty()
        && value.len() <= MAX_PATH_BYTES
        && !value.bytes().any(|byte| byte.is_ascii_control())
        && value
            .split('.')
            .all(|segment| !segment.is_empty() && segment.len() <= MAX_SEGMENT_BYTES);
    if !valid {
        return Err(CompositionError::InvalidIdentifier {
            field,
            value: value.to_string(),
        });
    }
    Ok(())
}

fn validate_digest(field: &'static str, value: &str) -> Result<()> {
    let valid = value.strip_prefix("sha256:").is_some_and(|hex| {
        hex.len() == 64
            && hex
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    });
    if !valid {
        return Err(CompositionError::InvalidDigest {
            field,
            value: value.to_string(),
        });
    }
    Ok(())
}

fn valid_secret_ref(value: &str) -> bool {
    if value.is_empty()
        || value.len() > 2048
        || value
            .bytes()
            .any(|byte| byte.is_ascii_whitespace() || byte.is_ascii_control())
    {
        return false;
    }
    let Some((scheme, target)) = value.split_once("://") else {
        return false;
    };
    !target.is_empty()
        && !scheme.is_empty()
        && scheme.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'+' | b'-' | b'.')
        })
}

fn composition_node_id(release: &CompositionReleaseV1, kind: &str, requirement: &str) -> String {
    let material = format!(
        "{}\0{}\0{}\0{}\0{}",
        release.release_digest, release.owner_instance_id, release.service_id, kind, requirement
    );
    let digest = Sha256::digest(material.as_bytes());
    format!("{kind}:{}:{digest:x}", release.service_id)
}

fn package_node_id(release: &CompositionReleaseV1) -> String {
    composition_node_id(release, "package", "root")
}

fn package_provider_id(service_id: &str) -> String {
    format!("package:{service_id}")
}

fn api_node_id(release: &CompositionReleaseV1, name: &str) -> String {
    composition_node_id(release, "api", name)
}

fn resource_node_id(release: &CompositionReleaseV1, name: &str) -> String {
    composition_node_id(release, "resource", name)
}

fn config_node_id(release: &CompositionReleaseV1) -> String {
    composition_node_id(release, "config", "root")
}

fn secret_node_id(release: &CompositionReleaseV1, name: &str) -> String {
    composition_node_id(release, "secret", name)
}

fn default_any_version() -> String {
    "*".to_string()
}

fn default_true() -> bool {
    true
}

fn digest_serializable<T: Serialize + ?Sized>(value: &T) -> Result<String> {
    let value = serde_json::to_value(value)
        .map_err(|error| CompositionError::Serialization(error.to_string()))?;
    let canonical = canonicalize_json(value);
    let bytes = serde_json::to_vec(&canonical)
        .map_err(|error| CompositionError::Serialization(error.to_string()))?;
    Ok(format!("sha256:{:x}", Sha256::digest(bytes)))
}

fn canonicalize_json(value: Value) -> Value {
    match value {
        Value::Array(values) => Value::Array(values.into_iter().map(canonicalize_json).collect()),
        Value::Object(values) => {
            let mut entries = values.into_iter().collect::<Vec<_>>();
            entries.sort_by(|left, right| left.0.cmp(&right.0));
            let mut object = Map::new();
            for (key, value) in entries {
                object.insert(key, canonicalize_json(value));
            }
            Value::Object(object)
        }
        scalar => scalar,
    }
}
