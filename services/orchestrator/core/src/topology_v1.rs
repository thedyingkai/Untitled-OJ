use crate::{OrchestratorError, Result, parse_endpoint_id, validate_endpoint_id};
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};

pub const TOPOLOGY_SPEC_VERSION: &str = "v1";

const MAX_TOPOLOGY_ID_LEN: usize = 63;
const MAX_TEXT_LEN: usize = 512;
const MAX_JSON_BYTES: usize = 64 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct TopologyAuthoritySpec {
    pub root_endpoint: String,
    pub exposure_policy: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct TopologyEndpointSpec {
    pub endpoint: String,
    pub service_id: String,
    pub protocol: String,
    #[serde(default)]
    pub health_path: String,
    #[serde(default)]
    pub display_name: String,
    #[serde(default)]
    pub note: String,
    #[serde(default)]
    pub config: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct TopologyLinkSpec {
    pub source_endpoint: String,
    pub target_endpoint: String,
    pub protocol: String,
    pub auth_mode: String,
    pub scope: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub config_ref: String,
    #[serde(default)]
    pub secret_ref: String,
    #[serde(default)]
    pub policy: Value,
    /// API-level bindings carried by this consumer -> provider Link. Empty is
    /// the exact v1 meaning and keeps historical revisions byte-compatible.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub api_bindings: Vec<TopologyApiBindingSpec>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct TopologyApiBindingSpec {
    /// Stable consumer-local binding name, for example `STORAGE_GET`.
    #[serde(rename = "requirement", alias = "name")]
    pub requirement_name: String,
    pub api_id: String,
    #[serde(default)]
    pub version: String,
    #[serde(default)]
    pub optional: bool,
    #[serde(default)]
    pub provider_deployment_id: String,
    #[serde(default = "default_api_binding_selection")]
    pub selection: String,
}

fn default_api_binding_selection() -> String {
    "nearest-healthy".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct TopologySpec {
    pub api_version: String,
    pub topology_id: String,
    pub root_endpoint: String,
    pub authority: TopologyAuthoritySpec,
    #[serde(default)]
    pub endpoints: Vec<TopologyEndpointSpec>,
    #[serde(default)]
    pub links: Vec<TopologyLinkSpec>,
}

impl TopologySpec {
    pub fn new(
        topology_id: impl Into<String>,
        root_endpoint: impl Into<String>,
        exposure_policy: impl Into<String>,
        endpoints: Vec<TopologyEndpointSpec>,
        links: Vec<TopologyLinkSpec>,
    ) -> Result<Self> {
        let root_endpoint = root_endpoint.into();
        Self {
            api_version: TOPOLOGY_SPEC_VERSION.to_string(),
            topology_id: topology_id.into(),
            authority: TopologyAuthoritySpec {
                root_endpoint: root_endpoint.clone(),
                exposure_policy: exposure_policy.into(),
            },
            root_endpoint,
            endpoints,
            links,
        }
        .canonicalized()
    }

    pub fn validate(&self) -> Result<()> {
        validate_topology_id(&self.topology_id)?;
        if self.api_version != TOPOLOGY_SPEC_VERSION {
            return invalid(format!(
                "topology api_version must be {TOPOLOGY_SPEC_VERSION}"
            ));
        }
        validate_endpoint_id(&self.root_endpoint)?;
        if self.authority.root_endpoint != self.root_endpoint {
            return invalid("authority root_endpoint must match topology root_endpoint");
        }
        validate_token(
            "authority exposure_policy",
            &self.authority.exposure_policy,
            MAX_TEXT_LEN,
        )?;

        let mut endpoint_ids = BTreeSet::new();
        for endpoint in &self.endpoints {
            validate_endpoint_spec(endpoint)?;
            if !endpoint_ids.insert(endpoint.endpoint.as_str()) {
                return invalid(format!("duplicate endpoint {}", endpoint.endpoint));
            }
        }
        if !endpoint_ids.contains(self.root_endpoint.as_str()) {
            return invalid("topology root_endpoint must be present in endpoints");
        }

        let mut link_ids = BTreeSet::new();
        let mut consumer_requirements = BTreeSet::new();
        for link in &self.links {
            validate_link_spec(link, &endpoint_ids)?;
            let key = (link.source_endpoint.as_str(), link.target_endpoint.as_str());
            if !link_ids.insert(key) {
                return invalid(format!(
                    "duplicate link {} -> {}",
                    link.source_endpoint, link.target_endpoint
                ));
            }
            if link.enabled {
                for binding in &link.api_bindings {
                    if !consumer_requirements.insert((
                        link.source_endpoint.as_str(),
                        binding.requirement_name.as_str(),
                    )) {
                        return invalid(format!(
                            "consumer endpoint {} binds requirement {} more than once",
                            link.source_endpoint, binding.requirement_name
                        ));
                    }
                }
            }
        }
        Ok(())
    }

    pub fn validate_against_registered_services(
        &self,
        registered_services: &BTreeSet<String>,
    ) -> Result<()> {
        self.validate()?;
        for endpoint in &self.endpoints {
            if !registered_services.contains(&endpoint.service_id) {
                return invalid(format!(
                    "endpoint {} references unregistered service {}",
                    endpoint.endpoint, endpoint.service_id
                ));
            }
        }
        Ok(())
    }

    pub fn canonicalized(&self) -> Result<Self> {
        self.validate()?;
        let mut canonical = self.clone();
        canonical
            .endpoints
            .sort_by(|left, right| left.endpoint.cmp(&right.endpoint));
        for link in &mut canonical.links {
            link.api_bindings.sort_by(|left, right| {
                (
                    &left.requirement_name,
                    &left.api_id,
                    &left.version,
                    left.optional,
                    &left.provider_deployment_id,
                    &left.selection,
                )
                    .cmp(&(
                        &right.requirement_name,
                        &right.api_id,
                        &right.version,
                        right.optional,
                        &right.provider_deployment_id,
                        &right.selection,
                    ))
            });
        }
        canonical.links.sort_by(|left, right| {
            (&left.source_endpoint, &left.target_endpoint)
                .cmp(&(&right.source_endpoint, &right.target_endpoint))
        });
        Ok(canonical)
    }

    pub fn content_sha256(&self) -> Result<String> {
        let canonical = self.canonicalized()?;
        let bytes = serde_json::to_vec(&canonical)?;
        Ok(hex_sha256(&bytes))
    }
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct TopologyRevision {
    topology_id: String,
    revision_number: u64,
    revision_id: String,
    parent_revision_id: Option<String>,
    rollback_of_revision_id: Option<String>,
    content_sha256: String,
    spec: TopologySpec,
    created_at: String,
    created_by: String,
    message: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TopologyRevisionWire {
    topology_id: String,
    revision_number: u64,
    revision_id: String,
    parent_revision_id: Option<String>,
    rollback_of_revision_id: Option<String>,
    content_sha256: String,
    spec: TopologySpec,
    created_at: String,
    created_by: String,
    message: String,
}

impl<'de> Deserialize<'de> for TopologyRevision {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        use serde::de::Error;

        let wire = TopologyRevisionWire::deserialize(deserializer)?;
        let revision = Self {
            topology_id: wire.topology_id,
            revision_number: wire.revision_number,
            revision_id: wire.revision_id,
            parent_revision_id: wire.parent_revision_id,
            rollback_of_revision_id: wire.rollback_of_revision_id,
            content_sha256: wire.content_sha256,
            spec: wire.spec,
            created_at: wire.created_at,
            created_by: wire.created_by,
            message: wire.message,
        };
        revision.verify().map_err(D::Error::custom)?;
        Ok(revision)
    }
}

impl TopologyRevision {
    pub fn initial(
        spec: TopologySpec,
        created_at: impl Into<String>,
        created_by: impl Into<String>,
        message: impl Into<String>,
    ) -> Result<Self> {
        Self::from_parts(1, None, None, spec, created_at, created_by, message)
    }

    pub fn from_parts(
        revision_number: u64,
        parent_revision_id: Option<String>,
        rollback_of_revision_id: Option<String>,
        spec: TopologySpec,
        created_at: impl Into<String>,
        created_by: impl Into<String>,
        message: impl Into<String>,
    ) -> Result<Self> {
        let spec = spec.canonicalized()?;
        validate_revision_lineage(
            revision_number,
            parent_revision_id.as_deref(),
            rollback_of_revision_id.as_deref(),
        )?;
        let created_at = created_at.into();
        let created_by = created_by.into();
        let message = message.into();
        validate_required_text("revision created_at", &created_at, MAX_TEXT_LEN)?;
        validate_required_text("revision created_by", &created_by, MAX_TEXT_LEN)?;
        validate_optional_text("revision message", &message, MAX_TEXT_LEN)?;
        let content_sha256 = spec.content_sha256()?;
        let revision_id = revision_id(&spec.topology_id, revision_number, &content_sha256);
        let revision = Self {
            topology_id: spec.topology_id.clone(),
            revision_number,
            revision_id,
            parent_revision_id,
            rollback_of_revision_id,
            content_sha256,
            spec,
            created_at,
            created_by,
            message,
        };
        revision.verify()?;
        Ok(revision)
    }

    pub fn next(
        &self,
        spec: TopologySpec,
        created_at: impl Into<String>,
        created_by: impl Into<String>,
        message: impl Into<String>,
    ) -> Result<Self> {
        self.verify()?;
        if spec.topology_id != self.topology_id {
            return invalid("next revision must belong to the same topology");
        }
        let next_number = self
            .revision_number
            .checked_add(1)
            .ok_or_else(|| OrchestratorError::Blocked("revision number overflow".to_string()))?;
        let next = Self::from_parts(
            next_number,
            Some(self.revision_id.clone()),
            None,
            spec,
            created_at,
            created_by,
            message,
        )?;
        if next.content_sha256 == self.content_sha256 {
            return invalid("next revision must change the topology spec");
        }
        Ok(next)
    }

    pub fn rollback_to(
        &self,
        target: &Self,
        created_at: impl Into<String>,
        created_by: impl Into<String>,
        message: impl Into<String>,
    ) -> Result<Self> {
        self.verify()?;
        target.verify()?;
        if target.topology_id != self.topology_id {
            return invalid("rollback target must belong to the same topology");
        }
        if target.revision_number >= self.revision_number {
            return invalid("rollback target must be older than the current revision");
        }
        if target.content_sha256 == self.content_sha256 {
            return invalid("rollback target must differ from the current topology spec");
        }
        let next_number = self
            .revision_number
            .checked_add(1)
            .ok_or_else(|| OrchestratorError::Blocked("revision number overflow".to_string()))?;
        Self::from_parts(
            next_number,
            Some(self.revision_id.clone()),
            Some(target.revision_id.clone()),
            target.spec.clone(),
            created_at,
            created_by,
            message,
        )
    }

    pub fn verify(&self) -> Result<()> {
        self.spec.validate()?;
        if self.spec.topology_id != self.topology_id {
            return invalid("revision topology_id must match spec topology_id");
        }
        if self.spec != self.spec.canonicalized()? {
            return invalid("revision spec must use canonical endpoint and link ordering");
        }
        validate_revision_lineage(
            self.revision_number,
            self.parent_revision_id.as_deref(),
            self.rollback_of_revision_id.as_deref(),
        )?;
        validate_required_text("revision created_at", &self.created_at, MAX_TEXT_LEN)?;
        validate_required_text("revision created_by", &self.created_by, MAX_TEXT_LEN)?;
        validate_optional_text("revision message", &self.message, MAX_TEXT_LEN)?;
        let expected_sha256 = self.spec.content_sha256()?;
        if self.content_sha256 != expected_sha256 {
            return invalid("revision content_sha256 does not match spec");
        }
        if self.revision_id
            != revision_id(
                &self.topology_id,
                self.revision_number,
                &self.content_sha256,
            )
        {
            return invalid("revision_id does not match topology, number, and content");
        }
        Ok(())
    }

    pub fn topology_id(&self) -> &str {
        &self.topology_id
    }

    pub fn revision_number(&self) -> u64 {
        self.revision_number
    }

    pub fn revision_id(&self) -> &str {
        &self.revision_id
    }

    pub fn parent_revision_id(&self) -> Option<&str> {
        self.parent_revision_id.as_deref()
    }

    pub fn rollback_of_revision_id(&self) -> Option<&str> {
        self.rollback_of_revision_id.as_deref()
    }

    pub fn content_sha256(&self) -> &str {
        &self.content_sha256
    }

    pub fn spec(&self) -> &TopologySpec {
        &self.spec
    }

    pub fn created_at(&self) -> &str {
        &self.created_at
    }

    pub fn created_by(&self) -> &str {
        &self.created_by
    }

    pub fn message(&self) -> &str {
        &self.message
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TopologyChange {
    RootEndpointUpdated {
        before: String,
        after: String,
    },
    AuthorityUpdated {
        before: TopologyAuthoritySpec,
        after: TopologyAuthoritySpec,
    },
    EndpointAdded {
        endpoint: TopologyEndpointSpec,
    },
    EndpointUpdated {
        before: TopologyEndpointSpec,
        after: TopologyEndpointSpec,
    },
    EndpointRemoved {
        endpoint: TopologyEndpointSpec,
    },
    LinkAdded {
        link: TopologyLinkSpec,
    },
    LinkUpdated {
        before: TopologyLinkSpec,
        after: TopologyLinkSpec,
    },
    LinkRemoved {
        link: TopologyLinkSpec,
    },
}

impl TopologyChange {
    pub fn resource_key(&self) -> String {
        match self {
            Self::RootEndpointUpdated { .. } => "topology/root".to_string(),
            Self::AuthorityUpdated { .. } => "topology/authority".to_string(),
            Self::EndpointAdded { endpoint }
            | Self::EndpointRemoved { endpoint }
            | Self::EndpointUpdated {
                after: endpoint, ..
            } => format!("endpoint/{}", endpoint.endpoint),
            Self::LinkAdded { link }
            | Self::LinkRemoved { link }
            | Self::LinkUpdated { after: link, .. } => {
                format!("link/{}->{}", link.source_endpoint, link.target_endpoint)
            }
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct TopologyDiff {
    pub topology_id: String,
    pub from_revision_id: Option<String>,
    pub to_revision_id: Option<String>,
    pub from_sha256: Option<String>,
    pub to_sha256: String,
    pub changes: Vec<TopologyChange>,
}

impl TopologyDiff {
    pub fn between_specs(from: Option<&TopologySpec>, to: &TopologySpec) -> Result<Self> {
        to.validate()?;
        if let Some(from) = from {
            from.validate()?;
            if from.topology_id != to.topology_id {
                return invalid("cannot diff specs from different topologies");
            }
        }
        Ok(Self {
            topology_id: to.topology_id.clone(),
            from_revision_id: None,
            to_revision_id: None,
            from_sha256: from.map(TopologySpec::content_sha256).transpose()?,
            to_sha256: to.content_sha256()?,
            changes: diff_changes(from, to),
        })
    }

    pub fn between_revisions(
        from: Option<&TopologyRevision>,
        to: &TopologyRevision,
    ) -> Result<Self> {
        to.verify()?;
        if let Some(from) = from {
            from.verify()?;
            if from.topology_id != to.topology_id {
                return invalid("cannot diff revisions from different topologies");
            }
        }
        Ok(Self {
            topology_id: to.topology_id.clone(),
            from_revision_id: from.map(|revision| revision.revision_id.clone()),
            to_revision_id: Some(to.revision_id.clone()),
            from_sha256: from.map(|revision| revision.content_sha256.clone()),
            to_sha256: to.content_sha256.clone(),
            changes: diff_changes(from.map(TopologyRevision::spec), to.spec()),
        })
    }

    pub fn is_empty(&self) -> bool {
        self.changes.is_empty()
    }
}

pub fn diff_topology_specs(from: Option<&TopologySpec>, to: &TopologySpec) -> Result<TopologyDiff> {
    TopologyDiff::between_specs(from, to)
}

pub fn diff_topology_revisions(
    from: Option<&TopologyRevision>,
    to: &TopologyRevision,
) -> Result<TopologyDiff> {
    TopologyDiff::between_revisions(from, to)
}

pub fn rollback_topology_revision(
    current: &TopologyRevision,
    target: &TopologyRevision,
    created_at: impl Into<String>,
    created_by: impl Into<String>,
    message: impl Into<String>,
) -> Result<TopologyRevision> {
    current.rollback_to(target, created_at, created_by, message)
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum TopologyReconciliationState {
    Draft,
    Reconciling,
    InSync,
    Degraded,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum TopologyHealth {
    Unknown,
    Healthy,
    Unhealthy,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum TopologyDesiredDeploymentState {
    Running,
    Stopped,
    Absent,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum TopologyObservedDeploymentState {
    Pending,
    Running,
    Stopped,
    Failed,
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum TopologyDriftKind {
    Missing,
    Unexpected,
    Changed,
    Unreachable,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum TopologyResourceKind {
    Authority,
    Deployment,
    Endpoint,
    Link,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct TopologyDeploymentStatus {
    pub deployment_id: String,
    pub service_id: String,
    pub node_id: String,
    pub desired_state: TopologyDesiredDeploymentState,
    pub observed_state: TopologyObservedDeploymentState,
    pub health: TopologyHealth,
    pub desired_generation: u64,
    pub observed_generation: u64,
    #[serde(default)]
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct TopologyEndpointStatus {
    pub endpoint: String,
    pub health: TopologyHealth,
    pub reachable: bool,
    #[serde(default)]
    pub latency_ms: Option<u64>,
    #[serde(default)]
    pub message: String,
    pub observed_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct TopologyLinkStatus {
    pub source_endpoint: String,
    pub target_endpoint: String,
    pub health: TopologyHealth,
    #[serde(default)]
    pub latency_ms: Option<u64>,
    #[serde(default)]
    pub message: String,
    pub observed_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct TopologyDrift {
    pub resource_kind: TopologyResourceKind,
    pub resource_id: String,
    pub kind: TopologyDriftKind,
    pub detail: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct TopologyStatus {
    pub topology_id: String,
    #[serde(default)]
    pub desired_revision_id: Option<String>,
    #[serde(default)]
    pub observed_revision_id: Option<String>,
    pub state: TopologyReconciliationState,
    #[serde(default)]
    pub deployments: Vec<TopologyDeploymentStatus>,
    #[serde(default)]
    pub endpoints: Vec<TopologyEndpointStatus>,
    #[serde(default)]
    pub links: Vec<TopologyLinkStatus>,
    #[serde(default)]
    pub drift: Vec<TopologyDrift>,
    #[serde(default)]
    pub last_operation_id: Option<String>,
    pub updated_at: String,
}

impl TopologyStatus {
    pub fn draft(
        topology_id: impl Into<String>,
        desired_revision_id: Option<String>,
        updated_at: impl Into<String>,
    ) -> Result<Self> {
        let status = Self {
            topology_id: topology_id.into(),
            desired_revision_id,
            observed_revision_id: None,
            state: TopologyReconciliationState::Draft,
            deployments: Vec::new(),
            endpoints: Vec::new(),
            links: Vec::new(),
            drift: Vec::new(),
            last_operation_id: None,
            updated_at: updated_at.into(),
        };
        status.validate()?;
        Ok(status)
    }

    pub fn validate(&self) -> Result<()> {
        validate_topology_id(&self.topology_id)?;
        validate_optional_identifier("desired_revision_id", self.desired_revision_id.as_deref())?;
        validate_optional_identifier("observed_revision_id", self.observed_revision_id.as_deref())?;
        validate_optional_identifier("last_operation_id", self.last_operation_id.as_deref())?;
        validate_required_text("status updated_at", &self.updated_at, MAX_TEXT_LEN)?;

        if self.state == TopologyReconciliationState::InSync {
            if self.desired_revision_id.is_none()
                || self.desired_revision_id != self.observed_revision_id
            {
                return invalid(
                    "IN_SYNC topology status requires matching desired and observed revisions",
                );
            }
            if !self.drift.is_empty() {
                return invalid("IN_SYNC topology status cannot contain drift");
            }
        }

        let mut deployment_ids = BTreeSet::new();
        for deployment in &self.deployments {
            validate_required_text("deployment_id", &deployment.deployment_id, MAX_TEXT_LEN)?;
            validate_service_id(&deployment.service_id)?;
            validate_required_text("node_id", &deployment.node_id, MAX_TEXT_LEN)?;
            validate_optional_text("deployment message", &deployment.message, MAX_TEXT_LEN)?;
            if !deployment_ids.insert(deployment.deployment_id.as_str()) {
                return invalid(format!(
                    "duplicate deployment status {}",
                    deployment.deployment_id
                ));
            }
        }

        let mut endpoint_ids = BTreeSet::new();
        for endpoint in &self.endpoints {
            validate_endpoint_id(&endpoint.endpoint)?;
            validate_optional_text("endpoint status message", &endpoint.message, MAX_TEXT_LEN)?;
            validate_required_text("endpoint observed_at", &endpoint.observed_at, MAX_TEXT_LEN)?;
            if !endpoint_ids.insert(endpoint.endpoint.as_str()) {
                return invalid(format!("duplicate endpoint status {}", endpoint.endpoint));
            }
        }

        let mut link_ids = BTreeSet::new();
        for link in &self.links {
            validate_endpoint_id(&link.source_endpoint)?;
            validate_endpoint_id(&link.target_endpoint)?;
            if link.source_endpoint == link.target_endpoint {
                return invalid("link status source and target must differ");
            }
            validate_optional_text("link status message", &link.message, MAX_TEXT_LEN)?;
            validate_required_text("link observed_at", &link.observed_at, MAX_TEXT_LEN)?;
            let key = (link.source_endpoint.as_str(), link.target_endpoint.as_str());
            if !link_ids.insert(key) {
                return invalid(format!(
                    "duplicate link status {} -> {}",
                    link.source_endpoint, link.target_endpoint
                ));
            }
        }

        let mut drift_ids = BTreeSet::new();
        for drift in &self.drift {
            validate_required_text("drift resource_id", &drift.resource_id, MAX_TEXT_LEN)?;
            validate_required_text("drift detail", &drift.detail, MAX_TEXT_LEN)?;
            let key = (&drift.resource_kind, drift.resource_id.as_str());
            if !drift_ids.insert(key) {
                return invalid(format!("duplicate drift status {}", drift.resource_id));
            }
        }
        Ok(())
    }
}

fn diff_changes(from: Option<&TopologySpec>, to: &TopologySpec) -> Vec<TopologyChange> {
    let mut changes = Vec::new();
    if let Some(from) = from {
        if from.root_endpoint != to.root_endpoint {
            changes.push(TopologyChange::RootEndpointUpdated {
                before: from.root_endpoint.clone(),
                after: to.root_endpoint.clone(),
            });
        }
        if from.authority != to.authority {
            changes.push(TopologyChange::AuthorityUpdated {
                before: from.authority.clone(),
                after: to.authority.clone(),
            });
        }
    } else {
        changes.push(TopologyChange::RootEndpointUpdated {
            before: String::new(),
            after: to.root_endpoint.clone(),
        });
        changes.push(TopologyChange::AuthorityUpdated {
            before: TopologyAuthoritySpec {
                root_endpoint: String::new(),
                exposure_policy: String::new(),
            },
            after: to.authority.clone(),
        });
    }

    let from_endpoints = from
        .map(|spec| {
            spec.endpoints
                .iter()
                .map(|endpoint| (endpoint.endpoint.as_str(), endpoint))
                .collect::<BTreeMap<_, _>>()
        })
        .unwrap_or_default();
    let to_endpoints = to
        .endpoints
        .iter()
        .map(|endpoint| (endpoint.endpoint.as_str(), endpoint))
        .collect::<BTreeMap<_, _>>();
    let endpoint_keys = from_endpoints
        .keys()
        .chain(to_endpoints.keys())
        .copied()
        .collect::<BTreeSet<_>>();
    for key in endpoint_keys {
        match (from_endpoints.get(key), to_endpoints.get(key)) {
            (None, Some(endpoint)) => changes.push(TopologyChange::EndpointAdded {
                endpoint: (*endpoint).clone(),
            }),
            (Some(endpoint), None) => changes.push(TopologyChange::EndpointRemoved {
                endpoint: (*endpoint).clone(),
            }),
            (Some(before), Some(after)) if before != after => {
                changes.push(TopologyChange::EndpointUpdated {
                    before: (*before).clone(),
                    after: (*after).clone(),
                });
            }
            _ => {}
        }
    }

    let from_links = from
        .map(|spec| {
            spec.links
                .iter()
                .map(|link| {
                    (
                        (link.source_endpoint.as_str(), link.target_endpoint.as_str()),
                        link,
                    )
                })
                .collect::<BTreeMap<_, _>>()
        })
        .unwrap_or_default();
    let to_links = to
        .links
        .iter()
        .map(|link| {
            (
                (link.source_endpoint.as_str(), link.target_endpoint.as_str()),
                link,
            )
        })
        .collect::<BTreeMap<_, _>>();
    let link_keys = from_links
        .keys()
        .chain(to_links.keys())
        .copied()
        .collect::<BTreeSet<_>>();
    for key in link_keys {
        match (from_links.get(&key), to_links.get(&key)) {
            (None, Some(link)) => changes.push(TopologyChange::LinkAdded {
                link: (*link).clone(),
            }),
            (Some(link), None) => changes.push(TopologyChange::LinkRemoved {
                link: (*link).clone(),
            }),
            (Some(before), Some(after)) if before != after => {
                changes.push(TopologyChange::LinkUpdated {
                    before: (*before).clone(),
                    after: (*after).clone(),
                });
            }
            _ => {}
        }
    }
    changes
}

fn validate_endpoint_spec(endpoint: &TopologyEndpointSpec) -> Result<()> {
    validate_endpoint_id(&endpoint.endpoint)?;
    validate_service_id(&endpoint.service_id)?;
    if parse_endpoint_id(&endpoint.endpoint)?.service_name != endpoint.service_id {
        return invalid(format!(
            "endpoint {} service name must match service_id {}",
            endpoint.endpoint, endpoint.service_id
        ));
    }
    validate_protocol(&endpoint.protocol)?;
    validate_optional_text(
        "endpoint display_name",
        &endpoint.display_name,
        MAX_TEXT_LEN,
    )?;
    validate_optional_text("endpoint note", &endpoint.note, MAX_TEXT_LEN)?;
    if !endpoint.health_path.is_empty() {
        if !endpoint.health_path.starts_with('/') {
            return invalid("endpoint health_path must start with /");
        }
        validate_required_text("endpoint health_path", &endpoint.health_path, MAX_TEXT_LEN)?;
    }
    validate_json_object("endpoint config", &endpoint.config)
}

fn validate_link_spec(link: &TopologyLinkSpec, endpoints: &BTreeSet<&str>) -> Result<()> {
    validate_endpoint_id(&link.source_endpoint)?;
    validate_endpoint_id(&link.target_endpoint)?;
    if link.source_endpoint == link.target_endpoint {
        return invalid("link source and target must be different endpoints");
    }
    if !endpoints.contains(link.source_endpoint.as_str()) {
        return invalid(format!(
            "link source endpoint {} is not registered",
            link.source_endpoint
        ));
    }
    if !endpoints.contains(link.target_endpoint.as_str()) {
        return invalid(format!(
            "link target endpoint {} is not registered",
            link.target_endpoint
        ));
    }
    validate_protocol(&link.protocol)?;
    validate_token("link auth_mode", &link.auth_mode, MAX_TEXT_LEN)?;
    validate_token("link scope", &link.scope, MAX_TEXT_LEN)?;
    validate_optional_text("link config_ref", &link.config_ref, MAX_TEXT_LEN)?;
    validate_optional_text("link secret_ref", &link.secret_ref, MAX_TEXT_LEN)?;
    if link.auth_mode == "secret-ref" && link.secret_ref.is_empty() {
        return invalid("secret-ref link auth_mode requires secret_ref");
    }
    let mut binding_names = BTreeSet::new();
    for binding in &link.api_bindings {
        validate_binding_name(&binding.requirement_name)?;
        validate_token("link api binding api_id", &binding.api_id, MAX_TEXT_LEN)?;
        if !binding_names.insert(binding.requirement_name.as_str()) {
            return invalid(format!(
                "link {} -> {} has duplicate API binding name {}",
                link.source_endpoint, link.target_endpoint, binding.requirement_name
            ));
        }
        if !binding.version.is_empty() {
            validate_required_text("link api binding version", &binding.version, MAX_TEXT_LEN)?;
        }
        if !binding.provider_deployment_id.is_empty() {
            validate_token(
                "link api binding provider_deployment_id",
                &binding.provider_deployment_id,
                MAX_TEXT_LEN,
            )?;
        }
        if !matches!(
            binding.selection.as_str(),
            "nearest-healthy" | "same-node" | "explicit"
        ) {
            return invalid(format!(
                "link API binding {} has unsupported selection policy {}",
                binding.requirement_name, binding.selection
            ));
        }
        if binding.selection == "explicit" && binding.provider_deployment_id.is_empty() {
            return invalid(format!(
                "explicit link API binding {} requires provider_deployment_id",
                binding.requirement_name
            ));
        }
    }
    validate_json_object("link policy", &link.policy)
}

fn validate_topology_id(value: &str) -> Result<()> {
    validate_required_text("topology_id", value, MAX_TOPOLOGY_ID_LEN)?;
    let mut chars = value.chars();
    let Some(first) = chars.next() else {
        return invalid("topology_id is required");
    };
    if !first.is_ascii_lowercase() && !first.is_ascii_digit() {
        return invalid("topology_id must start with a lowercase letter or digit");
    }
    if !chars.all(|character| {
        character.is_ascii_lowercase()
            || character.is_ascii_digit()
            || matches!(character, '-' | '.' | '_')
    }) {
        return invalid("topology_id contains unsupported characters");
    }
    Ok(())
}

fn validate_service_id(value: &str) -> Result<()> {
    validate_required_text("service_id", value, MAX_TEXT_LEN)?;
    let mut chars = value.chars();
    let Some(first) = chars.next() else {
        return invalid("service_id is required");
    };
    if !first.is_ascii_lowercase() && !first.is_ascii_digit() {
        return invalid("service_id must start with a lowercase letter or digit");
    }
    if !chars.all(|character| {
        character.is_ascii_lowercase() || character.is_ascii_digit() || character == '-'
    }) {
        return invalid("service_id contains unsupported characters");
    }
    Ok(())
}

fn validate_protocol(value: &str) -> Result<()> {
    validate_token("protocol", value, 32)
}

fn validate_token(name: &str, value: &str, max_len: usize) -> Result<()> {
    validate_required_text(name, value, max_len)?;
    let mut chars = value.chars();
    let Some(first) = chars.next() else {
        return invalid(format!("{name} is required"));
    };
    if !first.is_ascii_lowercase() {
        return invalid(format!("{name} must start with a lowercase letter"));
    }
    if !chars.all(|character| {
        character.is_ascii_lowercase()
            || character.is_ascii_digit()
            || matches!(character, '-' | '.' | '+')
    }) {
        return invalid(format!("{name} contains unsupported characters"));
    }
    Ok(())
}

fn validate_binding_name(value: &str) -> Result<()> {
    validate_required_text("link api binding name", value, MAX_TEXT_LEN)?;
    let mut chars = value.chars();
    let Some(first) = chars.next() else {
        return invalid("link api binding name is required");
    };
    if !first.is_ascii_alphanumeric()
        || !chars.all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.' | ':')
        })
    {
        return invalid("link api binding name contains unsupported characters");
    }
    Ok(())
}

fn validate_required_text(name: &str, value: &str, max_len: usize) -> Result<()> {
    if value.is_empty() {
        return invalid(format!("{name} is required"));
    }
    validate_optional_text(name, value, max_len)
}

fn validate_optional_text(name: &str, value: &str, max_len: usize) -> Result<()> {
    if value != value.trim() {
        return invalid(format!("{name} must not have surrounding whitespace"));
    }
    if value.len() > max_len {
        return invalid(format!("{name} exceeds {max_len} bytes"));
    }
    if value.chars().any(char::is_control) {
        return invalid(format!("{name} contains control characters"));
    }
    Ok(())
}

fn validate_optional_identifier(name: &str, value: Option<&str>) -> Result<()> {
    if let Some(value) = value {
        validate_required_text(name, value, MAX_TEXT_LEN)?;
    }
    Ok(())
}

fn validate_json_object(name: &str, value: &Value) -> Result<()> {
    if !value.is_null() && !value.is_object() {
        return invalid(format!("{name} must be an object or null"));
    }
    if serde_json::to_vec(value)?.len() > MAX_JSON_BYTES {
        return invalid(format!("{name} exceeds {MAX_JSON_BYTES} bytes"));
    }
    Ok(())
}

fn validate_revision_lineage(
    revision_number: u64,
    parent_revision_id: Option<&str>,
    rollback_of_revision_id: Option<&str>,
) -> Result<()> {
    if revision_number == 0 {
        return invalid("revision_number must be greater than zero");
    }
    if revision_number == 1 && parent_revision_id.is_some() {
        return invalid("initial revision cannot have a parent");
    }
    if revision_number == 1 && rollback_of_revision_id.is_some() {
        return invalid("initial revision cannot be a rollback");
    }
    if revision_number > 1 && parent_revision_id.is_none() {
        return invalid("non-initial revision requires parent_revision_id");
    }
    validate_optional_identifier("parent_revision_id", parent_revision_id)?;
    validate_optional_identifier("rollback_of_revision_id", rollback_of_revision_id)?;
    if rollback_of_revision_id.is_some() && rollback_of_revision_id == parent_revision_id {
        return invalid("rollback target cannot be the current parent revision");
    }
    Ok(())
}

fn revision_id(topology_id: &str, revision_number: u64, content_sha256: &str) -> String {
    format!("{topology_id}:r{revision_number}:{content_sha256}")
}

fn hex_sha256(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    digest
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>()
}

fn default_true() -> bool {
    true
}

fn invalid<T>(message: impl Into<String>) -> Result<T> {
    Err(OrchestratorError::InvalidManifest(message.into()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn endpoint(ip: &str, port: u16, service_id: &str) -> TopologyEndpointSpec {
        TopologyEndpointSpec {
            endpoint: format!("{ip}:{port}:{service_id}"),
            service_id: service_id.to_string(),
            protocol: "http".to_string(),
            health_path: "/health".to_string(),
            display_name: service_id.to_string(),
            note: String::new(),
            config: json!({}),
        }
    }

    fn link(source: &str, target: &str) -> TopologyLinkSpec {
        TopologyLinkSpec {
            source_endpoint: source.to_string(),
            target_endpoint: target.to_string(),
            protocol: "http".to_string(),
            auth_mode: "internal".to_string(),
            scope: "api".to_string(),
            enabled: true,
            config_ref: String::new(),
            secret_ref: String::new(),
            policy: json!({}),
            api_bindings: Vec::new(),
        }
    }

    fn spec(reverse: bool) -> TopologySpec {
        let gateway = endpoint("127.0.0.1", 8080, "gateway");
        let problem = endpoint("127.0.0.1", 8083, "problem-service");
        let mut endpoints = vec![gateway.clone(), problem.clone()];
        if reverse {
            endpoints.reverse();
        }
        TopologySpec::new(
            "primary",
            gateway.endpoint.clone(),
            "root-only",
            endpoints,
            vec![link(&gateway.endpoint, &problem.endpoint)],
        )
        .expect("valid spec")
    }

    #[test]
    fn canonical_spec_hash_does_not_depend_on_input_order() {
        let forward = spec(false);
        let reverse = spec(true);
        assert_eq!(forward, reverse);
        assert_eq!(
            forward.content_sha256().unwrap(),
            reverse.content_sha256().unwrap()
        );
        assert_eq!(forward.endpoints[0].service_id, "gateway");
    }

    #[test]
    fn canonical_spec_hash_does_not_depend_on_api_binding_order() {
        let mut forward = spec(false);
        forward.links[0].api_bindings = vec![
            TopologyApiBindingSpec {
                requirement_name: "storage_put".to_string(),
                api_id: "storage.object.put".to_string(),
                version: ">=1.0.0, <2.0.0".to_string(),
                optional: false,
                provider_deployment_id: "storage-a".to_string(),
                selection: "explicit".to_string(),
            },
            TopologyApiBindingSpec {
                requirement_name: "storage_get".to_string(),
                api_id: "storage.object.get".to_string(),
                version: ">=1.0.0, <2.0.0".to_string(),
                optional: false,
                provider_deployment_id: "storage-a".to_string(),
                selection: "explicit".to_string(),
            },
        ];
        let forward = forward.canonicalized().unwrap();
        let mut reverse = forward.clone();
        reverse.links[0].api_bindings.reverse();
        let reverse = reverse.canonicalized().unwrap();

        assert_eq!(forward, reverse);
        assert_eq!(
            forward.content_sha256().unwrap(),
            reverse.content_sha256().unwrap()
        );
        assert_eq!(
            forward.links[0].api_bindings[0].requirement_name,
            "storage_get"
        );
    }

    #[test]
    fn strict_spec_validation_rejects_duplicate_and_dangling_resources() {
        let mut duplicate = spec(false);
        duplicate.endpoints.push(duplicate.endpoints[0].clone());
        assert!(duplicate.validate().is_err());

        let mut dangling = spec(false);
        dangling.links[0].target_endpoint = "127.0.0.1:9000:missing".to_string();
        assert!(dangling.validate().is_err());

        let mut wrong_root = spec(false);
        wrong_root.authority.root_endpoint = wrong_root.endpoints[1].endpoint.clone();
        assert!(wrong_root.validate().is_err());
    }

    #[test]
    fn spec_rejects_runtime_only_and_unknown_fields() {
        let value = serde_json::to_value(spec(false)).unwrap();
        let mut value = value.as_object().unwrap().clone();
        value.insert("operations".to_string(), json!([]));
        assert!(serde_json::from_value::<TopologySpec>(Value::Object(value)).is_err());

        let endpoint_value = json!({
            "endpoint": "127.0.0.1:8080:gateway",
            "service_id": "gateway",
            "protocol": "http",
            "health": "healthy"
        });
        assert!(serde_json::from_value::<TopologyEndpointSpec>(endpoint_value).is_err());
    }

    #[test]
    fn registered_service_validation_rejects_manifest_only_endpoint() {
        let topology = spec(false);
        let registered = BTreeSet::from(["gateway".to_string()]);
        assert!(
            topology
                .validate_against_registered_services(&registered)
                .is_err()
        );
    }

    #[test]
    fn diff_is_deterministic_and_sorted_by_resource_identity() {
        let before = spec(false);
        let mut after = before.clone();
        after.endpoints[1].display_name = "Problems".to_string();
        let user = endpoint("127.0.0.1", 8084, "user-service");
        after.endpoints.push(user.clone());
        after.links.push(link(&after.root_endpoint, &user.endpoint));
        after = after.canonicalized().unwrap();

        let first = diff_topology_specs(Some(&before), &after).unwrap();
        let second = diff_topology_specs(Some(&spec(true)), &after).unwrap();
        assert_eq!(first, second);
        assert_eq!(
            first
                .changes
                .iter()
                .map(TopologyChange::resource_key)
                .collect::<Vec<_>>(),
            vec![
                "endpoint/127.0.0.1:8083:problem-service",
                "endpoint/127.0.0.1:8084:user-service",
                "link/127.0.0.1:8080:gateway->127.0.0.1:8084:user-service",
            ]
        );
        assert_eq!(
            serde_json::to_vec(&first).unwrap(),
            serde_json::to_vec(&second).unwrap()
        );
    }

    #[test]
    fn revision_is_content_addressed_and_tampering_is_rejected() {
        let first =
            TopologyRevision::initial(spec(false), "2026-08-03T00:00:00Z", "admin", "initial")
                .unwrap();
        assert_eq!(first.revision_number(), 1);
        assert_eq!(first.parent_revision_id(), None);

        let mut next_spec = first.spec().clone();
        next_spec.endpoints[1].note = "managed".to_string();
        let second = first
            .next(next_spec, "2026-08-03T00:01:00Z", "admin", "update note")
            .unwrap();
        assert_eq!(second.revision_number(), 2);
        assert_eq!(second.parent_revision_id(), Some(first.revision_id()));
        assert_ne!(second.content_sha256(), first.content_sha256());

        let mut serialized = serde_json::to_value(&second).unwrap();
        serialized["content_sha256"] = json!("00".repeat(32));
        assert!(serde_json::from_value::<TopologyRevision>(serialized).is_err());
        assert!(
            first
                .next(
                    first.spec().clone(),
                    "2026-08-03T00:02:00Z",
                    "admin",
                    "noop",
                )
                .is_err()
        );
    }

    #[test]
    fn rollback_creates_a_new_revision_without_mutating_history() {
        let first =
            TopologyRevision::initial(spec(false), "2026-08-03T00:00:00Z", "admin", "initial")
                .unwrap();
        let mut updated = first.spec().clone();
        updated.endpoints[1].note = "changed".to_string();
        let second = first
            .next(updated, "2026-08-03T00:01:00Z", "admin", "change")
            .unwrap();
        let rollback = rollback_topology_revision(
            &second,
            &first,
            "2026-08-03T00:02:00Z",
            "admin",
            "rollback",
        )
        .unwrap();

        assert_eq!(rollback.revision_number(), 3);
        assert_eq!(rollback.parent_revision_id(), Some(second.revision_id()));
        assert_eq!(
            rollback.rollback_of_revision_id(),
            Some(first.revision_id())
        );
        assert_eq!(rollback.spec(), first.spec());
        assert_ne!(rollback.revision_id(), first.revision_id());
    }

    #[test]
    fn in_sync_status_requires_matching_revision_and_no_drift() {
        let mut status = TopologyStatus::draft(
            "primary",
            Some("primary:r1:0123456789abcdef".to_string()),
            "2026-08-03T00:00:00Z",
        )
        .unwrap();
        status.state = TopologyReconciliationState::InSync;
        assert!(status.validate().is_err());

        status.observed_revision_id = status.desired_revision_id.clone();
        assert!(status.validate().is_ok());
        status.drift.push(TopologyDrift {
            resource_kind: TopologyResourceKind::Endpoint,
            resource_id: "127.0.0.1:8080:gateway".to_string(),
            kind: TopologyDriftKind::Changed,
            detail: "runtime differs from desired state".to_string(),
        });
        assert!(status.validate().is_err());
    }
}
