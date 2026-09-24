//! Background payload responsibilities.
use orchestrator_core::ApiBinding;
use serde::Deserialize;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct TopologyApplyPayload {
    #[serde(default)]
    pub(super) topology_id: String,
    #[serde(default)]
    pub(super) revision_id: String,
    #[serde(default)]
    pub(super) phase: TopologyApplyPhase,
    #[serde(default)]
    pub(super) bindings: Vec<ApiBinding>,
    #[serde(default)]
    pub(super) previous_bindings: Vec<ApiBinding>,
    #[serde(default)]
    pub(super) group: Vec<TopologyApplyGroupPayloadMember>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct TopologyApplyGroupPayloadMember {
    pub(super) topology_id: String,
    pub(super) revision_id: String,
}

#[derive(Debug, Clone, Copy, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub(super) enum TopologyApplyPhase {
    #[default]
    Full,
    Stage,
    Prepare,
    Finalize,
    FinalizeGroup,
    Abort,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct NodeLifecyclePayload {
    pub(super) node_id: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ExternalHealthPayload {
    pub(super) deployment_id: String,
    pub(super) service_id: String,
    pub(super) version: String,
    pub(super) endpoint: String,
    pub(super) protocol: String,
    #[serde(default)]
    pub(super) health_path: String,
    pub(super) artifact_digest: String,
}
