//! Store commands responsibilities.
use crate::store::error::StoreError;
use orchestrator_manager::catalog_v2::ReleaseChannel;
use orchestrator_manager::store::validation::InstallBindingSelection;
use orchestrator_manager::store::validation::InstallTopologySelection;
use serde::Deserialize;
use serde_json::Value;
use std::collections::BTreeMap;
use std::collections::BTreeSet;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ImportReleaseRequest {
    pub(crate) service_id: String,
    pub(crate) target_node_id: String,
    #[serde(default)]
    pub(crate) catalog_source_id: String,
    #[serde(default)]
    pub(crate) version: String,
    #[serde(default = "default_release_channel")]
    pub(crate) channel: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct InstallReleaseRequest {
    #[serde(default)]
    pub(crate) service_id: String,
    #[serde(default)]
    pub(crate) catalog_source_id: String,
    #[serde(default)]
    pub(crate) source_url: String,
    #[serde(default, alias = "metadata_sha256")]
    pub(crate) checksum: String,
    #[serde(default)]
    pub(crate) version: String,
    #[serde(default = "default_release_channel")]
    pub(crate) channel: String,
    #[serde(default)]
    pub(crate) target_node_id: String,
    #[serde(default)]
    pub(crate) endpoint: String,
    #[serde(default = "default_managed_mode")]
    pub(crate) mode: String,
    #[serde(default = "default_true")]
    pub(crate) start: bool,
    #[serde(default = "default_apply_policy")]
    pub(crate) migration_policy: String,
    #[serde(default)]
    pub(crate) gateway_node_id: String,
    #[serde(default)]
    pub(crate) config: Value,
    #[serde(default)]
    pub(crate) secret_refs: BTreeMap<String, String>,
    #[serde(default)]
    pub(crate) plan_digest: String,
    #[serde(default)]
    pub(crate) release_graph_digest: String,
    /// Per Composition node inputs. Legacy root `config` and `secret_refs`
    /// remain aliases for one release cycle.
    #[serde(default)]
    pub(crate) inputs: BTreeMap<String, BTreeMap<String, Value>>,
    #[serde(default)]
    pub(crate) bindings: Vec<InstallBindingSelection>,
    #[serde(default)]
    pub(crate) topology_id: String,
    #[serde(default)]
    pub(crate) topology_etag: String,
    #[serde(default)]
    pub(crate) topology: Option<InstallTopologySelection>,
}

pub(crate) fn normalize_store_topology_selection(
    topology_id: &str,
    topology_etag: &str,
    compatibility: Option<&InstallTopologySelection>,
) -> Result<Option<InstallTopologySelection>, StoreError> {
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
        return Err(StoreError::new(
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
        return Err(StoreError::new(
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

pub(crate) fn strong_topology_etag(value: &str) -> Result<&str, StoreError> {
    value
        .trim()
        .strip_prefix('"')
        .and_then(|value| value.strip_suffix('"'))
        .filter(|value| !value.is_empty() && !value.contains('"'))
        .ok_or_else(|| {
            StoreError::new(
                422,
                "STORE_TOPOLOGY_ETAG_INVALID",
                "topology_etag must be a strong quoted revision ETag",
            )
        })
}

pub(crate) fn normalize_replacement_topologies(
    single: Option<&InstallTopologySelection>,
    group: &[ReplacementTopologyCas],
) -> Result<Vec<InstallTopologySelection>, StoreError> {
    if group.is_empty() {
        return Ok(single.into_iter().cloned().collect());
    }
    if single.is_some() {
        return Err(StoreError::new(
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
            return Err(StoreError::new(
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

pub(crate) fn default_managed_mode() -> String {
    "MANAGED".to_string()
}

pub(crate) fn default_apply_policy() -> String {
    "APPLY".to_string()
}

pub(crate) fn default_true() -> bool {
    true
}

pub(crate) fn default_release_channel() -> String {
    "stable".to_string()
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ReplaceReleaseRequest {
    pub(crate) deployment_id: String,
    #[serde(default)]
    pub(crate) catalog_source_id: String,
    #[serde(default)]
    pub(crate) version: String,
    #[serde(default)]
    pub(crate) channel: Option<String>,
    #[serde(default = "default_apply_policy")]
    pub(crate) migration_policy: String,
    #[serde(default)]
    pub(crate) endpoint: String,
    #[serde(default)]
    pub(crate) gateway_node_id: String,
    #[serde(default)]
    pub(crate) config: Value,
    #[serde(default)]
    pub(crate) secret_refs: BTreeMap<String, String>,
    #[serde(default)]
    pub(crate) bindings: Vec<InstallBindingSelection>,
    #[serde(default)]
    pub(crate) topology_id: String,
    #[serde(default)]
    pub(crate) topology_etag: String,
    /// Strong-CAS inputs for provider replacements referenced by more than
    /// one applied topology. Entries are normalized and processed in
    /// topology-id order so the resulting Operation plan is deterministic.
    #[serde(default)]
    pub(crate) topologies: Vec<ReplacementTopologyCas>,
    #[serde(default)]
    pub(crate) topology: Option<InstallTopologySelection>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ReplacementTopologyCas {
    pub(crate) topology_id: String,
    pub(crate) topology_etag: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ReleaseDeleteRequest {
    pub(crate) service_id: String,
    pub(crate) version: String,
}

pub(crate) fn parse_release_channel(value: &str) -> Result<ReleaseChannel, StoreError> {
    match value.trim().to_ascii_lowercase().as_str() {
        "stable" | "" => Ok(ReleaseChannel::Stable),
        "beta" => Ok(ReleaseChannel::Beta),
        "nightly" => Ok(ReleaseChannel::Nightly),
        _ => Err(StoreError::new(
            422,
            "CATALOG_CHANNEL_INVALID",
            "channel must be stable, beta, or nightly",
        )),
    }
}

pub(crate) fn required_text<'a>(value: &'a str, field: &str) -> Result<&'a str, StoreError> {
    non_empty(value).ok_or_else(|| {
        StoreError::new(422, "STORE_REQUEST_INVALID", format!("{field} is required"))
    })
}

pub(crate) fn non_empty(value: &str) -> Option<&str> {
    let value = value.trim();
    (!value.is_empty()).then_some(value)
}
