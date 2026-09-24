//! Deterministic pipeline contracts; no environment reads or I/O.

use super::{ContainerSpec, OciImageReference};
use crate::{HealthGatePolicy, RuntimeError, RuntimeInstance};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Shared wire payload for an ordinary managed install.  It deliberately
/// lives beside the runtime contracts so the control plane and Agent decode
/// one schema instead of maintaining look-alike private structs.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RuntimeInstallPayload {
    pub spec: ContainerSpec,
    #[serde(default = "default_true")]
    pub start: bool,
    #[serde(default)]
    pub health_gate: HealthGatePolicy,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub offline_oci_artifact: Option<ArtifactReference>,
}

pub const RESOURCE_PURGE_JOB_SCHEMA_VERSION: &str = "ojos.dev/resource-purge-job/v1";

/// Credential-free wire contract for the separately authorized ResourceClaim
/// purge job. Administrator connection details and resource output paths are
/// deliberately absent: the Agent resolves both from node-local durable state.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ResourcePurgePayloadV1 {
    pub schema_version: String,
    pub node_id: String,
    pub claim_id: String,
    pub claim_digest: String,
    pub generation: u64,
    pub confirmation: String,
    pub reason: String,
    pub audit_intent: ResourcePurgeAuditIntentV1,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ResourcePurgeAuditIntentV1 {
    pub intent_id: String,
    pub actor_id: String,
    pub claim_digest: String,
    pub generation: u64,
}

impl ResourcePurgePayloadV1 {
    pub fn validate(&self) -> Result<(), String> {
        if self.schema_version != RESOURCE_PURGE_JOB_SCHEMA_VERSION {
            return Err("resource purge payload schema_version is unsupported".to_string());
        }
        for (field, value) in [
            ("node_id", self.node_id.as_str()),
            ("claim_id", self.claim_id.as_str()),
            (
                "audit_intent.intent_id",
                self.audit_intent.intent_id.as_str(),
            ),
        ] {
            if !valid_resource_purge_identifier(value) {
                return Err(format!(
                    "resource purge {field} is not a canonical identifier"
                ));
            }
        }
        if self.audit_intent.actor_id.trim() != self.audit_intent.actor_id
            || self.audit_intent.actor_id.is_empty()
            || self.audit_intent.actor_id.len() > 256
            || self.audit_intent.actor_id.chars().any(char::is_control)
        {
            return Err(
                "resource purge actor_id must be an authenticated subject without whitespace padding or controls"
                    .to_string(),
            );
        }
        if !valid_sha256_digest(&self.claim_digest)
            || self.audit_intent.claim_digest != self.claim_digest
        {
            return Err(
                "resource purge claim digest is invalid or not bound to its audit intent"
                    .to_string(),
            );
        }
        if self.generation == 0 || self.audit_intent.generation != self.generation {
            return Err(
                "resource purge generation is invalid or not bound to its audit intent".to_string(),
            );
        }
        let expected = format!(
            "PURGE {} {} GENERATION {}",
            self.claim_id, self.claim_digest, self.generation
        );
        if self.confirmation != expected {
            return Err(
                "resource purge confirmation does not exactly match the target identity"
                    .to_string(),
            );
        }
        if self.reason.trim() != self.reason
            || !(8..=512).contains(&self.reason.chars().count())
            || self.reason.chars().any(char::is_control)
        {
            return Err(
                "resource purge reason must be 8..512 characters without surrounding whitespace or controls"
                    .to_string(),
            );
        }
        Ok(())
    }
}

fn valid_resource_purge_identifier(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 180
        && value
            .bytes()
            .next()
            .is_some_and(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
        && value
            .bytes()
            .last()
            .is_some_and(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
        && value.bytes().all(|byte| {
            byte.is_ascii_lowercase()
                || byte.is_ascii_digit()
                || matches!(byte, b'-' | b'_' | b'.' | b':')
        })
}

fn valid_sha256_digest(value: &str) -> bool {
    value.strip_prefix("sha256:").is_some_and(|hex| {
        hex.len() == 64
            && hex
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    })
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ArtifactReference {
    /// Content-addressed identifier served only through the authenticated
    /// Agent protocol. Job payloads never contain artifact bytes.
    pub artifact_id: String,
    pub sha256: String,
    pub size_bytes: u64,
    #[serde(default = "default_artifact_chunk_bytes")]
    pub chunk_bytes: u32,
}

const fn default_artifact_chunk_bytes() -> u32 {
    1024 * 1024
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AuthServiceIdentitySpec {
    pub service_name: String,
    #[serde(default)]
    pub allowed_apis: Vec<String>,
    #[serde(default)]
    pub grants: Vec<AuthServiceIdentityGrantSpec>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AuthServiceIdentityGrantSpec {
    pub api_id: String,
    pub permission: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AuthPipelineStep {
    pub service_name: String,
    #[serde(default)]
    pub permissions: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub service_identity: Option<AuthServiceIdentitySpec>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct GatewayRouteSpec {
    pub route_id: String,
    pub path_prefix: String,
    pub upstream_base: String,
    #[serde(default)]
    pub api_id: String,
    #[serde(default)]
    pub binding_id: String,
    #[serde(default)]
    pub consumer_deployment_id: String,
    #[serde(default = "default_credential_generation")]
    pub credential_generation: u64,
    #[serde(default = "default_binding_timeout_ms")]
    pub timeout_ms: u64,
    #[serde(default)]
    pub provider_node_id: String,
    #[serde(default)]
    pub provider_endpoint: String,
    #[serde(default)]
    pub strip_prefix: bool,
    #[serde(default)]
    pub rewrite_prefix: String,
    #[serde(default)]
    pub methods: Vec<String>,
    pub auth_mode: String,
    #[serde(default)]
    pub required_permission: String,
}

const fn default_credential_generation() -> u64 {
    1
}

const fn default_binding_timeout_ms() -> u64 {
    30_000
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct GatewayPipelineStep {
    pub operation_id: String,
    pub service_name: String,
    pub node_id: String,
    #[serde(default)]
    pub routes: Vec<GatewayRouteSpec>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RuntimeMaterializationStep {
    #[serde(default)]
    pub config: BTreeMap<String, String>,
    /// Maps the manifest secret name to a Node-local provider reference. The
    /// secret value itself is never persisted in the control-plane Job.
    #[serde(default)]
    pub secret_refs: BTreeMap<String, String>,
    #[serde(default)]
    pub environment_templates: BTreeMap<String, String>,
}

/// Credential-free request for one Agent-local managed resource.  The
/// control plane sends only stable identity and provider selection; provider
/// administrator credentials and the resulting connection document never
/// cross the Node boundary.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ResourceClaimStepV1 {
    pub claim_id: String,
    /// Stable installation/service-instance owner. Unlike deployment_id this
    /// value must survive release upgrades and rollbacks.
    pub owner_instance_id: String,
    /// Runtime binding for this execution. It may change across releases and
    /// is deliberately excluded from the durable resource identity.
    pub deployment_id: String,
    pub service_id: String,
    pub resource_name: String,
    pub resource_type: String,
    #[serde(default = "default_resource_claim_generation")]
    pub generation: u64,
    pub provider_id: String,
    /// Environment key receiving the Node-local output *file path*.  The file
    /// contains the DSN and is shared by the migration and service runtime.
    /// It never contains the DSN itself.
    pub output_path_environment: String,
}

impl ResourceClaimStepV1 {
    pub fn validate(&self) -> Result<(), RuntimeError> {
        for (name, value) in [
            ("claim_id", self.claim_id.as_str()),
            ("owner_instance_id", self.owner_instance_id.as_str()),
            ("deployment_id", self.deployment_id.as_str()),
            ("service_id", self.service_id.as_str()),
            ("resource_name", self.resource_name.as_str()),
            ("provider_id", self.provider_id.as_str()),
        ] {
            if value.is_empty()
                || value.len() > 256
                || !value.bytes().all(|byte| {
                    byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b':' | b'-')
                })
            {
                return Err(RuntimeError::InvalidRuntimeContext(format!(
                    "resource claim {name} is not a bounded identifier"
                )));
            }
        }
        if self.resource_type != "postgresql.database/v1" || self.generation == 0 {
            return Err(RuntimeError::InvalidRuntimeContext(
                "resource claim must be postgresql.database/v1 with a positive generation"
                    .to_string(),
            ));
        }
        let environment = self.output_path_environment.as_bytes();
        if environment.is_empty()
            || environment.len() > 128
            || !environment[0].is_ascii_alphabetic()
            || !environment
                .iter()
                .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit() || *byte == b'_')
        {
            return Err(RuntimeError::InvalidRuntimeContext(
                "resource output_path_environment must be a bounded uppercase environment key"
                    .to_string(),
            ));
        }
        let normalized_resource = self
            .resource_name
            .bytes()
            .map(|byte| {
                if byte.is_ascii_alphanumeric() {
                    byte.to_ascii_uppercase() as char
                } else {
                    '_'
                }
            })
            .collect::<String>();
        let expected_environment = format!("OJOS_RESOURCE_{normalized_resource}_OUTPUT_FILE");
        if self.output_path_environment != expected_environment {
            return Err(RuntimeError::InvalidRuntimeContext(format!(
                "resource output_path_environment must be {expected_environment}"
            )));
        }
        Ok(())
    }
}

const fn default_resource_claim_generation() -> u64 {
    1
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "provider", rename_all = "snake_case", deny_unknown_fields)]
pub enum TypedProvisionerStep {
    Redis {
        service_name: String,
        resources: Vec<RedisNamespaceSpec>,
    },
    Storage {
        service_name: String,
        resources: Vec<StorageResourceSpec>,
    },
    ApiRegistry {
        service_name: String,
        #[serde(default = "default_provider_connection_id")]
        registry_id: String,
        apis: Vec<ApiSurfaceSpec>,
        required_apis: Vec<String>,
    },
    Frontend {
        service_name: String,
        #[serde(default = "default_provider_connection_id")]
        asset_store_id: String,
        version: String,
        route_prefix: String,
        remote_entry: String,
        metadata_source_url: String,
        metadata_sha256: String,
    },
}

impl TypedProvisionerStep {
    pub fn provider_name(&self) -> &'static str {
        match self {
            Self::Redis { .. } => "redis",
            Self::Storage { .. } => "storage",
            Self::ApiRegistry { .. } => "api_registry",
            Self::Frontend { .. } => "frontend",
        }
    }

    pub fn service_name(&self) -> &str {
        match self {
            Self::Redis { service_name, .. }
            | Self::Storage { service_name, .. }
            | Self::ApiRegistry { service_name, .. }
            | Self::Frontend { service_name, .. } => service_name,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RedisNamespaceSpec {
    pub name: String,
    pub kind: String,
    #[serde(default = "default_provider_connection_id")]
    pub connection_id: String,
    pub namespace: String,
    pub consumer_group: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct StorageResourceSpec {
    pub object_type: String,
    pub bucket: String,
    pub prefix: String,
    #[serde(default = "default_storage_backend")]
    pub backend: String,
    #[serde(default = "default_provider_connection_id")]
    pub connection_id: String,
}

fn default_provider_connection_id() -> String {
    "default".to_string()
}

fn default_storage_backend() -> String {
    "node_directory".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ApiSurfaceSpec {
    pub api_id: String,
    pub protocol: String,
    pub path_prefix: String,
    #[serde(default)]
    pub methods: Vec<String>,
    pub visibility: String,
    pub auth_mode: String,
    pub permission: String,
    pub version: String,
}

/// Signed, immutable one-shot migration declaration sent to the selected
/// Node.  The Agent ledger keys the durable outcome by service/version and
/// refuses a checksum or image mismatch.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct OciMigrationStep {
    pub service_name: String,
    pub version: String,
    pub checksum: String,
    pub image: OciImageReference,
    #[serde(default)]
    pub command: Vec<String>,
    #[serde(default)]
    pub environment: Vec<String>,
    /// Resource names whose Node-local output files must be made available to
    /// this migration.  Old payloads omit this field and retain v1 behavior.
    #[serde(default)]
    pub resource_claims: Vec<String>,
    pub timeout_ms: u64,
    #[serde(default)]
    pub dry_run: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ReleasePipelinePayload {
    pub install: RuntimeInstallPayload,
    /// Agent-side managed resource Ensures.  This additive default preserves
    /// decoding of every legacy ReleasePipeline payload.
    #[serde(default)]
    pub resource_claims: Vec<ResourceClaimStepV1>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub materialization: Option<RuntimeMaterializationStep>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth: Option<AuthPipelineStep>,
    #[serde(default)]
    pub provisioners: Vec<TypedProvisionerStep>,
    #[serde(default)]
    pub migrations: Vec<OciMigrationStep>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gateway: Option<GatewayPipelineStep>,
}

/// Complete declarative provider state owned by one signed Release revision.
/// Replacement jobs carry both sides so compensation restores the proven old
/// state instead of deleting resources that the old deployment still needs.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ReleaseProviderRevision {
    pub revision_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth: Option<AuthPipelineStep>,
    #[serde(default)]
    pub provisioners: Vec<TypedProvisionerStep>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gateway: Option<GatewayPipelineStep>,
}

impl ReleaseProviderRevision {
    pub fn has_managed_state(&self) -> bool {
        self.auth.is_some() || !self.provisioners.is_empty() || self.gateway.is_some()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ReplacementProviderSaga {
    pub previous: ReleaseProviderRevision,
    pub desired: ReleaseProviderRevision,
}

/// Successful result of an atomic single-node release replacement. The
/// control plane can update both runtime projections in one database
/// transaction from this self-contained value.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RuntimeReplacement {
    pub instance: RuntimeInstance,
    pub replaced_deployment_id: String,
    pub replaced_container_id: String,
}

/// Wire payload shared by Upgrade and Rollback jobs. `start=false` is
/// represented for forward-compatible decoding but rejected by `validate` in
/// v1 because a stopped replacement cannot pass the cutover health gate.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ReleaseReplacementPayload {
    pub old_deployment_id: String,
    pub old_container_id: String,
    pub new_spec: ContainerSpec,
    #[serde(default = "default_true")]
    pub start: bool,
    #[serde(default)]
    pub health_gate: HealthGatePolicy,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub offline_oci_artifact: Option<ArtifactReference>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub materialization: Option<RuntimeMaterializationStep>,
    /// Existing stable ResourceClaims that the replacement must reuse. The
    /// Agent rejects a set that differs from the old deployment's bindings.
    #[serde(default)]
    pub resource_claims: Vec<ResourceClaimStepV1>,
    #[serde(default)]
    pub migrations: Vec<OciMigrationStep>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_saga: Option<ReplacementProviderSaga>,
    /// Keep the proven old container and its runtime projection until a
    /// control-plane Topology job has atomically switched every ApiBinding to
    /// the healthy replacement. The old container is removed by a subsequent
    /// explicit Uninstall job in the same Operation.
    #[serde(default)]
    pub preserve_old_until_topology_cutover: bool,
    /// Require a stop-before-start cutover when the old and new release attach
    /// the same retained read-write volume. This deliberately trades a short
    /// bounded outage for a hard single-writer invariant.
    #[serde(default)]
    pub exclusive_retained_volume_cutover: bool,
}

impl ReleaseReplacementPayload {
    pub fn validate(&self) -> Result<(), RuntimeError> {
        if self.old_deployment_id.trim().is_empty()
            || self.old_container_id.trim().is_empty()
            || self.new_spec.deployment_id.trim().is_empty()
        {
            return Err(RuntimeError::InvalidReleaseReplacement(
                "old_deployment_id, old_container_id, and new_spec.deployment_id are required"
                    .to_string(),
            ));
        }
        if self.old_deployment_id == self.new_spec.deployment_id {
            return Err(RuntimeError::InvalidReleaseReplacement(
                "new_spec.deployment_id must differ from old_deployment_id so both containers can coexist before cutover"
                    .to_string(),
            ));
        }
        if !self.start {
            return Err(RuntimeError::InvalidReleaseReplacement(
                "start must be true because cutover is gated on new-instance health".to_string(),
            ));
        }
        if self.new_spec.retained_volume.is_some() && !self.exclusive_retained_volume_cutover {
            return Err(RuntimeError::InvalidReleaseReplacement(
                "a replacement with a retained read-write volume requires exclusive_retained_volume_cutover=true"
                    .to_string(),
            ));
        }
        if self.exclusive_retained_volume_cutover && self.new_spec.retained_volume.is_none() {
            return Err(RuntimeError::InvalidReleaseReplacement(
                "exclusive_retained_volume_cutover is only valid for a replacement with a retained volume"
                    .to_string(),
            ));
        }
        self.health_gate.validate()?;
        let mut resource_names = BTreeMap::new();
        for resource in &self.resource_claims {
            resource.validate()?;
            if resource.deployment_id != self.new_spec.deployment_id
                || resource.service_id != self.new_spec.service_id
                || resource_names
                    .insert(resource.resource_name.as_str(), ())
                    .is_some()
            {
                return Err(RuntimeError::InvalidReleaseReplacement(
                    "replacement resource claims must be unique and match new_spec deployment/service"
                        .to_string(),
                ));
            }
        }
        let mut migration_versions = BTreeMap::new();
        for migration in &self.migrations {
            if migration.service_name != self.new_spec.service_id
                || migration.version.trim().is_empty()
                || migration_versions
                    .insert(migration.version.as_str(), ())
                    .is_some()
            {
                return Err(RuntimeError::InvalidReleaseReplacement(
                    "migration service_name must match new_spec.service_id and versions must be unique"
                        .to_string(),
                ));
            }
            for resource_name in &migration.resource_claims {
                if !resource_names.contains_key(resource_name.as_str()) {
                    return Err(RuntimeError::InvalidReleaseReplacement(format!(
                        "migration {} references unresolved replacement resource {resource_name}",
                        migration.version
                    )));
                }
            }
        }
        if let Some(saga) = &self.provider_saga {
            if saga.previous.revision_id.trim().is_empty()
                || saga.desired.revision_id.trim().is_empty()
                || saga.previous.revision_id == saga.desired.revision_id
            {
                return Err(RuntimeError::InvalidReleaseReplacement(
                    "provider saga requires distinct non-empty previous and desired revision ids"
                        .to_string(),
                ));
            }
            let service_id = self.new_spec.service_id.as_str();
            for revision in [&saga.previous, &saga.desired] {
                if let Some(auth) = &revision.auth
                    && auth.service_name != service_id
                {
                    return Err(RuntimeError::InvalidReleaseReplacement(
                        "provider auth state must match new_spec.service_id".to_string(),
                    ));
                }
                if let Some(gateway) = &revision.gateway
                    && gateway.service_name != service_id
                {
                    return Err(RuntimeError::InvalidReleaseReplacement(
                        "provider Gateway state must match new_spec.service_id".to_string(),
                    ));
                }
                let mut provider_names = BTreeMap::new();
                for provisioner in &revision.provisioners {
                    if provisioner.service_name() != service_id {
                        return Err(RuntimeError::InvalidReleaseReplacement(
                            "provider state must match new_spec.service_id".to_string(),
                        ));
                    }
                    if provider_names
                        .insert(provisioner.provider_name(), ())
                        .is_some()
                    {
                        return Err(RuntimeError::InvalidReleaseReplacement(format!(
                            "provider revision contains duplicate {} state",
                            provisioner.provider_name()
                        )));
                    }
                }
            }
        }
        Ok(())
    }
}

const fn default_true() -> bool {
    true
}
